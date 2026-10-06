// SPDX-License-Identifier: MIT

//! System services and the commands the compositor sends the shell, such as the volume keys.

use crate::ipc::log_failure;
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
}

impl SystemServices {
    /// Starts the services; their events reach [`State::service_event`] on the event loop.
    pub fn spawn(handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        let sender =
            forward(handle, State::service_event).context("cannot receive service events")?;
        let services = Services::spawn(ServicesConfig::default(), move |event| {
            let _ = sender.send(event);
        });
        Ok(Self { services, next_local_notification: u32::MAX })
    }

    pub fn send(&self, command: ServiceCommand) {
        self.services.send(command);
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
        }
    }

    /// Toggles a full-output view on `output`, or the first one, and closes the launcher and overview elsewhere.
    fn toggle_on(&mut self, output: Option<&str>, toggle: fn(&ShellView)) {
        let target = self
            .outputs
            .iter()
            .position(|o| Some(o.name()) == output)
            .or((!self.outputs.is_empty()).then_some(0));
        let Some(target) = target else {
            return;
        };
        for (_, other) in self.outputs.iter().enumerate().filter(|(i, _)| *i != target) {
            let ui = other.view().component();
            if ui.get_launcher_open() {
                other.view().toggle_launcher();
            }
            if ui.get_overview_open() {
                other.view().toggle_overview();
            }
        }
        toggle(self.outputs[target].view());
    }

    /// Shows a toast, for errors the user would otherwise never see.
    pub fn show_error(&mut self, summary: String, body: String) {
        let id = self.services.next_local_notification;
        self.services.next_local_notification = id.wrapping_sub(1);
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
