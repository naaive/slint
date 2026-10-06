// SPDX-License-Identifier: MIT

//! A wlr-output-management client on its own thread and Wayland connection,
//! apart from the one that shows the window.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use nimbus_config::Transform;
use rustix::event::{PollFd, PollFlags, poll};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::wl_output;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_configuration_head_v1::ZwlrOutputConfigurationHeadV1,
    zwlr_output_configuration_v1::{self, ZwlrOutputConfigurationV1},
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

use super::{ApplyError, DisplayControl, DisplayEvent, DisplayEvents, Head, HeadConfig, HeadMode};

/// The newest protocol version this client knows.
const VERSION: u32 = 4;

/// The handle to the client thread, which ends when this is dropped.
pub struct WlrControl {
    commands: mpsc::Sender<HeadConfigs>,
    wake: UnixStream,
}

type HeadConfigs = Vec<HeadConfig>;

impl WlrControl {
    /// Starts the client on the compositor that `WAYLAND_DISPLAY` names; it reports to `events`.
    pub fn spawn(events: DisplayEvents) -> std::io::Result<Self> {
        Self::spawn_on(
            || {
                Connection::connect_to_env()
                    .map_err(|e| format!("Settings isn't running in a Wayland session ({e})"))
            },
            events,
        )
    }

    /// Starts the client on the connection that `connect` makes on the client's thread.
    pub fn spawn_on(
        connect: impl FnOnce() -> Result<Connection, String> + Send + 'static,
        events: DisplayEvents,
    ) -> std::io::Result<Self> {
        let (wake, wakeup) = UnixStream::pair()?;
        wakeup.set_nonblocking(true)?;
        let (commands, receiver) = mpsc::channel();
        std::thread::Builder::new().name("nimbus-settings-displays".into()).spawn(move || {
            let mut client =
                Client { events, manager: None, heads: Vec::new(), serial: 0, pending: None };
            if let Err(reason) =
                connect().and_then(|conn| run(conn, &mut client, &receiver, wakeup))
            {
                (client.events)(DisplayEvent::Unavailable(reason));
            }
        })?;
        Ok(Self { commands, wake })
    }
}

impl DisplayControl for WlrControl {
    fn apply(&self, configuration: Vec<HeadConfig>) {
        if self.commands.send(configuration).is_ok() {
            // A full socket buffer already holds a wake-up.
            let _ = (&self.wake).write(&[1]);
        }
    }
}

impl Drop for WlrControl {
    fn drop(&mut self) {
        // The thread ends when it finds the other end closed.
        let _ = self.wake.shutdown(std::net::Shutdown::Both);
    }
}

