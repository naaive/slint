// SPDX-License-Identifier: MIT

//! The display client against the headless compositor: listing heads and applying a configuration.

use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use nimbus_settings::displays::wlr::WlrControl;
use nimbus_settings::displays::{DisplayControl, DisplayEvent, Head, HeadConfig};
use nimbus_test_support::{Compositor, TIMEOUT};
use wayland_client::Connection;

fn next_heads(events: &mpsc::Receiver<DisplayEvent>) -> Vec<Head> {
    loop {
        match events.recv_timeout(TIMEOUT).expect("an event") {
            DisplayEvent::Heads(heads) => return heads,
            DisplayEvent::Unavailable(reason) => panic!("unavailable: {reason}"),
            DisplayEvent::Applied(_) => {}
        }
    }
}

#[test]
fn configures_the_compositor() {
    let compositor = Compositor::builder("").outputs("1280x720,1920x1080").start();
    let socket = compositor.wayland_socket();
    let (sender, events) = mpsc::channel();
    let control = WlrControl::spawn_on(
        move || {
            let stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
            Connection::from_socket(stream).map_err(|e| e.to_string())
        },
        Box::new(move |event| {
            let _ = sender.send(event);
        }),
    )
    .expect("the display thread starts");

    let heads = next_heads(&events);
    let names: Vec<&str> = heads.iter().map(|h| h.name.as_str()).collect();
    assert_eq!(names, ["HEADLESS-1", "HEADLESS-2"]);
    assert_eq!(heads[1].title(), "Headless");
    assert_eq!(heads[1].position, (1280, 0));
    let mode = heads[1].current_mode.expect("a current mode");
    assert_eq!((mode.width, mode.height, mode.preferred), (1920, 1080, true));

    let mut configuration: Vec<HeadConfig> = heads.iter().map(Head::config).collect();
    configuration[1].scale = 2.0;
    configuration[1].position = (0, 720);
    control.apply(configuration);
    let heads = next_heads(&events);
    assert_eq!((heads[1].scale, heads[1].position), (2.0, (0, 720)));
    match events.recv_timeout(TIMEOUT).expect("the outcome") {
        DisplayEvent::Applied(result) => assert_eq!(result, Ok(())),
        other => panic!("unexpected {other:?}"),
    }

    assert_eq!(compositor.state().outputs[1].scale, 2.0);
}
