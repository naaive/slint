// SPDX-License-Identifier: MIT

//! Output volume through PipeWire's `wpctl`, or PulseAudio's `pactl` (also served by `pipewire-pulse`).

use std::collections::VecDeque;
use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, sleep_until, timeout};

use crate::Audio;
use crate::hub::{Update, Updates};

pub(crate) const MAX_VOLUME: f32 = 1.5;
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);
/// Polling interval when nothing reports changes.
const POLL: Duration = Duration::from_secs(2);
/// Safety net while `pactl subscribe` reports changes.
const SUBSCRIBED_POLL: Duration = Duration::from_secs(30);
const RESUBSCRIBE: Duration = Duration::from_secs(10);
const SETTLE: Duration = Duration::from_millis(30);

#[derive(Debug)]
pub(crate) enum AudioCommand {
    SetVolume(f32),
    ToggleMute,
}

/// Parses `wpctl get-volume`, such as `Volume: 0.40 [MUTED]`.
pub(crate) fn parse_wpctl(output: &str) -> Option<Audio> {
    let line = output.lines().find_map(|line| line.trim().strip_prefix("Volume:"))?;
    let volume: f32 = line.split_whitespace().next()?.parse().ok()?;
    if !volume.is_finite() {
        return None;
    }
    Some(Audio { volume: volume.clamp(0.0, MAX_VOLUME), muted: line.contains("[MUTED]") })
}

/// Parses `pactl get-sink-volume` and returns the average of all channels.
pub(crate) fn parse_pactl_volume(output: &str) -> Option<f32> {
    let line = output.lines().find_map(|line| line.trim().strip_prefix("Volume:"))?;
    let percentages: Vec<f32> = line
        .split(|c: char| c == '/' || c.is_whitespace())
        .filter_map(|token| token.strip_suffix('%')?.parse::<f32>().ok())
        .collect();
    if percentages.is_empty() {
        return None;
    }
    let average = percentages.iter().sum::<f32>() / percentages.len() as f32 / 100.0;
    Some(average.clamp(0.0, MAX_VOLUME))
}

