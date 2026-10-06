// SPDX-License-Identifier: MIT

//! The lock marker that keeps a compositor crash from unlocking the session.

mod common;

use common::Compositor;
use nimbus_ipc::Request;
use std::os::unix::fs::PermissionsExt;

const CONFIG: &str = "[appearance]\nanimations = false\n";

#[test]
fn a_lock_marker_at_startup_starts_locked() {
    let compositor = Compositor::start_in(CONFIG, &[], |runtime| {
        let marker = nimbus_ipc::lock_marker_path(runtime);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, "").unwrap();
    });
    assert!(compositor.locked(), "{}", compositor.log());
    assert!(nimbus_ipc::lock_marker_path(&compositor.runtime_dir()).exists());
}

#[test]
fn the_locked_flag_starts_locked_and_creates_the_marker() {
    let compositor = Compositor::start_with_args(CONFIG, &["--locked"]);
    assert!(compositor.locked(), "{}", compositor.log());
    assert!(nimbus_ipc::lock_marker_path(&compositor.runtime_dir()).exists());
}

#[test]
fn locking_creates_a_private_marker() {
    let compositor = Compositor::start(CONFIG, &[]);
    let marker = nimbus_ipc::lock_marker_path(&compositor.runtime_dir());
    assert!(!compositor.locked());
    assert!(!marker.exists());
    compositor.request(Request::Lock);
    assert!(compositor.locked());
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&marker), 0o600);
    assert_eq!(mode(marker.parent().unwrap()), 0o700);
}
