// SPDX-License-Identifier: MIT

//! The sound client against a shell script standing in for `pactl`, which keeps its devices in files.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nimbus_services::sound::{Client, Command, Device, Direction, Event, SoundState};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

/// Answers `info`, `list sinks`, `list sources`, the `set-…` commands, and `subscribe`, like `pactl --format=json`.
/// Each device is a file `<sink|source>/<name>` holding `description|raw volume|mute`.
const FAKE_PACTL: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo "$*" >> "$dir/log"
fail() { echo "Failure: No such entity" >&2; exit 1; }
list() {
    printf '['
    sep=
    for file in "$dir/$1"/*; do
        [ -e "$file" ] || continue
        IFS='|' read -r description volume mute < "$file"
        printf '%s{"name":"%s","description":"%s","mute":%s,"volume":{"front-left":{"value":%s},"front-right":{"value":%s}}}' \
            "$sep" "$(basename "$file")" "$description" "$mute" "$volume" "$volume"
        sep=,
    done
    printf ']\n'
}
change() {
    file="$dir/$1/$2"
    [ -e "$file" ] || fail
    IFS='|' read -r description volume mute < "$file"
    case $3 in
        volume) volume=$4 ;;
        mute) if [ "$4" = 1 ]; then mute=true; else mute=false; fi ;;
    esac
    echo "$description|$volume|$mute" > "$file"
    echo "Event 'change' on $1 #1" >> "$dir/events"
}
default() {
    [ -e "$dir/$1/$2" ] || fail
    echo "$2" > "$dir/default-$1"
    echo "Event 'change' on server #-1" >> "$dir/events"
}
[ -e "$dir/down" ] && { echo "Connection failure: Connection refused" >&2; exit 1; }
case "$*" in
    "--format=json info") printf '{"default_sink_name":"%s","default_source_name":"%s"}\n' "$(cat "$dir/default-sink")" "$(cat "$dir/default-source")" ;;
    "--format=json list sinks") list sink ;;
    "--format=json list sources") list source ;;
    subscribe) exec tail -n 0 -f "$dir/events" ;;
    set-default-sink\ *) default sink "$2" ;;
    set-default-source\ *) default source "$2" ;;
    set-sink-volume\ *) change sink "$2" volume "$3" ;;
    set-source-volume\ *) change source "$2" volume "$3" ;;
    set-sink-mute\ *) change sink "$2" mute "$3" ;;
    set-source-mute\ *) change source "$2" mute "$3" ;;
    *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("pactl");
        std::fs::write(&script, FAKE_PACTL).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.path().join("events"), "").unwrap();
        let fake = Self { dir };
        fake.device("sink", "alsa_output.speakers", "Speakers|32768|false");
        fake.device("sink", "bluez_output.headphones", "WH-1000XM5|65536|false");
        fake.device("source", "alsa_input.mic", "Built-in Microphone|65536|false");
        fake.write("default-sink", "alsa_output.speakers");
        fake.write("default-source", "alsa_input.mic");
        fake
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn program(&self) -> PathBuf {
        self.path().join("pactl")
    }

    fn write(&self, name: &str, contents: &str) {
        std::fs::write(self.path().join(name), format!("{contents}\n")).unwrap();
    }

    fn device(&self, kind: &str, name: &str, line: &str) {
        std::fs::create_dir_all(self.path().join(kind)).unwrap();
        self.write(&format!("{kind}/{name}"), line);
    }

    fn calls(&self, prefix: &str) -> Vec<String> {
        let log = std::fs::read_to_string(self.path().join("log")).unwrap_or_default();
        log.lines().filter(|line| line.starts_with(prefix)).map(str::to_owned).collect()
    }
}

fn start(program: PathBuf) -> (Client, UnboundedReceiver<Event>) {
    let (sender, events) = unbounded_channel();
    let client = Client::spawn_with(program, move |event| {
        let _ = sender.send(event);
    });
    (client, events)
}

async fn next(events: &mut UnboundedReceiver<Event>) -> Event {
    timeout(WAIT, events.recv()).await.expect("an event in time").expect("the client runs")
}

async fn state_where(
    events: &mut UnboundedReceiver<Event>,
    matches: impl Fn(&SoundState) -> bool,
) -> SoundState {
    loop {
        if let Event::State(state) = next(events).await
            && matches(&state)
        {
            return state;
        }
    }
}

fn device<'a>(state: &'a SoundState, name: &str) -> &'a Device {
    state.outputs.iter().chain(&state.inputs).find(|d| d.name == name).expect("the device")
}

#[tokio::test]
async fn lists_devices_and_changes_defaults_volume_and_mute() {
    let fake = Fake::new();
    let (client, mut events) = start(fake.program());

    let Event::State(state) = next(&mut events).await else { panic!("a state first") };
    assert!(state.available);
    let names: Vec<&str> = state.outputs.iter().map(|d| d.description.as_str()).collect();
    assert_eq!(names, ["Speakers", "WH-1000XM5"]);
    let speakers = device(&state, "alsa_output.speakers");
    assert!(speakers.default && !speakers.muted);
    assert_eq!(speakers.volume, 0.5);
    assert_eq!(state.inputs.len(), 1);

    let headphones = "bluez_output.headphones".to_owned();
    client.send(Command::SetDefault { direction: Direction::Output, name: headphones.clone() });
    state_where(&mut events, |s| device(s, &headphones).default).await;

    // A drag sends many volumes; the last one wins.
    for volume in [0.1, 0.2, 0.25] {
        let name = headphones.clone();
        client.send(Command::SetVolume { direction: Direction::Output, name, volume });
    }
    state_where(&mut events, |s| device(s, &headphones).volume == 0.25).await;
    let volumes = fake.calls("set-sink-volume");
    assert_eq!(
        volumes.last().map(String::as_str),
        Some("set-sink-volume bluez_output.headphones 16384")
    );
    assert!(volumes.len() <= 3, "{volumes:?}");

    let mic = "alsa_input.mic".to_owned();
    client.send(Command::SetMute { direction: Direction::Input, name: mic.clone(), muted: true });
    state_where(&mut events, |s| device(s, &mic).muted).await;

    // Changes made elsewhere arrive through `pactl subscribe`, long before the next poll.
    fake.device("sink", "alsa_output.speakers", "Speakers|65536|true");
    std::fs::write(fake.path().join("events"), "Event 'change' on sink #7\n").unwrap();
    state_where(&mut events, |s| device(s, "alsa_output.speakers").muted).await;

    client.send(Command::SetDefault { direction: Direction::Output, name: "gone".into() });
    loop {
        if let Event::Failed(reason) = next(&mut events).await {
            assert_eq!(reason, "Failure: No such entity");
            break;
        }
    }
}

#[tokio::test]
async fn reports_a_missing_server_or_pactl() {
    let fake = Fake::new();
    fake.write("down", "");
    let (client, mut events) = start(fake.program());
    assert_eq!(next(&mut events).await, Event::State(SoundState::default()));
    std::fs::remove_file(fake.path().join("down")).unwrap();
    client.send(Command::Refresh);
    state_where(&mut events, |s| s.available && s.outputs.len() == 2).await;

    let (client, mut events) = start(fake.path().join("missing"));
    assert_eq!(next(&mut events).await, Event::State(SoundState::default()));
    client.send(Command::Refresh);
    assert_eq!(next(&mut events).await, Event::Failed("pactl isn't installed".into()));
}