/// Parses `pactl get-sink-mute`, such as `Mute: no`.
pub(crate) fn parse_pactl_mute(output: &str) -> Option<bool> {
    let value = output.lines().find_map(|line| line.trim().strip_prefix("Mute:"))?;
    match value.trim() {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

/// Whether a `pactl subscribe` line can change the default sink's volume.
pub(crate) fn is_sink_event(line: &str) -> bool {
    line.starts_with("Event ") && (line.contains(" on sink #") || line.contains(" on server"))
}

#[derive(Debug, PartialEq, Eq)]
enum ToolError {
    Missing,
    Failed,
}

async fn run_tool(program: &str, args: &[&str]) -> Result<String, ToolError> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output();
    match timeout(TOOL_TIMEOUT, output).await {
        Ok(Ok(output)) if output.status.success() => {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        Ok(Err(err)) if err.kind() == io::ErrorKind::NotFound => Err(ToolError::Missing),
        _ => Err(ToolError::Failed),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tool {
    Wpctl,
    Pactl,
}

struct Subscription {
    _child: Child,
    lines: Lines<BufReader<ChildStdout>>,
}

impl Subscription {
    fn spawn() -> io::Result<Self> {
        let mut child = Command::new("pactl")
            .arg("subscribe")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        Ok(Self { _child: child, lines: BufReader::new(stdout).lines() })
    }
}

/// Reads the next line; pends forever without a subscription.
async fn next_line(subscription: &mut Option<Subscription>) -> Option<String> {
    match subscription {
        Some(subscription) => subscription.lines.next_line().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

struct AudioService {
    updates: Updates,
    wpctl_missing: bool,
    pactl_missing: bool,
    backend: Option<Tool>,
    current: Option<Audio>,
    published: bool,
}

impl AudioService {
    /// The tools to try, starting with the one that worked last time.
    fn tools(&self) -> Vec<Tool> {
        let mut tools = Vec::with_capacity(2);
        if let Some(tool) = self.backend {
            tools.push(tool);
        }
        for tool in [Tool::Wpctl, Tool::Pactl] {
            if !tools.contains(&tool) && !self.is_missing(tool) {
                tools.push(tool);
            }
        }
        tools
    }

    fn is_missing(&self, tool: Tool) -> bool {
        match tool {
            Tool::Wpctl => self.wpctl_missing,
            Tool::Pactl => self.pactl_missing,
        }
    }

    fn mark(&mut self, tool: Tool, error: &ToolError) {
        if *error == ToolError::Missing {
            match tool {
                Tool::Wpctl => self.wpctl_missing = true,
                Tool::Pactl => self.pactl_missing = true,
            }
        }
        if self.backend == Some(tool) {
            self.backend = None;
        }
    }

    fn all_missing(&self) -> bool {
        self.wpctl_missing && self.pactl_missing
    }

    async fn read(tool: Tool) -> Result<Option<Audio>, ToolError> {
        match tool {
            Tool::Wpctl => {
                Ok(parse_wpctl(&run_tool("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]).await?))
            }
            Tool::Pactl => {
                let volume = run_tool("pactl", &["get-sink-volume", "@DEFAULT_SINK@"]).await?;
                let mute = run_tool("pactl", &["get-sink-mute", "@DEFAULT_SINK@"]).await?;
                Ok(parse_pactl_volume(&volume).map(|volume| Audio {
                    volume,
                    muted: parse_pactl_mute(&mute).unwrap_or(false),
                }))
            }
        }
    }

    async fn refresh(&mut self) {
        let mut audio = None;
        for tool in self.tools() {
            match Self::read(tool).await {
                Ok(Some(found)) => {
                    if self.backend != Some(tool) {
                        tracing::info!("Controlling audio through {tool:?}");
                    }
                    self.backend = Some(tool);
                    audio = Some(found);
                    break;
                }
                Ok(None) => {
                    tracing::debug!("Unexpected output from {tool:?}");
                    self.mark(tool, &ToolError::Failed);
                }
                Err(err) => self.mark(tool, &err),
            }
        }
        self.publish(audio);
    }

    fn publish(&mut self, audio: Option<Audio>) {
        if !self.published || audio != self.current {
            self.published = true;
            self.current.clone_from(&audio);
            self.updates.send(Update::Audio(audio));
        }
    }

    async fn set_volume(&mut self, volume: f32) {
        let volume = if volume.is_finite() { volume.clamp(0.0, MAX_VOLUME) } else { return };
        let result = match self.backend {
            Some(Tool::Wpctl) => {
                let value = format!("{volume:.3}");
                run_tool("wpctl", &["set-volume", "-l", "1.5", "@DEFAULT_AUDIO_SINK@", &value])
                    .await
            }
            Some(Tool::Pactl) => {
                let value = format!("{}%", (volume * 100.0).round());
                run_tool("pactl", &["set-sink-volume", "@DEFAULT_SINK@", &value]).await
            }
            None => {
                tracing::debug!("No audio backend; ignoring volume change");
                return;
            }
        };
        match result {
            Ok(_) => {
                let muted = self.current.as_ref().is_some_and(|audio| audio.muted);
                self.publish(Some(Audio { volume, muted }));
            }
            Err(err) => tracing::debug!("Setting the volume failed: {err:?}"),
        }
    }

    async fn toggle_mute(&mut self) {
        let result = match self.backend {
            Some(Tool::Wpctl) => {
                run_tool("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]).await
            }
            Some(Tool::Pactl) => {
                run_tool("pactl", &["set-sink-mute", "@DEFAULT_SINK@", "toggle"]).await
            }
            None => {
                tracing::debug!("No audio backend; ignoring mute toggle");
                return;
            }
        };
        match result {
            Ok(_) => {
                if let Some(mut audio) = self.current.clone() {
                    audio.muted = !audio.muted;
                    self.publish(Some(audio));
                }
            }
            Err(err) => tracing::debug!("Toggling mute failed: {err:?}"),
        }
    }

    /// Applies `first` and every command already queued, collapsing consecutive volume changes from slider drags.
    async fn handle(
        &mut self,
        first: AudioCommand,
        commands: &mut UnboundedReceiver<AudioCommand>,
    ) {
        let mut queue = VecDeque::from([first]);
        while let Ok(command) = commands.try_recv() {
            queue.push_back(command);
        }
        while let Some(command) = queue.pop_front() {
            match command {
                AudioCommand::SetVolume(mut volume) => {
                    while let Some(AudioCommand::SetVolume(next)) = queue.front() {
                        volume = *next;
                        queue.pop_front();
                    }
                    self.set_volume(volume).await;
                }
                AudioCommand::ToggleMute => self.toggle_mute().await,
            }
        }
    }
}

pub(crate) async fn run(updates: Updates, mut commands: UnboundedReceiver<AudioCommand>) {
    let mut service = AudioService {
        updates,
        wpctl_missing: false,
        pactl_missing: false,
        backend: None,
        current: None,
        published: false,
    };
    let mut subscription: Option<Subscription> = None;
    let mut next_subscribe = Instant::now();
    service.refresh().await;
    loop {
        if service.all_missing() {
            tracing::info!("Neither wpctl nor pactl is installed; audio controls disabled");
            service.publish(None);
            while commands.recv().await.is_some() {
                tracing::debug!("No audio backend; ignoring command");
            }
            return;
        }
        if subscription.is_none() && !service.pactl_missing && Instant::now() >= next_subscribe {
            next_subscribe = Instant::now() + RESUBSCRIBE;
            match Subscription::spawn() {
                Ok(spawned) => subscription = Some(spawned),
                Err(err) if err.kind() == io::ErrorKind::NotFound => service.pactl_missing = true,
                Err(err) => tracing::debug!("Can't run pactl subscribe: {err}"),
            }
        }
        let poll = Instant::now() + if subscription.is_some() { SUBSCRIBED_POLL } else { POLL };
        tokio::select! {
            command = commands.recv() => match command {
                Some(command) => service.handle(command, &mut commands).await,
                None => return,
            },
            line = next_line(&mut subscription) => match line {
                Some(line) if is_sink_event(&line) => {
                    let deadline = Instant::now() + SETTLE;
                    while let Ok(Some(_)) = tokio::time::timeout_at(deadline, next_line(&mut subscription)).await {}
                    service.refresh().await;
                }
                Some(_) => {}
                None => {
                    tracing::debug!("pactl subscribe exited; polling instead");
                    subscription = None;
                }
            },
            () = sleep_until(poll) => service.refresh().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wpctl_output() {
        assert_eq!(parse_wpctl("Volume: 0.40\n"), Some(Audio { volume: 0.4, muted: false }));
        assert_eq!(
            parse_wpctl("Volume: 0.55 [MUTED]\n"),
            Some(Audio { volume: 0.55, muted: true })
        );
        assert_eq!(parse_wpctl("Volume: 2.00"), Some(Audio { volume: 1.5, muted: false }));
        assert_eq!(parse_wpctl("Volume: NaN"), None);
        assert_eq!(parse_wpctl("Translate ID error\n"), None);
        assert_eq!(parse_wpctl(""), None);
    }

    #[test]
    fn pactl_volume_output() {
        let stereo = "Volume: front-left: 26214 /  40% / -23.88 dB,   front-right: 32768 /  50% / -18.06 dB\n        balance 0.10\n";
        assert_eq!(parse_pactl_volume(stereo), Some(0.45));
        let mono = "Volume: mono: 65536 / 100% / 0.00 dB\n";
        assert_eq!(parse_pactl_volume(mono), Some(1.0));
        let loud = "Volume: mono: 131072 / 200% / 18.06 dB\n";
        assert_eq!(parse_pactl_volume(loud), Some(1.5));
        assert_eq!(parse_pactl_volume("Connection failure: Connection refused\n"), None);
    }

    #[test]
    fn pactl_mute_output() {
        assert_eq!(parse_pactl_mute("Mute: yes\n"), Some(true));
        assert_eq!(parse_pactl_mute("Mute: no\n"), Some(false));
        assert_eq!(parse_pactl_mute("Stummschalten: ja\n"), None);
    }

    #[test]
    fn subscribe_events() {
        assert!(is_sink_event("Event 'change' on sink #56"));
        assert!(is_sink_event("Event 'change' on server #-1"));
        assert!(!is_sink_event("Event 'change' on sink-input #102"));
        assert!(!is_sink_event("Event 'new' on client #7"));
        assert!(!is_sink_event("garbage"));
    }
}
