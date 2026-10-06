// SPDX-License-Identifier: MIT

//! Running `pactl`: reading devices, following `pactl subscribe`, and the commands.

use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command as Process};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, sleep_until, timeout, timeout_at};

use super::parse::{self, VOLUME_NORM};
use super::{Command, Direction, Event, SoundState};
use crate::audio::MAX_VOLUME;

const TOOL_TIMEOUT: Duration = Duration::from_secs(5);
/// How often devices are read while nothing reports changes.
const POLL: Duration = Duration::from_secs(2);
/// A safety net while `pactl subscribe` reports changes.
const SUBSCRIBED_POLL: Duration = Duration::from_secs(30);
/// How often a missing sound server is looked for.
const UNAVAILABLE_POLL: Duration = Duration::from_secs(5);
/// A burst of `pactl subscribe` events is coalesced until it has been quiet this long.
const SETTLE: Duration = Duration::from_millis(50);

#[derive(Debug)]
enum Failure {
    /// The program isn't installed.
    Missing,
    /// It ran and failed, with its message.
    Failed(String),
}

struct Pactl {
    program: PathBuf,
}

impl Pactl {
    async fn run(&self, args: &[&str]) -> Result<String, Failure> {
        let output = Process::new(&self.program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output();
        match timeout(TOOL_TIMEOUT, output).await {
            Ok(Ok(output)) if output.status.success() => {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            }
            Ok(Ok(output)) => {
                let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
                Err(Failure::Failed(if message.is_empty() {
                    format!("pactl exited with {}", output.status)
                } else {
                    message
                }))
            }
            Ok(Err(err)) if err.kind() == io::ErrorKind::NotFound => Err(Failure::Missing),
            Ok(Err(err)) => Err(Failure::Failed(err.to_string())),
            Err(_) => Err(Failure::Failed("pactl didn't answer in time".into())),
        }
    }

    async fn read(&self) -> Result<SoundState, Failure> {
        let invalid = |err: serde_json::Error| Failure::Failed(format!("unexpected output: {err}"));
        let info = self.run(&["--format=json", "info"]).await?;
        let (sink, source) = parse::defaults(&info).map_err(invalid)?;
        let sinks = self.run(&["--format=json", "list", "sinks"]).await?;
        let sources = self.run(&["--format=json", "list", "sources"]).await?;
        Ok(SoundState {
            available: true,
            outputs: parse::devices(&sinks, sink.as_deref()).map_err(invalid)?,
            inputs: parse::devices(&sources, source.as_deref()).map_err(invalid)?,
        })
    }

    fn subscribe(&self) -> io::Result<Subscription> {
        let mut child = Process::new(&self.program)
            .arg("subscribe")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        Ok(Subscription { _child: child, lines: BufReader::new(stdout).lines() })
    }
}

struct Subscription {
    _child: Child,
    lines: Lines<BufReader<ChildStdout>>,
}

/// Reads the next line; pends forever without a subscription.
async fn next_line(subscription: &mut Option<Subscription>) -> Option<String> {
    match subscription {
        Some(subscription) => subscription.lines.next_line().await.ok().flatten(),
        None => std::future::pending().await,
    }
}

/// The `pactl` arguments of a command that changes something.
fn arguments(command: &Command) -> Option<Vec<String>> {
    let kind = |direction: &Direction| match direction {
        Direction::Output => "sink",
        Direction::Input => "source",
    };
    Some(match command {
        Command::SetDefault { direction, name } => {
            vec![format!("set-default-{}", kind(direction)), name.clone()]
        }
        Command::SetVolume { direction, name, volume } => {
            let raw = (volume.clamp(0.0, MAX_VOLUME) * VOLUME_NORM).round() as u32;
            vec![format!("set-{}-volume", kind(direction)), name.clone(), raw.to_string()]
        }
        Command::SetMute { direction, name, muted } => {
            let value = if *muted { "1" } else { "0" };
            vec![format!("set-{}-mute", kind(direction)), name.clone(), value.into()]
        }
        Command::Refresh => return None,
    })
}

struct SoundService {
    pactl: Pactl,
    on_event: Box<dyn Fn(Event) + Send + Sync>,
    last: Option<SoundState>,
    missing: bool,
}

impl SoundService {
    fn publish(&mut self, state: SoundState) {
        if self.last.as_ref() != Some(&state) {
            self.last = Some(state.clone());
            (self.on_event)(Event::State(state));
        }
    }

