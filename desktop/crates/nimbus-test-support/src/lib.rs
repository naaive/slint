// SPDX-License-Identifier: MIT

//! Harness code shared by the integration tests of the Nimbus crates.

mod bus;
mod compositor;
mod input_method;
mod udisks;
mod wayland;

pub use bus::PrivateBus;
pub use compositor::{Compositor, CompositorBuilder, Events, compositor_binary};
pub use input_method::{InputMethod, InputMethodState, TextInputState};
pub use udisks::{FAKE_DRIVE, FakeUdisks, MountAnswer};
pub use wayland::{
    TestClient, TestClientState, TestWindow, attach_buffer, connect, dispatch_until,
};

use std::time::{Duration, Instant};

/// How long a test waits for something before it fails.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// Polls `poll` until it returns a value, and fails after [`TIMEOUT`].
pub fn wait_for<T>(what: &str, mut poll: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(value) = poll() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
