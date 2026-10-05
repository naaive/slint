// SPDX-License-Identifier: MIT

//! Media players over MPRIS on the session bus.

use std::collections::HashMap;

use futures_util::StreamExt;
use tokio::sync::mpsc::UnboundedReceiver;
use zbus::names::OwnedUniqueName;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, Message};

use crate::Media;
use crate::bus::{self, Props, Signals};
use crate::hub::{Update, Updates};

const PREFIX: &str = "org.mpris.MediaPlayer2.";
const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";

#[derive(Debug)]
pub(crate) enum MediaCommand {
    PlayPause,
    Next,
    Previous,
}

impl MediaCommand {
    fn method(&self) -> &'static str {
        match self {
            Self::PlayPause => "PlayPause",
            Self::Next => "Next",
            Self::Previous => "Previous",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Metadata {
    pub(crate) title: String,
    pub(crate) artist: String,
    pub(crate) art_url: Option<String>,
}

fn value_str<'a>(value: &'a Value<'_>) -> Option<&'a str> {
    match value {
        Value::Str(s) => Some(s.as_str()),
        Value::ObjectPath(path) => Some(path.as_str()),
        Value::Value(inner) => value_str(inner),
        _ => None,
    }
}

/// Reads `xesam:artist`, a list of strings per the specification, though some players send one string.
fn value_strings(value: &Value<'_>) -> Vec<String> {
    match value {
        Value::Array(array) => {
            array.inner().iter().filter_map(value_str).map(str::to_owned).collect()
        }
        Value::Value(inner) => value_strings(inner),
        other => value_str(other).map(str::to_owned).into_iter().collect(),
    }
}

pub(crate) fn parse_metadata(metadata: &HashMap<String, OwnedValue>) -> Metadata {
    let string =
        |key: &str| metadata.get(key).and_then(|value| value_str(value)).map(str::to_owned);
    Metadata {
        title: string("xesam:title").unwrap_or_default(),
        artist: metadata
            .get("xesam:artist")
            .map(|value| value_strings(value).join(", "))
            .unwrap_or_default(),
        art_url: string("mpris:artUrl").filter(|url| !url.is_empty()),
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Player {
    pub(crate) name: String,
    pub(crate) owner: String,
    pub(crate) playing: bool,
    pub(crate) metadata: Metadata,
    /// When the player last started playing, on the tracker's activity clock.
    pub(crate) last_played: u64,
    /// Discovery order, so the newest of otherwise equal players wins.
    pub(crate) seen: u64,
}

/// Picks the player to show: the one that most recently started playing,
/// skipping players that neither play nor have a title.
pub(crate) fn select(players: &[Player]) -> Option<&Player> {
    players
        .iter()
        .filter(|player| player.playing || !player.metadata.title.is_empty())
        .max_by_key(|player| (player.playing, player.last_played, player.seen))
}

fn media(player: &Player) -> Media {
    Media {
        player: player.name.clone(),
        title: player.metadata.title.clone(),
        artist: player.metadata.artist.clone(),
        art_url: player.metadata.art_url.clone(),
        playing: player.playing,
    }
}

struct Tracker {
    conn: Connection,
    updates: Updates,
    players: Vec<Player>,
    clock: u64,
    published: Option<Option<Media>>,
}

impl Tracker {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn publish(&mut self) {
        let media = select(&self.players).map(media);
        if self.published.as_ref() != Some(&media) {
            self.published = Some(media.clone());
            self.updates.send(Update::Media(media));
        }
    }

    fn apply(&mut self, index: usize, mut props: Props) {
        let status = props.take::<String>("PlaybackStatus");
        let metadata = props.take::<HashMap<String, OwnedValue>>("Metadata");
        let now = self.tick();
        let player = &mut self.players[index];
        if let Some(status) = status {
            let playing = status == "Playing";
            if playing && !player.playing {
                player.last_played = now;
            }
            player.playing = playing;
        }
        if let Some(metadata) = metadata {
            player.metadata = parse_metadata(&metadata);
        }
    }

    async fn add(&mut self, name: String, owner: String) {
        self.players.retain(|player| player.name != name);
        let seen = self.tick();
        self.players.push(Player {
            name,
            owner,
            playing: false,
            metadata: Metadata::default(),
            last_played: 0,
            seen,
        });
        self.refetch(self.players.len() - 1).await;
    }

    async fn refetch(&mut self, index: usize) {
        let owner = self.players[index].owner.clone();
        match bus::get_all(&self.conn, &owner, PATH, PLAYER_INTERFACE).await {
            Ok(props) => self.apply(index, props),
            Err(err) => tracing::debug!("Can't read player {}: {err}", self.players[index].name),
        }
    }

    async fn on_owner_changed(&mut self, message: &Message) {
        let Ok((name, _, new_owner)) = message.body().deserialize::<(String, String, String)>()
        else {
            return;
        };
        if !name.starts_with(PREFIX) {
            return;
        }
        if new_owner.is_empty() {
            self.players.retain(|player| player.name != name);
        } else {
            self.add(name, new_owner).await;
        }
    }

    async fn on_properties_changed(&mut self, message: &Message) {
        let header = message.header();
        let Some(sender) = header.sender() else { return };
        let Some(index) = self.players.iter().position(|player| player.owner == sender.as_str())
        else {
            return;
        };
        let Ok((_, changed, invalidated)) =
            message.body().deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            return;
        };
        self.apply(index, Props(changed));
        if invalidated.iter().any(|name| name == "PlaybackStatus" || name == "Metadata") {
            self.refetch(index).await;
        }
    }

    async fn command(&self, command: &MediaCommand) {
        let Some(player) = select(&self.players) else {
            tracing::debug!("No media player for {command:?}");
            return;
        };
        if let Err(err) =
            bus::call(&self.conn, &player.owner, PATH, PLAYER_INTERFACE, command.method(), &())
                .await
        {
            tracing::debug!("{} rejected {command:?}: {err}", player.name);
        }
    }

    async fn run(&mut self, commands: &mut UnboundedReceiver<MediaCommand>) -> zbus::Result<()> {
        let owner_rule = bus::signal_rule()
            .sender("org.freedesktop.DBus")?
            .interface("org.freedesktop.DBus")?
            .member("NameOwnerChanged")?
            .arg0ns("org.mpris.MediaPlayer2")?
            .build();
        let properties_rule = bus::signal_rule()
            .path(PATH)?
            .interface(bus::PROPERTIES)?
            .member("PropertiesChanged")?
            .arg(0, PLAYER_INTERFACE)?
            .build();
        let mut owners: Signals = bus::signals(&self.conn, [owner_rule]).await?;
        let mut properties: Signals = bus::signals(&self.conn, [properties_rule]).await?;

        let names: Vec<String> = bus::call_for(
            &self.conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "ListNames",
            &(),
        )
        .await?;
        for name in names.into_iter().filter(|name| name.starts_with(PREFIX)) {
            let owner: zbus::Result<OwnedUniqueName> = bus::call_for(
                &self.conn,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "GetNameOwner",
                &(name.as_str(),),
            )
            .await;
            if let Ok(owner) = owner {
                self.add(name, owner.to_string()).await;
            }
        }
        loop {
            self.publish();
            tokio::select! {
                message = owners.next() => match message {
                    Some(message) => self.on_owner_changed(&message).await,
                    None => return Err(bus::stream_ended()),
                },
                message = properties.next() => match message {
                    Some(message) => self.on_properties_changed(&message).await,
                    None => return Err(bus::stream_ended()),
                },
                command = commands.recv() => match command {
                    Some(command) => self.command(&command).await,
                    None => return Ok(()),
                },
            }
        }
    }
}

pub(crate) async fn run(
    conn: Connection,
    updates: Updates,
    mut commands: UnboundedReceiver<MediaCommand>,
) {
    let mut tracker = Tracker { conn, updates, players: Vec::new(), clock: 0, published: None };
    if let Err(err) = tracker.run(&mut commands).await {
        tracing::info!("MPRIS tracking stopped: {err}");
        tracker.updates.send(Update::Media(None));
        while commands.recv().await.is_some() {
            tracing::debug!("MPRIS tracking stopped; ignoring media command");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Array;

    fn player(name: &str, playing: bool, title: &str, last_played: u64, seen: u64) -> Player {
        Player {
            name: name.into(),
            owner: format!(":1.{seen}"),
            playing,
            metadata: Metadata { title: title.into(), ..Default::default() },
            last_played,
            seen,
        }
    }

    #[test]
    fn selection_prefers_most_recent_playing() {
        let players = [
            player("a", true, "A", 5, 1),
            player("b", true, "B", 9, 2),
            player("c", false, "C", 12, 3),
        ];
        assert_eq!(select(&players).unwrap().name, "b");
    }

    #[test]
    fn selection_falls_back_to_last_played() {
        let players = [
            player("a", false, "A", 5, 1),
            player("b", false, "B", 0, 4),
            player("c", false, "C", 7, 3),
            player("d", false, "", 9, 5),
        ];
        assert_eq!(select(&players).unwrap().name, "c");
        assert!(select(&[player("idle", false, "", 0, 1)]).is_none());
        assert!(select(&[]).is_none());
    }

    #[test]
    fn metadata_parsing() {
        let artists = Array::from(vec!["Daft Punk", "Pharrell"]);
        let metadata = Props::from_pairs([
            ("xesam:title", Value::from("Get Lucky")),
            ("xesam:artist", Value::from(artists)),
            ("mpris:artUrl", Value::from("file:///tmp/cover.jpg")),
            ("mpris:length", Value::I64(1)),
        ])
        .0;
        assert_eq!(
            parse_metadata(&metadata),
            Metadata {
                title: "Get Lucky".into(),
                artist: "Daft Punk, Pharrell".into(),
                art_url: Some("file:///tmp/cover.jpg".into()),
            }
        );

        let metadata = Props::from_pairs([
            ("xesam:artist", Value::from("Solo")),
            ("mpris:artUrl", Value::from("")),
        ])
        .0;
        assert_eq!(
            parse_metadata(&metadata),
            Metadata { title: String::new(), artist: "Solo".into(), art_url: None }
        );
        assert_eq!(parse_metadata(&HashMap::new()), Metadata::default());
    }
}