    fn available(&self) -> bool {
        self.last.as_ref().is_some_and(|state| state.available)
    }

    async fn refresh(&mut self) {
        match self.pactl.read().await {
            Ok(state) => self.publish(state),
            Err(failure) => {
                if self.available() || self.last.is_none() {
                    tracing::info!("No sound server through pactl: {failure:?}");
                }
                self.missing = matches!(failure, Failure::Missing);
                self.publish(SoundState::default());
            }
        }
    }

    /// Runs `first` and every command already queued, keeping only the last of consecutive volume changes
    /// to one device, as a slider drag sends them.
    async fn handle(&mut self, first: Command, commands: &mut UnboundedReceiver<Command>) {
        let mut queue = VecDeque::from([first]);
        while let Ok(command) = commands.try_recv() {
            queue.push_back(command);
        }
        while let Some(mut command) = queue.pop_front() {
            while let (
                Command::SetVolume { direction, name, .. },
                Some(Command::SetVolume { direction: next_direction, name: next_name, .. }),
            ) = (&command, queue.front())
                && direction == next_direction
                && name == next_name
            {
                command = queue.pop_front().unwrap_or(command);
            }
            let Some(args) = arguments(&command) else { continue };
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            if let Err(failure) = self.pactl.run(&args).await {
                tracing::info!("pactl refused {command:?}: {failure:?}");
                let reason = match failure {
                    Failure::Missing => "pactl isn't installed".to_owned(),
                    Failure::Failed(message) => message,
                };
                (self.on_event)(Event::Failed(reason));
            }
        }
        self.refresh().await;
    }
}

pub(super) async fn run(
    program: PathBuf,
    on_event: Box<dyn Fn(Event) + Send + Sync>,
    mut commands: UnboundedReceiver<Command>,
) {
    let mut service =
        SoundService { pactl: Pactl { program }, on_event, last: None, missing: false };
    let mut subscription: Option<Subscription> = None;
    service.refresh().await;
    loop {
        if service.missing {
            tracing::info!("pactl isn't installed; sound settings are unavailable");
            while commands.recv().await.is_some() {
                (service.on_event)(Event::Failed("pactl isn't installed".into()));
            }
            return;
        }
        if subscription.is_none() && service.available() {
            subscription = service
                .pactl
                .subscribe()
                .inspect_err(|err| tracing::debug!("Can't run pactl subscribe: {err}"))
                .ok();
        }
        let poll = match (&subscription, service.available()) {
            (Some(_), _) => SUBSCRIBED_POLL,
            (None, true) => POLL,
            (None, false) => UNAVAILABLE_POLL,
        };
        let next_poll = Instant::now() + poll;
        loop {
            tokio::select! {
                command = commands.recv() => match command {
                    Some(command) => {
                        service.handle(command, &mut commands).await;
                        break;
                    }
                    None => return,
                },
                line = next_line(&mut subscription) => match line {
                    Some(line) if parse::is_device_event(&line) => {
                        let deadline = Instant::now() + SETTLE;
                        while let Ok(Some(_)) = timeout_at(deadline, next_line(&mut subscription)).await {}
                        service.refresh().await;
                        break;
                    }
                    Some(_) => {}
                    None => {
                        tracing::debug!("pactl subscribe exited; polling instead");
                        subscription = None;
                        service.refresh().await;
                        break;
                    }
                },
                () = sleep_until(next_poll) => {
                    service.refresh().await;
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_arguments() {
        let name = || "alsa_output.analog-stereo".to_owned();
        let output = Direction::Output;
        assert_eq!(
            arguments(&Command::SetDefault { direction: Direction::Input, name: name() }),
            Some(vec!["set-default-source".into(), name()])
        );
        assert_eq!(
            arguments(&Command::SetVolume { direction: output, name: name(), volume: 0.5 }),
            Some(vec!["set-sink-volume".into(), name(), "32768".into()])
        );
        assert_eq!(
            arguments(&Command::SetVolume { direction: output, name: name(), volume: 9.0 }),
            Some(vec!["set-sink-volume".into(), name(), "98304".into()]),
            "clamped to 150 %"
        );
        assert_eq!(
            arguments(&Command::SetMute { direction: output, name: name(), muted: true }),
            Some(vec!["set-sink-mute".into(), name(), "1".into()])
        );
        assert_eq!(arguments(&Command::Refresh), None);
    }
}
