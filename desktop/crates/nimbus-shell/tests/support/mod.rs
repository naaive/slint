// SPDX-License-Identifier: MIT

//! Realistic mock data for the shell's tests and `nimbus-shell-preview`, which includes this file.

#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, SystemTime};

use nimbus_config::{ColorScheme, Config};
use nimbus_ipc::{CompositorState, LayoutMode, OutputInfo, WindowInfo};
use nimbus_services::{
    Audio, Battery, Bluetooth, ConnectionKind, Media, Network, Notification, SystemState, Urgency,
};
use nimbus_xdg::{AppIndex, DesktopEntry, IconResolver};

pub const OUTPUT: &str = "eDP-1";

/// `(id, name, comment, top color, bottom color, glyph)` for the mock applications.
const APPS: [(&str, &str, &str, &str, &str, &str); 16] = [
    ("org.nimbus.Files", "Files", "Browse and organize files", "#5aa7ff", "#2a6fd6", FOLDER),
    ("org.nimbus.Terminal", "Terminal", "Use the command line", "#4a4a55", "#1e1e24", TERMINAL),
    ("firefox", "Web Browser", "Browse the web", "#ff9f43", "#e8590c", GLOBE),
    ("org.nimbus.TextEditor", "Text Editor", "Edit text files", "#69d2a6", "#20a070", DOCUMENT),
    ("org.nimbus.Settings", "Settings", "Change system settings", "#9aa5b8", "#5d6878", GEAR),
    ("org.nimbus.Music", "Music", "Play your music collection", "#ff6b8b", "#d6336c", NOTE),
    ("org.nimbus.Photos", "Photos", "View and organize photos", "#ffd43b", "#f08c00", MOUNTAIN),
    ("org.nimbus.Calendar", "Calendar", "Manage your schedule", "#ff8787", "#e03131", CALENDAR),
    ("org.nimbus.Mail", "Mail", "Read and write email", "#74c0fc", "#1c7ed6", ENVELOPE),
    ("org.nimbus.Calculator", "Calculator", "Perform calculations", "#868e96", "#495057", GRID),
    ("org.nimbus.Monitor", "System Monitor", "View running processes", "#63e6be", "#0ca678", PULSE),
    ("org.nimbus.Software", "Software", "Install and update apps", "#b197fc", "#7048e8", BAG),
    ("org.nimbus.Videos", "Videos", "Watch movies and shows", "#495057", "#212529", PLAY),
    ("org.nimbus.Maps", "Maps", "Find places and directions", "#8ce99a", "#2f9e44", PIN),
    ("org.nimbus.Weather", "Weather", "Check the forecast", "#66d9e8", "#1098ad", SUN),
    ("org.nimbus.Notes", "Notes", "Jot down ideas", "#ffe066", "#fab005", LINES),
];

