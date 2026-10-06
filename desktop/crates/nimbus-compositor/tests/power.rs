// SPDX-License-Identifier: MIT

//! Output power: blanking after inactivity, waking on input, idle inhibitors, and wlr-output-power-management.

mod common;

use common::Compositor;
use nimbus_ipc::{Event, PowerState, Request};
use std::time::Duration;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_output::{self, WlOutput},
    wl_registry::{self, WlRegistry},
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, ZwlrLayerSurfaceV1},
};
use wayland_protocols_wlr::output_power_management::v1::client::{
    zwlr_output_power_manager_v1::ZwlrOutputPowerManagerV1,
    zwlr_output_power_v1::{self, Mode, ZwlrOutputPowerV1},
};

const TWO_OUTPUTS: &str = "1280x720,800x600";

/// Starts the compositor on two outputs, blanking after `minutes` minutes of `minute_ms` milliseconds.
fn start(minutes: u32, minute_ms: u64) -> Compositor {
    common::compositor(&format!("[power]\nblank_after_minutes = {minutes}\n"))
        .outputs(TWO_OUTPUTS)
        .env("NIMBUS_HEADLESS_IDLE_MINUTE_MS", &minute_ms.to_string())
        .start()
}

/// What a power object was told.
#[derive(Default)]
struct Power {
    mode: Option<Mode>,
    failed: bool,
}

#[derive(Default)]
struct App {
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    layer_shell: Option<ZwlrLayerShellV1>,
    idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    power_manager: Option<ZwlrOutputPowerManagerV1>,
    /// Each output with its name.
    outputs: Vec<(WlOutput, String)>,
    /// Indexed by the power object's user data.
    powers: Vec<Power>,
    layer_configure: Option<(u32, u32, u32)>,
}

struct Probe {
    conn: Connection,
    queue: EventQueue<App>,
    qh: QueueHandle<App>,
    app: App,
}

impl Probe {
    fn connect(compositor: &Compositor) -> Self {
        let conn = common::connect(&compositor.wayland_socket());
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();
        conn.display().get_registry(&qh, ());
        let mut app = App::default();
        queue.roundtrip(&mut app).expect("roundtrip");
        queue.roundtrip(&mut app).expect("roundtrip");
        assert!(app.power_manager.is_some(), "no zwlr_output_power_manager_v1");
        Self { conn, queue, qh, app }
    }

    fn roundtrip(&mut self) {
        self.queue.roundtrip(&mut self.app).expect("roundtrip");
        self.conn.flush().expect("flush");
    }

    fn dispatch_until(&mut self, what: &str, cond: impl Fn(&App) -> bool) {
        common::dispatch_until(&self.conn, &mut self.queue, &mut self.app, what, cond);
    }

    /// Creates a power object for the output named `name` and returns its index once it's answered.
    fn power(&mut self, name: &str) -> (ZwlrOutputPowerV1, usize) {
        let output = &self.app.outputs.iter().find(|(_, n)| n == name).expect("the output").0;
        let index = self.app.powers.len();
        self.app.powers.push(Power::default());
        let object =
            self.app.power_manager.as_ref().unwrap().get_output_power(output, &self.qh, index);
        self.dispatch_until("the power object's answer", |app| {
            app.powers[index].mode.is_some() || app.powers[index].failed
        });
        (object, index)
    }

    fn mode(&self, index: usize) -> Option<Mode> {
        self.app.powers[index].mode
    }

    /// Maps a 20 pixel bar on the top layer, which is visible, and inhibits idling on it.
    fn inhibit_idling(&mut self) -> (WlSurface, ZwpIdleInhibitorV1) {
        let surface = self.app.compositor.as_ref().unwrap().create_surface(&self.qh, ());
        let layer_surface = self.app.layer_shell.as_ref().unwrap().get_layer_surface(
            &surface,
            None,
            zwlr_layer_shell_v1::Layer::Top,
            "video".into(),
            &self.qh,
            (),
        );
        layer_surface.set_anchor(Anchor::Top | Anchor::Left | Anchor::Right);
        layer_surface.set_size(0, 20);
        surface.commit();
        self.dispatch_until("the layer surface configure", |app| app.layer_configure.is_some());
        let (serial, width, height) = self.app.layer_configure.unwrap();
        layer_surface.ack_configure(serial);
        let size = (i32::try_from(width).unwrap(), i32::try_from(height).unwrap());
        common::attach_buffer(self.app.shm.as_ref().unwrap(), &self.qh, &surface, size);
        let inhibitor =
            self.app.idle_inhibit.as_ref().unwrap().create_inhibitor(&surface, &self.qh, ());
        self.roundtrip();
        (surface, inhibitor)
    }
}

fn is_black(compositor: &Compositor, output: &str) -> bool {
    compositor.screenshot(output).pixels().all(|p| p.0[..3] == [0, 0, 0])
}

fn power_events(event: Event) -> Option<PowerState> {
    match event {
        Event::PowerState(state) => Some(state),
        _ => None,
    }
}

