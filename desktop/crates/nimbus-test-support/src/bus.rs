// SPDX-License-Identifier: MIT

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

/// A private session bus daemon, killed on drop.
pub struct PrivateBus {
    child: Child,
    pub address: String,
}

impl PrivateBus {
    /// Starts `dbus-daemon`, or prints why the test is skipped and returns `None` when it can't run.
    pub fn start() -> Option<Self> {
        let mut child = match Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                eprintln!("skipping: can't run dbus-daemon: {err}");
                return None;
            }
        };
        let mut address = String::new();
        if let Some(stdout) = child.stdout.take() {
            let _ = BufReader::new(stdout).read_line(&mut address);
        }
        let address = address.trim().to_owned();
        if address.is_empty() {
            eprintln!("skipping: dbus-daemon didn't print its address");
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(Self { child, address })
    }

    pub async fn connect(&self) -> zbus::Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .expect("connect to the private bus")
    }
}

impl Drop for PrivateBus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
