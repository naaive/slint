// SPDX-License-Identifier: MIT

//! Outputs reported by the compositor over the Nimbus control socket.

use std::path::Path;

use nimbus_ipc::{Client, OutputInfo, Request, Response};

#[derive(Debug, thiserror::Error)]
pub enum DisplayError {
    #[error("Nimbus isn't running, so displays can't be listed")]
    NotRunning,
    #[error("the compositor didn't answer: {0}")]
    Ipc(#[from] nimbus_ipc::Error),
    #[error("the compositor sent an unexpected reply")]
    Unexpected,
}

fn query(mut client: Client) -> Result<Vec<OutputInfo>, DisplayError> {
    match client.request(&Request::GetState)? {
        Response::State(state) => Ok(state.outputs),
        _ => Err(DisplayError::Unexpected),
    }
}

/// Asks the running compositor for its outputs. Blocks on the socket.
pub fn fetch() -> Result<Vec<OutputInfo>, DisplayError> {
    let path = nimbus_ipc::socket_path().ok_or(DisplayError::NotRunning)?;
    fetch_from(&path)
}

pub fn fetch_from(path: &Path) -> Result<Vec<OutputInfo>, DisplayError> {
    match Client::connect_to(path) {
        Ok(client) => query(client),
        Err(nimbus_ipc::Error::Io(_)) => Err(DisplayError::NotRunning),
        Err(other) => Err(other.into()),
    }
}

/// One output as the Displays page shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayRow {
    pub name: String,
    pub resolution: String,
    pub refresh: String,
    pub scale: String,
    /// The size in logical pixels, for the arrangement preview.
    pub logical_width: f32,
    pub logical_height: f32,
}

/// A refresh rate in millihertz as `60 Hz` or `59.95 Hz`; empty when unknown.
pub fn format_refresh(mhz: u32) -> String {
    if mhz == 0 {
        return String::new();
    }
    let hz = f64::from(mhz) / 1000.0;
    let text = format!("{hz:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text} Hz")
}

pub fn format_scale(scale: f64) -> String {
    if !scale.is_finite() || scale <= 0.0 {
        return "100%".into();
    }
    format!("{}%", (scale * 100.0).round())
}

pub fn rows(outputs: &[OutputInfo]) -> Vec<DisplayRow> {
    outputs
        .iter()
        .map(|o| {
            let scale = if o.scale.is_finite() && o.scale > 0.0 { o.scale } else { 1.0 };
            DisplayRow {
                name: o.name.clone(),
                resolution: format!("{} × {}", o.width.max(0), o.height.max(0)),
                refresh: format_refresh(o.refresh_mhz),
                scale: format_scale(o.scale),
                logical_width: (f64::from(o.width.max(0)) / scale) as f32,
                logical_height: (f64::from(o.height.max(0)) / scale) as f32,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nimbus_ipc::{CompositorState, read_message, write_message};

    #[test]
    fn formatting() {
        assert_eq!(format_refresh(60000), "60 Hz");
        assert_eq!(format_refresh(59951), "59.95 Hz");
        assert_eq!(format_refresh(143_900), "143.9 Hz");
        assert_eq!(format_refresh(0), "");
        assert_eq!(format_scale(1.25), "125%");
        assert_eq!(format_scale(f64::NAN), "100%");
        let row = &rows(&[OutputInfo {
            name: "eDP-1".into(),
            width: 2880,
            height: 1800,
            scale: 2.0,
            refresh_mhz: 90000,
        }])[0];
        assert_eq!(row.resolution, "2880 × 1800");
        assert_eq!((row.logical_width, row.logical_height), (1440.0, 900.0));
        let broken = &rows(&[OutputInfo { width: -5, scale: 0.0, ..Default::default() }])[0];
        assert_eq!(broken.logical_width, 0.0);
    }

    #[test]
    fn missing_socket_means_not_running() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(fetch_from(&dir.path().join("none.sock")), Err(DisplayError::NotRunning)));
    }

    #[test]
    fn queries_the_compositor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nimbus.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
            let mut writer = stream;
            let request: Request = read_message(&mut reader).unwrap();
            assert_eq!(request, Request::GetState);
            let outputs = vec![OutputInfo {
                name: "HDMI-A-1".into(),
                width: 1920,
                height: 1080,
                scale: 1.0,
                refresh_mhz: 60000,
            }];
            write_message(
                &mut writer,
                &Response::State(CompositorState { outputs, ..Default::default() }),
            )
            .unwrap();
        });
        let outputs = fetch_from(&path).unwrap();
        assert_eq!(outputs[0].name, "HDMI-A-1");
        server.join().unwrap();
    }
}
