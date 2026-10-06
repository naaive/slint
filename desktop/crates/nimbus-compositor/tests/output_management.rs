// SPDX-License-Identifier: MIT

//! wlr-output-management: listing heads, applying and testing configurations, and saving the layout.

mod common;

use common::Compositor;
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

const CONFIG: &str = "[appearance]\nscale = 1.0\n";

fn start_two_outputs(config: &str) -> Compositor {
    common::compositor(config).outputs("1280x720,1920x1080").start()
}

#[derive(Clone, Debug, Default)]
struct Head {
    proxy: Option<ZwlrOutputHeadV1>,
    name: String,
    make: String,
    modes: Vec<ObjectId>,
    current_mode: Option<ObjectId>,
    enabled: bool,
    position: (i32, i32),
    scale: f64,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Default)]
struct App {
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<Head>,
    modes: Vec<(ObjectId, (i32, i32), i32)>,
    serial: Option<u32>,
    outcome: Option<Outcome>,
}

impl App {
    fn head(&self, name: &str) -> &Head {
        self.heads.iter().find(|h| h.name == name && !h.finished).expect("head")
    }

    fn mode_size(&self, id: &ObjectId) -> (i32, i32) {
        self.modes.iter().find(|(m, _, _)| m == id).map(|(_, size, _)| *size).expect("mode")
    }
}

struct Client {
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
}

