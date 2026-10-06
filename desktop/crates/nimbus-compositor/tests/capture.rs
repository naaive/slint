// SPDX-License-Identifier: MIT

//! Screen capture through wlr-screencopy and ext-image-copy-capture, checked against `Request::Screenshot`.

mod common;

use common::{Compositor, TestClient};
use nimbus_ipc::Request;
use std::fs::File;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::FileExt;
use std::time::Duration;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child,
};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1::{self, ExtForeignToplevelHandleV1},
    ext_foreign_toplevel_list_v1::{self, ExtForeignToplevelListV1},
};
use wayland_protocols::ext::image_capture_source::v1::client::{
    ext_foreign_toplevel_image_capture_source_manager_v1::ExtForeignToplevelImageCaptureSourceManagerV1,
    ext_image_capture_source_v1::ExtImageCaptureSourceV1,
    ext_output_image_capture_source_manager_v1::ExtOutputImageCaptureSourceManagerV1,
};
use wayland_protocols::ext::image_copy_capture::v1::client::{
    ext_image_copy_capture_frame_v1::{self, ExtImageCopyCaptureFrameV1},
    ext_image_copy_capture_manager_v1::{ExtImageCopyCaptureManagerV1, Options},
    ext_image_copy_capture_session_v1::{self, ExtImageCopyCaptureSessionV1},
};
use wayland_protocols_wlr::screencopy::v1::client::{
    zwlr_screencopy_frame_v1::{self, ZwlrScreencopyFrameV1},
    zwlr_screencopy_manager_v1::ZwlrScreencopyManagerV1,
};

/// What a capture frame of either protocol reported.
#[derive(Default)]
struct Frame {
    /// The shm format, size, and stride that a screencopy frame advertised.
    buffer: Option<(u32, u32, u32, u32)>,
    buffer_done: bool,
    transform: Option<u32>,
    damage: Vec<(i32, i32, i32, i32)>,
    ready: bool,
    failed: bool,
}

impl Frame {
    fn finished(&self) -> bool {
        self.ready || self.failed
    }
}

#[derive(Default)]
struct Session {
    size: Option<(u32, u32)>,
    shm_formats: Vec<u32>,
    done: bool,
    stopped: bool,
}

#[derive(Default)]
struct App {
    shm: Option<WlShm>,
    output: Option<WlOutput>,
    screencopy: Option<ZwlrScreencopyManagerV1>,
    output_sources: Option<ExtOutputImageCaptureSourceManagerV1>,
    toplevel_sources: Option<ExtForeignToplevelImageCaptureSourceManagerV1>,
    copy_capture: Option<ExtImageCopyCaptureManagerV1>,
    toplevels: Vec<ExtForeignToplevelHandleV1>,
    frame: Frame,
    session: Session,
}

/// An shm buffer whose pixels the test reads back.
struct ShmBuffer {
    file: File,
    buffer: WlBuffer,
    size: (u32, u32),
    stride: u32,
}