const FOLDER: &str = r##"<path d="M14 22a4 4 0 0 1 4-4h9l4 4h15a4 4 0 0 1 4 4v16a4 4 0 0 1-4 4H18a4 4 0 0 1-4-4z" fill="#fff"/>"##;
const TERMINAL: &str = r##"<path d="M18 22l9 8-9 8" stroke="#69db7c" stroke-width="4" fill="none" stroke-linecap="round" stroke-linejoin="round"/><path d="M31 40h14" stroke="#fff" stroke-width="4" stroke-linecap="round"/>"##;
const GLOBE: &str = r##"<circle cx="32" cy="32" r="16" fill="none" stroke="#fff" stroke-width="3.5"/><ellipse cx="32" cy="32" rx="7" ry="16" fill="none" stroke="#fff" stroke-width="3"/><path d="M16 32h32" stroke="#fff" stroke-width="3"/>"##;
const DOCUMENT: &str = r##"<rect x="19" y="14" width="26" height="36" rx="4" fill="#fff"/><path d="M25 25h14M25 32h14M25 39h9" stroke="#20a070" stroke-width="3" stroke-linecap="round"/>"##;
const GEAR: &str = r##"<circle cx="32" cy="32" r="13" fill="none" stroke="#fff" stroke-width="7" stroke-dasharray="6 4.2"/><circle cx="32" cy="32" r="6" fill="#fff"/>"##;
const NOTE: &str = r##"<path d="M27 42V20l16-4v22" stroke="#fff" stroke-width="3.5" fill="none" stroke-linejoin="round"/><circle cx="23" cy="42" r="5" fill="#fff"/><circle cx="39" cy="38" r="5" fill="#fff"/>"##;
const MOUNTAIN: &str = r##"<path d="M14 46l12-16 8 10 6-7 10 13z" fill="#fff"/><circle cx="41" cy="22" r="5" fill="#fff"/>"##;
const CALENDAR: &str = r##"<rect x="16" y="18" width="32" height="30" rx="5" fill="#fff"/><rect x="16" y="18" width="32" height="9" rx="4" fill="#ffe3e3"/><text x="32" y="44" font-size="15" font-family="sans-serif" font-weight="bold" text-anchor="middle" fill="#e03131">5</text>"##;
const ENVELOPE: &str = r##"<rect x="14" y="20" width="36" height="25" rx="4" fill="#fff"/><path d="M15 22l17 13 17-13" stroke="#1c7ed6" stroke-width="3" fill="none" stroke-linejoin="round"/>"##;
const GRID: &str = r##"<rect x="18" y="16" width="28" height="9" rx="2" fill="#fff"/><g fill="#fff"><rect x="18" y="29" width="7" height="7" rx="2"/><rect x="28.5" y="29" width="7" height="7" rx="2"/><rect x="39" y="29" width="7" height="7" rx="2"/><rect x="18" y="39" width="7" height="7" rx="2"/><rect x="28.5" y="39" width="7" height="7" rx="2"/></g><rect x="39" y="39" width="7" height="7" rx="2" fill="#ff922b"/>"##;
const PULSE: &str = r##"<path d="M14 34h9l4-10 6 18 5-12 3 4h9" stroke="#fff" stroke-width="3.5" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"##;
const BAG: &str = r##"<path d="M18 26h28l-2 20H20z" fill="#fff"/><path d="M25 26v-3a7 7 0 0 1 14 0v3" stroke="#fff" stroke-width="3" fill="none"/>"##;
const PLAY: &str =
    r##"<circle cx="32" cy="32" r="16" fill="#fff"/><path d="M28 24l12 8-12 8z" fill="#212529"/>"##;
const PIN: &str = r##"<path d="M32 50s-12-12-12-21a12 12 0 0 1 24 0c0 9-12 21-12 21z" fill="#fff"/><circle cx="32" cy="29" r="5" fill="#2f9e44"/>"##;
const SUN: &str = r##"<circle cx="28" cy="28" r="9" fill="#ffe066"/><path d="M24 46a8 8 0 0 1 1-16 10 10 0 0 1 19 3 6.5 6.5 0 0 1 0 13z" fill="#fff"/>"##;
const LINES: &str = r##"<rect x="17" y="15" width="30" height="34" rx="4" fill="#fff"/><path d="M23 25h18M23 32h18M23 39h12" stroke="#fab005" stroke-width="3" stroke-linecap="round"/>"##;

fn icon_svg(top: &str, bottom: &str, glyph: &str) -> String {
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="64" viewBox="0 0 64 64">
<defs><linearGradient id="g" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{top}"/><stop offset="1" stop-color="{bottom}"/></linearGradient></defs>
<rect x="4" y="4" width="56" height="56" rx="14" fill="url(#g)"/>
<rect x="4.5" y="4.5" width="55" height="55" rx="13.5" fill="none" stroke="#ffffff" stroke-opacity="0.18"/>
{glyph}
</svg>"##
    )
}

/// Writes the mock applications' icons below `dir` and returns their index and an icon resolver for them.
pub fn apps(dir: &Path) -> std::io::Result<(AppIndex, IconResolver)> {
    let pixmaps = dir.join("pixmaps");
    std::fs::create_dir_all(&pixmaps)?;
    let mut entries = Vec::new();
    for (id, name, comment, top, bottom, glyph) in APPS {
        std::fs::write(pixmaps.join(format!("{id}.svg")), icon_svg(top, bottom, glyph))?;
        entries.push(DesktopEntry {
            id: id.into(),
            name: name.into(),
            comment: Some(comment.into()),
            icon: Some(id.into()),
            exec: id.into(),
            path: dir.join(format!("{id}.desktop")),
            ..Default::default()
        });
    }
    entries.sort_by_key(|e| e.name.to_lowercase());
    Ok((AppIndex { entries }, IconResolver::with_data_dirs("hicolor", &[dir.to_path_buf()])))
}