fn run(
    conn: Connection,
    client: &mut Client,
    commands: &mpsc::Receiver<HeadConfigs>,
    mut wakeup: UnixStream,
) -> Result<(), String> {
    let lost =
        |error: &dyn std::fmt::Display| format!("The connection to the compositor failed: {error}");
    let mut queue: EventQueue<Client> = conn.new_event_queue();
    let qh = queue.handle();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(client).map_err(|e| lost(&e))?;
    let Some(manager) = client.manager.clone() else {
        return Err("The compositor doesn't let applications configure displays".into());
    };
    loop {
        queue.dispatch_pending(client).map_err(|e| lost(&e))?;
        queue.flush().map_err(|e| lost(&e))?;
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (readable, woken) = {
            let connection = guard.connection_fd();
            let mut fds =
                [PollFd::new(&connection, PollFlags::IN), PollFd::new(&wakeup, PollFlags::IN)];
            match poll(&mut fds, None) {
                Ok(_) => {}
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(lost(&error)),
            }
            (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
        };
        if readable {
            guard.read().map_err(|e| lost(&e))?;
        } else {
            drop(guard);
        }
        if woken {
            let mut buffer = [0u8; 64];
            // Reading nothing means the control was dropped.
            if let Ok(0) = wakeup.read(&mut buffer) {
                return Ok(());
            }
            while let Ok(configuration) = commands.try_recv() {
                client.apply(&manager, &qh, &configuration);
            }
        }
    }
}

/// A head as it's being described, between `done` events.
struct ClientHead {
    proxy: ZwlrOutputHeadV1,
    head: Head,
    modes: Vec<(ZwlrOutputModeV1, HeadMode)>,
    current_mode: Option<ObjectId>,
}

struct Client {
    events: DisplayEvents,
    manager: Option<ZwlrOutputManagerV1>,
    heads: Vec<ClientHead>,
    serial: u32,
    pending: Option<ZwlrOutputConfigurationV1>,
}

impl Client {
    fn snapshot(&self) -> Vec<Head> {
        self.heads
            .iter()
            .map(|h| {
                let mut head = h.head.clone();
                head.modes = h.modes.iter().map(|(_, mode)| *mode).collect();
                head.current_mode = h
                    .current_mode
                    .as_ref()
                    .and_then(|id| h.modes.iter().find(|(proxy, _)| &proxy.id() == id))
                    .map(|(_, mode)| *mode);
                head
            })
            .collect()
    }

    fn apply(
        &mut self,
        manager: &ZwlrOutputManagerV1,
        qh: &QueueHandle<Client>,
        configuration: &[HeadConfig],
    ) {
        if self.pending.is_some() {
            (self.events)(DisplayEvent::Applied(Err(ApplyError::Outdated)));
            return;
        }
        let pending = manager.create_configuration(self.serial, qh, ());
        for head in &self.heads {
            let config = configuration.iter().find(|c| c.name == head.head.name);
            // A head that the configuration doesn't name keeps its settings.
            if !config.map_or(head.head.enabled, |c| c.enabled) {
                pending.disable_head(&head.proxy);
                continue;
            }
            let configured = pending.enable_head(&head.proxy, qh, ());
            let Some(config) = config else {
                continue;
            };
            if let Some(mode) = config.mode {
                match head.modes.iter().find(|(_, m)| m.same(&mode)) {
                    Some((proxy, _)) => configured.set_mode(proxy),
                    None => configured.set_custom_mode(mode.width, mode.height, mode.refresh_mhz),
                }
            }
            configured.set_position(config.position.0, config.position.1);
            configured.set_transform(
                wl_output::Transform::try_from(u32::from(config.transform))
                    .unwrap_or(wl_output::Transform::Normal),
            );
            configured.set_scale(config.scale);
        }
        pending.apply();
        self.pending = Some(pending);
    }
}

impl Dispatch<WlRegistry, ()> for Client {
    fn event(
        client: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event
            && interface == ZwlrOutputManagerV1::interface().name
        {
            client.manager = Some(registry.bind(name, version.min(VERSION), qh, ()));
        }
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Client {
    fn event(
        client: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_output_manager_v1::Event::Head { head } => client.heads.push(ClientHead {
                proxy: head,
                head: Head::default(),
                modes: Vec::new(),
                current_mode: None,
            }),
            zwlr_output_manager_v1::Event::Done { serial } => {
                client.serial = serial;
                (client.events)(DisplayEvent::Heads(client.snapshot()));
            }
            zwlr_output_manager_v1::Event::Finished => {
                (client.events)(DisplayEvent::Unavailable(
                    "The compositor stopped reporting displays".into(),
                ));
            }
            _ => {}
        }
    }

    event_created_child!(Client, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_output_head_v1::Event::Finished = event {
            client.heads.retain(|h| &h.proxy != proxy);
            if proxy.version() >= 3 {
                proxy.release();
            }
            return;
        }
        let Some(entry) = client.heads.iter_mut().find(|h| &h.proxy == proxy) else {
            return;
        };
        let head = &mut entry.head;
        match event {
            zwlr_output_head_v1::Event::Name { name } => head.name = name,
            zwlr_output_head_v1::Event::Make { make } => head.make = make,
            zwlr_output_head_v1::Event::Model { model } => head.model = model,
            zwlr_output_head_v1::Event::Mode { mode } => {
                entry.modes.push((mode, HeadMode::default()))
            }
            zwlr_output_head_v1::Event::Enabled { enabled } => head.enabled = enabled != 0,
            zwlr_output_head_v1::Event::CurrentMode { mode } => {
                entry.current_mode = Some(mode.id())
            }
            zwlr_output_head_v1::Event::Position { x, y } => head.position = (x, y),
            zwlr_output_head_v1::Event::Transform { transform } => {
                head.transform = transform
                    .into_result()
                    .ok()
                    .and_then(|transform| Transform::try_from(u32::from(transform)).ok())
                    .unwrap_or_default();
            }
            zwlr_output_head_v1::Event::Scale { scale } => head.scale = scale,
            _ => {}
        }
    }

    event_created_child!(Client, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &ZwlrOutputModeV1,
        event: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        for head in &mut client.heads {
            if let zwlr_output_mode_v1::Event::Finished = event {
                head.modes.retain(|(mode, _)| mode != proxy);
                continue;
            }
            let Some((_, mode)) = head.modes.iter_mut().find(|(mode, _)| mode == proxy) else {
                continue;
            };
            match event {
                zwlr_output_mode_v1::Event::Size { width, height } => {
                    (mode.width, mode.height) = (width, height);
                }
                zwlr_output_mode_v1::Event::Refresh { refresh } => mode.refresh_mhz = refresh,
                zwlr_output_mode_v1::Event::Preferred => mode.preferred = true,
                _ => {}
            }
        }
        if let zwlr_output_mode_v1::Event::Finished = event
            && proxy.version() >= 3
        {
            proxy.release();
        }
    }
}

impl Dispatch<ZwlrOutputConfigurationV1, ()> for Client {
    fn event(
        client: &mut Self,
        proxy: &ZwlrOutputConfigurationV1,
        event: zwlr_output_configuration_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let result = match event {
            zwlr_output_configuration_v1::Event::Succeeded => Ok(()),
            zwlr_output_configuration_v1::Event::Failed => Err(ApplyError::Failed),
            zwlr_output_configuration_v1::Event::Cancelled => Err(ApplyError::Outdated),
            _ => return,
        };
        proxy.destroy();
        client.pending = None;
        (client.events)(DisplayEvent::Applied(result));
    }
}

wayland_client::delegate_noop!(Client: ignore ZwlrOutputConfigurationHeadV1);
