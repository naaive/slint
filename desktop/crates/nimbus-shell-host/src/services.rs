// SPDX-License-Identifier: MIT

//! System services and the commands the compositor sends the shell, such as the volume keys.

use crate::battery::BatteryAlert;
use crate::ipc::log_failure;
use crate::media::Media;
use crate::state::{State, forward};
use anyhow::Context;
use nimbus_ipc::{Request, ShellCommand};
use nimbus_services::{
    Notification, ServiceCommand, ServiceEvent, Services, ServicesConfig, Urgency,
};
use nimbus_shell::ShellView;
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use std::time::SystemTime;

pub struct SystemServices {
    services: Services,
    /// Ids for the shell's own toasts, counting down from the top so they stay clear of the
    /// notification server's, which count up.
    next_local_notification: u32,
    pub media: Media,
    pub battery: BatteryAlert,
}

impl SystemServices {
    /// Starts the services; their events reach [`State::service_event`] on the event loop.
    pub fn spawn(handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        let sender =
            forward(handle, State::service_event).context("cannot receive service events")?;
        let services = Services::spawn(ServicesConfig::default(), move |event| {
            let _ = sender.send(event);
        });
        Ok(Self {
            services,
            next_local_notification: u32::MAX,
            media: Media::default(),
            battery: BatteryAlert::default(),
        })
    }

    pub fn send(&self, command: ServiceCommand) {
        self.services.send(command);
    }

    /// An id for one of the shell's own toasts.
    pub fn next_local_id(&mut self) -> u32 {
        let id = self.next_local_notification;
        self.next_local_notification = id.wrapping_sub(1);
        id
    }
}

impl State {
    fn service_event(&mut self, event: ServiceEvent) {
        match event {
            ServiceEvent::LogoutRequested => {
                // The compositor exits with status 0, which tells nimbus-session the user logged out.
                tracing::info!("logging out");
                self.request(&Request::Quit, log_failure("logging out"));
            }
            ServiceEvent::LockRequested => {
                self.lock();
                self.lock.report_presented = true;
                self.report_lock_presented();
            }
            ServiceEvent::UnlockRequested => self.unlock(),
            ServiceEvent::Disks(event) => self.disks_event(event),
            ServiceEvent::State(ref system) => {
                self.model.handle_service_event(&event);
                self.battery_changed(system.battery.as_ref());
            }
            event => self.model.handle_service_event(&event),
        }
    }

    /// Carries out a command the compositor sends for a shortcut or a request.
    pub fn shell_command(&mut self, command: ShellCommand, output: Option<&str>) {
        match command {
            ShellCommand::ToggleLauncher => self.toggle_on(output, ShellView::toggle_launcher),
            ShellCommand::ToggleOverview => self.toggle_on(output, ShellView::toggle_overview),
            level @ (ShellCommand::VolumeUp
            | ShellCommand::VolumeDown
            | ShellCommand::ToggleMute
            | ShellCommand::BrightnessUp
            | ShellCommand::BrightnessDown) => {
                if let Some(command) = self.model.step_level(level) {
                    self.services.send(command);
                }
            }
            ShellCommand::SwitcherOpen { windows, selected } => {
                let target = self.target_output(output);
                for (index, shell) in self.outputs.iter().enumerate() {
                    if Some(index) == target {
                        shell.view().open_switcher(windows.clone(), selected);
                    } else {
                        shell.view().close_switcher();
                    }
                }
            }
            ShellCommand::SwitcherStep { selected } => {
                for shell in &self.outputs {
                    shell.view().select_in_switcher(selected);
                }
            }
            ShellCommand::SwitcherCommit | ShellCommand::SwitcherCancel => {
                for shell in &self.outputs {
                    shell.view().close_switcher();
                }
            }
        }
    }

    /// The index of the output named `output`, or else of the first one.
    fn target_output(&self, output: Option<&str>) -> Option<usize> {
        self.outputs
            .iter()
            .position(|o| Some(o.name()) == output)
            .or((!self.outputs.is_empty()).then_some(0))
    }

    /// Toggles a full-output view on `output`, or the first one, and closes the launcher and overview elsewhere.
    fn toggle_on(&mut self, output: Option<&str>, toggle: fn(&ShellView)) {
        let Some(target) = self.target_output(output) else {
            return;
        };
        for (_, other) in self.outputs.iter().enumerate().filter(|(i, _)| *i != target) {
            if other.view().launcher_open() {
                other.view().toggle_launcher();
            }
            if other.view().overview_open() {
                other.view().toggle_overview();
            }
        }
        toggle(self.outputs[target].view());
    }

    /// Shows a toast, for errors the user would otherwise never see.
    pub fn show_error(&mut self, summary: String, body: String) {
        let id = self.services.next_local_id();
        self.model.handle_service_event(&ServiceEvent::Notification(Notification {
            id,
            app_name: "Nimbus".into(),
            app_icon: "dialog-error".into(),
            summary,
            body,
            actions: Vec::new(),
            urgency: Urgency::Normal,
            expire_timeout: None,
            received: SystemTime::now(),
            transient: true,
            resident: false,
        }));
    }
}