impl Client {
    fn connect(compositor: &Compositor) -> Self {
        let conn = common::connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App::default();
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(app.manager.is_some(), "no zwlr_output_manager_v1 global");
        let mut client = Self { conn, queue, qh, app };
        client.dispatch_until("the first done", |app| app.serial.is_some());
        client
    }

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        common::dispatch_until(&self.conn, &mut self.queue, &mut self.app, what, cond);
    }

    /// Applies or tests a configuration that keeps every head as it is, except for `edit`'s changes.
    fn configure(
        &mut self,
        serial: u32,
        test: bool,
        edit: impl Fn(&str, &ZwlrOutputConfigurationHeadV1),
    ) -> Outcome {
        let manager = self.app.manager.clone().unwrap();
        let configuration = manager.create_configuration(serial, &self.qh, ());
        for head in self.app.heads.iter().filter(|h| !h.finished) {
            let proxy = head.proxy.as_ref().unwrap();
            let config_head = configuration.enable_head(proxy, &self.qh, ());
            edit(&head.name, &config_head);
        }
        self.app.outcome = None;
        if test {
            configuration.test();
        } else {
            configuration.apply();
        }
        self.dispatch_until("the configuration's outcome", |app| app.outcome.is_some());
        configuration.destroy();
        self.app.outcome.take().unwrap()
    }

    fn serial(&self) -> u32 {
        self.app.serial.unwrap()
    }
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
        if let wl_registry::Event::Global { name, interface, version } = event
            && interface == ZwlrOutputManagerV1::interface().name
        {
            app.manager = Some(registry.bind(name, version.min(4), qh, ()));
        }
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => {
                app.heads.push(Head { proxy: Some(head), ..Head::default() });
            }
            zwlr_output_manager_v1::Event::Done { serial } => app.serial = Some(serial),
            _ => {}
        }
    }

    event_created_child!(App, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for App {
    fn event(
        app: &mut Self,
        proxy: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(head) = app.heads.iter_mut().find(|h| h.proxy.as_ref() == Some(proxy)) else {
            return;
        };
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Make { make } => head.make = make,
            zwlr_output_head_v1::Event::Mode { mode } => head.modes.push(mode.id()),
            zwlr_output_head_v1::Event::Enabled { enabled } => head.enabled = enabled != 0,
            zwlr_output_head_v1::Event::CurrentMode { mode } => head.current_mode = Some(mode.id()),
            zwlr_output_head_v1::Event::Position { x, y } => head.position = (x, y),
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            zwlr_output_head_v1::Event::Finished => head.finished = true,
            _ => {}
        }
    }

    event_created_child!(App, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for App {
    fn event(
        app: &mut Self,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let index = match app.modes.iter().position(|(id, _, _)| *id == proxy.id()) {
            Some(index) => index,
            None => {
                app.modes.push((proxy.id(), (0, 0), 0));
                app.modes.len() - 1
            }
        };
        match event {
            zwlr_output_mode_v1::Event::Size { width, height } => {
                app.modes[index].1 = (width, height)
            }
            zwlr_output_mode_v1::Event::Refresh { refresh } => app.modes[index].2 = refresh,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        app.outcome = match event {
            zwlr_output_configuration_v1::Event::Succeeded => Some(Outcome::Succeeded),
            zwlr_output_configuration_v1::Event::Failed => Some(Outcome::Failed),
            zwlr_output_configuration_v1::Event::Cancelled => Some(Outcome::Cancelled),
            _ => return,
        };
    }
}

wayland_client::delegate_noop!(App: ignore ZwlrOutputConfigurationHeadV1);

#[test]
fn lists_heads() {
    let compositor = start_two_outputs(CONFIG);
    let client = Client::connect(&compositor);
    let app = &client.app;
    let first = app.head("HEADLESS-1");
    let second = app.head("HEADLESS-2");
    assert_eq!(first.make, "Nimbus");
    assert!(first.enabled && second.enabled);
    assert_eq!((first.position, second.position), ((0, 0), (1280, 0)));
    assert_eq!(first.scale, 1.0);
    assert_eq!(app.mode_size(first.current_mode.as_ref().unwrap()), (1280, 720));
    assert_eq!(second.modes.len(), 1);
}

#[test]
fn applies_and_saves_a_layout() {
    let compositor = start_two_outputs(CONFIG);
    let mut client = Client::connect(&compositor);
    let serial = client.serial();

    // HEADLESS-2 at 200%, below HEADLESS-1.
    let outcome = client.configure(serial, false, |name, head| {
        if name == "HEADLESS-2" {
            head.set_scale(2.0);
            head.set_position(0, 720);
        }
    });
    assert_eq!(outcome, Outcome::Succeeded);
    client.dispatch_until("the new layout", |app| app.serial != Some(serial));
    let second = client.app.head("HEADLESS-2");
    assert_eq!((second.position, second.scale), ((0, 720), 2.0));
    let state = compositor.state();
    assert_eq!(state.outputs.iter().find(|o| o.name == "HEADLESS-2").unwrap().scale, 2.0);

    let path = compositor.dir.path().join("config.toml");
    let id = nimbus_config::OutputId {
        connector: "HEADLESS-2",
        make: "Nimbus",
        model: "Headless",
        serial: "",
    };
    // The compositor saves in the background.
    let entry = common::wait_for("a saved entry", || {
        nimbus_config::Config::load_from(&path).ok()?.output(id).cloned()
    });
    assert_eq!((entry.position, entry.scale, entry.mode), (Some([0, 720]), Some(2.0), None));
    let saved = std::fs::read_to_string(&path).unwrap();
    let config = nimbus_config::Config::load_from(&path).unwrap();
    let first = config.output(nimbus_config::OutputId { connector: "HEADLESS-1", ..id }).unwrap();
    assert_eq!((first.position, first.scale), (Some([0, 0]), None), "only what was set is stored");

    // A configuration made before the change is out of date.
    assert_eq!(client.configure(serial, false, |_, _| {}), Outcome::Cancelled);

    // The saved layout comes back after a restart.
    drop(client);
    drop(compositor);
    let compositor = start_two_outputs(&saved);
    let client = Client::connect(&compositor);
    let second = client.app.head("HEADLESS-2");
    assert_eq!((second.position, second.scale), ((0, 720), 2.0));
}

#[test]
fn refuses_what_it_cannot_do() {
    let compositor = start_two_outputs(CONFIG);
    let mut client = Client::connect(&compositor);
    let serial = client.serial();
    let unsupported = |_: &str, head: &ZwlrOutputConfigurationHeadV1| {
        head.set_custom_mode(999, 999, 0);
    };
    assert_eq!(client.configure(serial, true, unsupported), Outcome::Failed);
    assert_eq!(client.configure(serial, false, unsupported), Outcome::Failed);
    assert_eq!(
        client.configure(serial, true, |_, head| head.set_scale(0.1)),
        Outcome::Failed,
        "the scale is outside the supported range"
    );
    let apart = |name: &str, head: &ZwlrOutputConfigurationHeadV1| {
        if name == "HEADLESS-2" {
            head.set_position(1300, 0);
        }
    };
    assert_eq!(client.configure(serial, true, apart), Outcome::Failed, "a display apart");
    let same = |name: &str, head: &ZwlrOutputConfigurationHeadV1| {
        if name == "HEADLESS-1" {
            head.set_custom_mode(1280, 720, 60_000);
        }
    };
    assert_eq!(client.configure(serial, true, same), Outcome::Succeeded);
    assert_eq!(client.serial(), serial, "a test changes nothing");

    // Turning every head off isn't allowed.
    let manager = client.app.manager.clone().unwrap();
    let configuration = manager.create_configuration(serial, &client.qh, ());
    for head in &client.app.heads {
        configuration.disable_head(head.proxy.as_ref().unwrap());
    }
    client.app.outcome = None;
    configuration.apply();
    client.dispatch_until("the outcome", |app| app.outcome.is_some());
    assert_eq!(client.app.outcome, Some(Outcome::Failed));
}

#[test]
fn disables_and_enables_a_head() {
    let compositor = start_two_outputs(CONFIG);
    let mut client = Client::connect(&compositor);
    let serial = client.serial();
    let manager = client.app.manager.clone().unwrap();
    let configuration = manager.create_configuration(serial, &client.qh, ());
    configuration.enable_head(
        client.app.head("HEADLESS-1").proxy.as_ref().unwrap(),
        &client.qh,
        (),
    );
    configuration.disable_head(client.app.head("HEADLESS-2").proxy.as_ref().unwrap());
    client.app.outcome = None;
    configuration.apply();
    client.dispatch_until("the outcome", |app| app.outcome.is_some());
    assert_eq!(client.app.outcome, Some(Outcome::Succeeded));
    client.dispatch_until("HEADLESS-2 off", |app| !app.head("HEADLESS-2").enabled);
    compositor.wait_state("one output", |state| state.outputs.len() == 1);

    let serial = client.serial();
    assert_eq!(client.configure(serial, false, |_, _| {}), Outcome::Succeeded);
    client.dispatch_until("HEADLESS-2 on", |app| app.head("HEADLESS-2").enabled);
    compositor.wait_state("two outputs", |state| state.outputs.len() == 2);
}