impl ShmBuffer {
    /// Reads the Xrgb8888 pixels as opaque RGBA.
    fn rgba(&self) -> Vec<u8> {
        let (width, height) = self.size;
        let mut data = vec![0u8; (self.stride * height) as usize];
        self.file.read_exact_at(&mut data, 0).unwrap();
        let mut pixels = Vec::with_capacity((width * height * 4) as usize);
        for row in data.chunks_exact(self.stride as usize) {
            for pixel in row[..(width * 4) as usize].chunks_exact(4) {
                pixels.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        pixels
    }
}

struct Capturer {
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
}

impl Capturer {
    fn connect(compositor: &Compositor) -> Self {
        let conn = common::connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App::default();
        queue.roundtrip(&mut app).expect("roundtrip");
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(app.screencopy.is_some(), "no zwlr_screencopy_manager_v1");
        assert!(app.copy_capture.is_some(), "no ext_image_copy_capture_manager_v1");
        Self { conn, queue, qh, app }
    }

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        common::dispatch_until(&self.conn, &mut self.queue, &mut self.app, what, cond);
    }

    /// Dispatches for a while, for checks that something doesn't happen.
    fn idle(&mut self) {
        for _ in 0..10 {
            self.queue.roundtrip(&mut self.app).expect("roundtrip");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn shm_buffer(&self, (width, height): (u32, u32)) -> ShmBuffer {
        let stride = width * 4;
        let len = (stride * height) as usize;
        let mut file = tempfile::tempfile().expect("shm file");
        file.write_all(&vec![0x55; len]).unwrap();
        let pool =
            self.app.shm.as_ref().unwrap().create_pool(file.as_fd(), len as i32, &self.qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride as i32,
            wl_shm::Format::Xrgb8888,
            &self.qh,
            (),
        );
        pool.destroy();
        ShmBuffer { file, buffer, size: (width, height), stride }
    }

    /// Captures the output with wlr-screencopy, returning RGBA pixels, or `None` when the frame failed.
    fn screencopy(&mut self) -> Option<((u32, u32), Vec<u8>)> {
        self.app.frame = Frame::default();
        let manager = self.app.screencopy.clone().unwrap();
        let frame = manager.capture_output(0, self.app.output.as_ref().unwrap(), &self.qh, ());
        self.dispatch_until("the screencopy buffer", |app| {
            app.frame.buffer_done || app.frame.failed
        });
        if self.app.frame.failed {
            return None;
        }
        let (format, width, height, stride) = self.app.frame.buffer.unwrap();
        assert_eq!(format, u32::from(wl_shm::Format::Xrgb8888));
        assert_eq!(stride, width * 4);
        let buffer = self.shm_buffer((width, height));
        frame.copy(&buffer.buffer);
        self.dispatch_until("the screencopy frame", |app| app.frame.finished());
        frame.destroy();
        buffer.buffer.destroy();
        self.app.frame.ready.then(|| ((width, height), buffer.rgba()))
    }

    fn output_source(&self) -> ExtImageCaptureSourceV1 {
        let manager = self.app.output_sources.as_ref().unwrap();
        manager.create_source(self.app.output.as_ref().unwrap(), &self.qh, ())
    }

    fn toplevel_source(&mut self) -> ExtImageCaptureSourceV1 {
        self.dispatch_until("a foreign toplevel", |app| !app.toplevels.is_empty());
        let manager = self.app.toplevel_sources.as_ref().unwrap();
        manager.create_source(&self.app.toplevels[0], &self.qh, ())
    }

    /// Starts a session and waits for its constraints.
    fn session(&mut self, source: &ExtImageCaptureSourceV1) -> ExtImageCopyCaptureSessionV1 {
        self.app.session = Session::default();
        let manager = self.app.copy_capture.as_ref().unwrap();
        let session = manager.create_session(source, Options::empty(), &self.qh, ());
        self.dispatch_until("the session constraints", |app| {
            app.session.done || app.session.stopped
        });
        assert!(
            self.app.session.shm_formats.contains(&u32::from(wl_shm::Format::Xrgb8888)),
            "Xrgb8888 isn't offered"
        );
        session
    }

    /// Asks for a frame of `session` into `buffer`; [`Capturer::finish`] waits for it.
    fn start_frame(
        &mut self,
        session: &ExtImageCopyCaptureSessionV1,
        buffer: &ShmBuffer,
    ) -> ExtImageCopyCaptureFrameV1 {
        self.app.frame = Frame::default();
        let frame = session.create_frame(&self.qh, ());
        frame.attach_buffer(&buffer.buffer);
        frame.damage_buffer(0, 0, buffer.size.0 as i32, buffer.size.1 as i32);
        frame.capture();
        self.conn.flush().unwrap();
        frame
    }

    fn finish(&mut self, frame: ExtImageCopyCaptureFrameV1) -> bool {
        self.dispatch_until("the captured frame", |app| app.frame.finished());
        frame.destroy();
        self.app.frame.ready
    }

    /// Captures one frame of a new session on `source`.
    fn capture(&mut self, source: &ExtImageCaptureSourceV1) -> Option<((u32, u32), Vec<u8>)> {
        let session = self.session(source);
        let size = self.app.session.size.unwrap();
        let buffer = self.shm_buffer(size);
        let frame = self.start_frame(&session, &buffer);
        let ready = self.finish(frame);
        session.destroy();
        ready.then(|| (size, buffer.rgba()))
    }
}

fn start_with_window() -> (Compositor, TestClient) {
    let compositor = common::start("");
    let mut client = TestClient::connect(&compositor);
    client.create_window("org.nimbus.Captured", "Captured");
    compositor.wait_state("a mapped window", |s| s.windows.len() == 1);
    (compositor, client)
}

fn assert_black(what: &str, pixels: &[u8]) {
    assert!(pixels.chunks_exact(4).all(|p| p[..3] == [0, 0, 0]), "{what} shows something");
}

#[test]
fn output_captures_match_screenshots() {
    let (compositor, _client) = start_with_window();
    let mut capturer = Capturer::connect(&compositor);
    let screenshot = compositor.screenshot("HEADLESS-1");

    let (size, pixels) = capturer.screencopy().expect("a screencopy frame");
    assert_eq!(size, screenshot.dimensions());
    assert!(pixels == screenshot.as_raw().as_slice(), "screencopy differs from the screenshot");

    let source = capturer.output_source();
    let (size, pixels) = capturer.capture(&source).expect("an image copy frame");
    assert_eq!(size, screenshot.dimensions());
    assert_eq!(capturer.app.frame.transform, Some(0));
    assert_eq!(capturer.app.frame.damage, vec![(0, 0, size.0 as i32, size.1 as i32)]);
    assert!(pixels == screenshot.as_raw().as_slice(), "image copy differs from the screenshot");
}

#[test]
fn image_copy_sessions_wait_for_damage() {
    let (compositor, mut client) = start_with_window();
    let mut capturer = Capturer::connect(&compositor);
    let source = capturer.output_source();
    let session = capturer.session(&source);
    let buffer = capturer.shm_buffer(capturer.app.session.size.unwrap());
    let frame = capturer.start_frame(&session, &buffer);
    assert!(capturer.finish(frame), "the first frame failed");

    let frame = capturer.start_frame(&session, &buffer);
    capturer.idle();
    assert!(!capturer.app.frame.finished(), "a frame without changes finished");
    client.create_window("org.nimbus.Second", "Second");
    assert!(capturer.finish(frame), "the frame after a change failed");
    let (width, height) = buffer.size;
    let damage = &capturer.app.frame.damage;
    assert!(!damage.is_empty(), "a new window caused no damage");
    assert!(
        damage.iter().all(|&(x, y, w, h)| x >= 0
            && y >= 0
            && w > 0
            && h > 0
            && x + w <= width as i32
            && y + h <= height as i32),
        "damage {damage:?} exceeds the buffer"
    );
}

#[test]
fn image_copy_sessions_of_blanked_outputs_wait_for_damage() {
    let (compositor, _client) = start_with_window();
    let mut capturer = Capturer::connect(&compositor);
    let source = capturer.output_source();
    let session = capturer.session(&source);
    let buffer = capturer.shm_buffer(capturer.app.session.size.unwrap());

    compositor.request(Request::Blank);
    let frame = capturer.start_frame(&session, &buffer);
    assert!(capturer.finish(frame), "the first frame failed");
    assert_black("the blanked output", &buffer.rgba());
    let frame = capturer.start_frame(&session, &buffer);
    capturer.idle();
    assert!(!capturer.app.frame.finished(), "a frame of a blanked output finished without changes");
    frame.destroy();
}

#[test]
fn toplevel_captures_show_the_window() {
    let (compositor, client) = start_with_window();
    let mut capturer = Capturer::connect(&compositor);
    let source = capturer.toplevel_source();
    let (size, pixels) = capturer.capture(&source).expect("a toplevel frame");
    let window = client.app.windows[0].size;
    assert_eq!(size, (window.0 as u32, window.1 as u32));
    assert!(pixels.iter().all(|&c| c == 255), "the white window isn't white");
}

#[test]
fn captures_while_locked_reveal_nothing() {
    let (compositor, _client) = start_with_window();
    let mut capturer = Capturer::connect(&compositor);
    let toplevel = capturer.toplevel_source();
    compositor.request(Request::Lock);
    assert!(compositor.locked());

    let (_, pixels) = capturer.screencopy().expect("a screencopy frame");
    assert_black("screencopy", &pixels);
    let output = capturer.output_source();
    let (_, pixels) = capturer.capture(&output).expect("an output frame");
    assert_black("the output", &pixels);
    let (_, pixels) = capturer.capture(&toplevel).expect("a toplevel frame");
    assert_black("the window", &pixels);
}

impl Dispatch<WlRegistry, ()> for App {
    fn event(
        app: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        match interface.as_str() {
            "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_output" if app.output.is_none() => {
                app.output = Some(registry.bind(name, version.min(4), qh, ()))
            }
            "zwlr_screencopy_manager_v1" => {
                assert_eq!(version, 3);
                app.screencopy = Some(registry.bind(name, 3, qh, ()));
            }
            "ext_output_image_capture_source_manager_v1" => {
                app.output_sources = Some(registry.bind(name, 1, qh, ()))
            }
            "ext_foreign_toplevel_image_capture_source_manager_v1" => {
                app.toplevel_sources = Some(registry.bind(name, 1, qh, ()))
            }
            "ext_image_copy_capture_manager_v1" => {
                app.copy_capture = Some(registry.bind(name, 1, qh, ()))
            }
            "ext_foreign_toplevel_list_v1" => {
                registry.bind::<ExtForeignToplevelListV1, _, _>(name, 1, qh, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrScreencopyFrameV1,
        event: zwlr_screencopy_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_screencopy_frame_v1::Event::Buffer { format, width, height, stride } => {
                let format = match format {
                    WEnum::Value(format) => u32::from(format),
                    WEnum::Unknown(format) => format,
                };
                app.frame.buffer = Some((format, width, height, stride));
            }
            zwlr_screencopy_frame_v1::Event::BufferDone => app.frame.buffer_done = true,
            zwlr_screencopy_frame_v1::Event::Ready { .. } => app.frame.ready = true,
            zwlr_screencopy_frame_v1::Event::Failed => app.frame.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureSessionV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureSessionV1,
        event: ext_image_copy_capture_session_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_session_v1::Event::BufferSize { width, height } => {
                app.session.size = Some((width, height));
            }
            ext_image_copy_capture_session_v1::Event::ShmFormat {
                format: WEnum::Value(format),
            } => {
                app.session.shm_formats.push(format.into());
            }
            ext_image_copy_capture_session_v1::Event::Done => app.session.done = true,
            ext_image_copy_capture_session_v1::Event::Stopped => app.session.stopped = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtImageCopyCaptureFrameV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtImageCopyCaptureFrameV1,
        event: ext_image_copy_capture_frame_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_image_copy_capture_frame_v1::Event::Transform {
                transform: WEnum::Value(transform),
            } => app.frame.transform = Some(transform.into()),
            ext_image_copy_capture_frame_v1::Event::Damage { x, y, width, height } => {
                app.frame.damage.push((x, y, width, height));
            }
            ext_image_copy_capture_frame_v1::Event::Ready => app.frame.ready = true,
            ext_image_copy_capture_frame_v1::Event::Failed { .. } => app.frame.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtForeignToplevelListV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ExtForeignToplevelListV1,
        event: ext_foreign_toplevel_list_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_list_v1::Event::Toplevel { toplevel } = event {
            app.toplevels.push(toplevel);
        }
    }

    event_created_child!(App, ExtForeignToplevelListV1, [
        ext_foreign_toplevel_list_v1::EVT_TOPLEVEL_OPCODE => (ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ExtForeignToplevelHandleV1, ()> for App {
    fn event(
        app: &mut Self,
        handle: &ExtForeignToplevelHandleV1,
        event: ext_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_foreign_toplevel_handle_v1::Event::Closed = event {
            app.toplevels.retain(|t| t != handle);
        }
    }
}

delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: ignore WlOutput);
delegate_noop!(App: ZwlrScreencopyManagerV1);
delegate_noop!(App: ExtOutputImageCaptureSourceManagerV1);
delegate_noop!(App: ExtForeignToplevelImageCaptureSourceManagerV1);
delegate_noop!(App: ExtImageCaptureSourceV1);
delegate_noop!(App: ExtImageCopyCaptureManagerV1);