fn names(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

#[test]
fn inactivity_blanks_every_output_until_input() {
    // Long enough to look at the woken outputs before they blank again.
    let compositor = start(1, 1000);
    let events = compositor.subscribe();
    let mut probe = Probe::connect(&compositor);
    let (_first, first) = probe.power("HEADLESS-1");
    let (_second, second) = probe.power("HEADLESS-2");

    probe.dispatch_until("both outputs off", |app| {
        app.powers.iter().all(|p| p.mode == Some(Mode::Off))
    });
    let blanked = PowerState { blanked: true, off: names(&["HEADLESS-1", "HEADLESS-2"]) };
    assert_eq!(compositor.power_state(), blanked);
    assert_eq!(events.wait("the blank state", power_events), blanked);
    assert!(is_black(&compositor, "HEADLESS-1"), "a blanked output shows black");

    // A key press wakes both outputs, and they blank again after the next timeout.
    compositor.request(Request::PressKey { code: 42 });
    probe.dispatch_until("both outputs on", |app| {
        app.powers.iter().all(|p| p.mode == Some(Mode::On))
    });
    assert_eq!(events.wait("the wake", power_events), PowerState::default());
    assert!(!is_black(&compositor, "HEADLESS-1"), "a woken output shows the desktop");
    probe.dispatch_until("both outputs off again", |app| {
        app.powers.iter().all(|p| p.mode == Some(Mode::Off))
    });
    assert_eq!(probe.mode(first), Some(Mode::Off));
    assert_eq!(probe.mode(second), Some(Mode::Off));

    // A click wakes them too.
    compositor.request(Request::Click { output: "HEADLESS-2".into(), x: 10.0, y: 10.0 });
    probe.dispatch_until("both outputs on after a click", |app| {
        app.powers.iter().all(|p| p.mode == Some(Mode::On))
    });
}

#[test]
fn a_visible_idle_inhibitor_keeps_the_outputs_on() {
    let minute_ms = 200;
    let compositor = start(0, minute_ms);
    let mut probe = Probe::connect(&compositor);
    let (_bar, inhibitor) = probe.inhibit_idling();

    std::fs::write(compositor.config_path(), "[power]\nblank_after_minutes = 1\n").unwrap();
    // Several timeouts and housekeeping ticks pass.
    std::thread::sleep(Duration::from_millis(5 * minute_ms + 1500));
    assert_eq!(compositor.power_state(), PowerState::default(), "the inhibitor was ignored");

    // Without the inhibitor, the outputs blank a timeout later.
    inhibitor.destroy();
    probe.roundtrip();
    common::wait_for("blanking", || compositor.power_state().blanked.then_some(()));
}

#[test]
fn power_clients_turn_single_outputs_off_and_on() {
    let compositor = start(0, 1000);
    let mut probe = Probe::connect(&compositor);
    let (power, index) = probe.power("HEADLESS-1");
    assert_eq!(probe.mode(index), Some(Mode::On));
    let (rival, rival_index) = probe.power("HEADLESS-1");
    assert!(probe.app.powers[rival_index].failed, "another object controls the output");
    rival.destroy();

    power.set_mode(Mode::Off);
    probe.dispatch_until("the output off", |app| app.powers[index].mode == Some(Mode::Off));
    let off = PowerState { blanked: false, off: names(&["HEADLESS-1"]) };
    assert_eq!(compositor.power_state(), off);
    assert!(is_black(&compositor, "HEADLESS-1"));
    assert!(!is_black(&compositor, "HEADLESS-2"), "the other output stays on");

    // Input doesn't turn on what a client turned off, but wakes what blanking turned off.
    compositor.request(Request::PressKey { code: 42 });
    assert_eq!(compositor.power_state(), off);
    compositor.request(Request::Blank);
    assert_eq!(
        compositor.power_state(),
        PowerState { blanked: true, off: names(&["HEADLESS-1", "HEADLESS-2"]) }
    );
    compositor.request(Request::PressKey { code: 42 });
    assert_eq!(compositor.power_state(), off);

    power.set_mode(Mode::On);
    probe.dispatch_until("the output on", |app| app.powers[index].mode == Some(Mode::On));
    assert_eq!(compositor.power_state(), PowerState::default());

    // Once the object is gone, another one can control the output.
    power.destroy();
    probe.roundtrip();
    let (_next, next) = probe.power("HEADLESS-1");
    assert_eq!(probe.mode(next), Some(Mode::On));
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
            "wl_compositor" => app.compositor = Some(registry.bind(name, version.min(5), qh, ())),
            "wl_shm" => app.shm = Some(registry.bind(name, 1, qh, ())),
            "wl_output" => {
                let output = registry.bind(name, version.min(4), qh, ());
                app.outputs.push((output, String::new()));
            }
            "zwlr_layer_shell_v1" => app.layer_shell = Some(registry.bind(name, 1, qh, ())),
            "zwp_idle_inhibit_manager_v1" => {
                app.idle_inhibit = Some(registry.bind(name, 1, qh, ()))
            }
            "zwlr_output_power_manager_v1" => {
                app.power_manager = Some(registry.bind(name, 1, qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, ()> for App {
    fn event(
        app: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Name { name } = event
            && let Some(entry) = app.outputs.iter_mut().find(|(o, _)| o == output)
        {
            entry.1 = name;
        }
    }
}

impl Dispatch<ZwlrOutputPowerV1, usize> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrOutputPowerV1,
        event: zwlr_output_power_v1::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let power = &mut app.powers[*index];
        match event {
            zwlr_output_power_v1::Event::Mode { mode: WEnum::Value(mode) } => {
                power.mode = Some(mode)
            }
            zwlr_output_power_v1::Event::Failed => power.failed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for App {
    fn event(
        app: &mut Self,
        _: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, width, height } = event {
            app.layer_configure = Some((serial, width, height));
        }
    }
}

delegate_noop!(App: ignore WlCompositor);
delegate_noop!(App: ignore WlSurface);
delegate_noop!(App: ignore WlShm);
delegate_noop!(App: ignore WlShmPool);
delegate_noop!(App: ignore WlBuffer);
delegate_noop!(App: ignore ZwlrLayerShellV1);
delegate_noop!(App: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(App: ignore ZwpIdleInhibitorV1);
delegate_noop!(App: ignore ZwlrOutputPowerManagerV1);