pub fn config() -> Config {
    let mut config = Config::default();
    config.appearance.color_scheme = ColorScheme::Dark;
    config.favorites = [
        "firefox",
        "org.nimbus.Files",
        "org.nimbus.Terminal",
        "org.nimbus.Mail",
        "org.nimbus.Music",
        "org.nimbus.Photos",
        "org.nimbus.Settings",
    ]
    .map(String::from)
    .to_vec();
    config
}

pub fn window(id: u64, app_id: &str, title: &str, workspace: u32, focused: bool) -> WindowInfo {
    WindowInfo {
        id,
        app_id: app_id.into(),
        title: title.into(),
        workspace,
        output: OUTPUT.into(),
        focused,
        ..Default::default()
    }
}

pub fn compositor_state() -> CompositorState {
    CompositorState {
        windows: vec![
            window(1, "firefox", "Nimbus — A Modern Wayland Desktop", 0, true),
            window(2, "org.nimbus.Terminal", "~/src/nimbus — cargo test", 0, false),
            window(3, "org.nimbus.Files", "Documents", 0, false),
            window(4, "org.nimbus.Terminal", "htop", 0, false),
            window(5, "org.nimbus.Music", "Music", 1, false),
            window(6, "org.nimbus.TextEditor", "notes.md", 2, false),
            window(7, "org.inkscape.Inkscape", "drawing.svg — Inkscape", 1, false),
        ],
        outputs: vec![OutputInfo {
            name: OUTPUT.into(),
            width: 1280,
            height: 800,
            scale: 1.0,
            refresh_mhz: 60_000,
        }],
        workspace_count: 4,
        active_workspace: 0,
        layout: LayoutMode::Floating,
    }
}

pub fn system_state() -> SystemState {
    SystemState {
        battery: Some(Battery {
            level: 0.78,
            charging: false,
            time_to_empty: Some(Duration::from_secs(3 * 3600 + 12 * 60)),
        }),
        network: Network {
            kind: ConnectionKind::Wifi,
            ssid: Some("Nimbus Studio".into()),
            strength: 0.82,
            wifi_enabled: true,
            available: true,
        },
        audio: Some(Audio { volume: 0.62, muted: false }),
        brightness: Some(0.7),
        media: Some(Media {
            player: "org.mpris.MediaPlayer2.music".into(),
            title: "Midnight City".into(),
            artist: "M83".into(),
            art_url: None,
            playing: true,
        }),
        bluetooth: Some(Bluetooth { powered: true, connected_devices: 1 }),
        do_not_disturb: false,
    }
}

pub fn notification(
    id: u32,
    app: &str,
    icon: &str,
    summary: &str,
    body: &str,
    minutes_ago: u64,
) -> Notification {
    Notification {
        id,
        app_name: app.into(),
        app_icon: icon.into(),
        summary: summary.into(),
        body: body.into(),
        actions: Vec::new(),
        urgency: Urgency::Normal,
        expire_timeout: None,
        received: SystemTime::now() - Duration::from_secs(minutes_ago * 60),
        transient: false,
        resident: false,
    }
}

pub fn notifications() -> Vec<Notification> {
    let mut update = notification(
        3,
        "Software",
        "org.nimbus.Software",
        "Updates Available",
        "3 updates are ready to install, including a <b>security fix</b>.",
        0,
    );
    update.actions = vec![
        ("default".into(), "Open".into()),
        ("install".into(), "Install".into()),
        ("later".into(), "Later".into()),
    ];
    vec![
        notification(
            1,
            "Calendar",
            "org.nimbus.Calendar",
            "Design Review",
            "Starts in 10 minutes · Room 4",
            12,
        ),
        notification(
            2,
            "Mail",
            "org.nimbus.Mail",
            "Alex Morgan",
            "Re: Shell screenshots — These look great! Can we get the overview into the release notes?",
            3,
        ),
        update,
    ]
}
