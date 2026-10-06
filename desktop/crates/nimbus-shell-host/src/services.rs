// SPDX-License-Identifier: MIT

//! System services and the commands the compositor sends the shell, such as the volume keys.

use crate::ipc::log_failure;
use crate::state::State;
use anyhow::anyhow;
use nimbus_ipc::{Request, ShellCommand};
use nimbus_services::{
    Notification, ServiceCommand, ServiceEvent, Services, ServicesConfig, SystemState, Urgency,
};
use nimbus_shell::{Osd, ShellView};
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event as ChannelEvent};
use std::time::SystemTime;

/// Change per volume or brightness key press.
const LEVEL_STEP: f32 = 0.05;

pub struct SystemServices {
    services: Services,
    /// The last state from the services, which the volume and brightness keys update ahead of them.
    state: Option<SystemState>,
    /// Ids for the shell's own toasts, counting down from the top so they stay clear of the
    /// notification server's, which count up.
    next_local_notification: u32,
}

impl SystemServices {
    /// Starts the services; their events reach [`State::service_event`] on the event loop.
    pub fn spawn(handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        let (sender, receiver) = channel::channel::<ServiceEvent>();
        handle
            .insert_source(receiver, |event, _, state| {
                if let ChannelEvent::Msg(event) = event {
                    state.service_event(event);
                }
            })
            .map_err(|e| anyhow!("cannot receive service events: {e}"))?;
        let services = Services::spawn(ServicesConfig::default(), move |event| {
            let _ = sender.send(event);
        });
        Ok(Self { services, state: None, next_local_notification: u32::MAX })
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
            event => {
                if let ServiceEvent::State(system) = &event {
                    self.services.state = Some(system.clone());
                }
                self.model.handle_service_event(&event);
            }
        }
    }

    /// Carries out a command the compositor sends for a shortcut or a request.
    pub fn shell_command(&mut self, command: ShellCommand, output: Option<&str>) {
        match command {
            ShellCommand::ToggleLauncher => self.toggle_on(output, ShellView::toggle_launcher),
            ShellCommand::ToggleOverview => self.toggle_on(output, ShellView::toggle_overview),
            ShellCommand::VolumeUp => self.change_level(|s| volume_step(s, LEVEL_STEP)),
            ShellCommand::VolumeDown => self.change_level(|s| volume_step(s, -LEVEL_STEP)),
            ShellCommand::ToggleMute => {
                self.services.send(ServiceCommand::ToggleMute);
                if let Some(osd) = self.services.state.as_mut().and_then(toggle_mute_state) {
                    self.model.show_osd(osd);
                }
            }
            ShellCommand::BrightnessUp => self.change_level(|s| brightness_step(s, LEVEL_STEP)),
            ShellCommand::BrightnessDown => self.change_level(|s| brightness_step(s, -LEVEL_STEP)),
        }
    }

    fn change_level(
        &mut self,
        step: impl FnOnce(&mut SystemState) -> Option<(ServiceCommand, Osd)>,
    ) {
        if let Some((command, osd)) = self.services.state.as_mut().and_then(step) {
            self.services.send(command);
            self.model.show_osd(osd);
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

/// Changes the cached volume by `delta`, so the next key press builds on it before the audio service reports back.
fn volume_step(state: &mut SystemState, delta: f32) -> Option<(ServiceCommand, Osd)> {
    let audio = state.audio.as_mut()?;
    audio.volume = (audio.volume + delta).clamp(0.0, 1.0);
    let muted = audio.muted && delta <= 0.0;
    Some((ServiceCommand::SetVolume(audio.volume), Osd::Volume { level: audio.volume, muted }))
}

/// Flips the cached mute state; see [`volume_step`].
fn toggle_mute_state(state: &mut SystemState) -> Option<Osd> {
    let audio = state.audio.as_mut()?;
    audio.muted = !audio.muted;
    Some(Osd::Volume { level: audio.volume, muted: audio.muted })
}

/// Changes the cached brightness by `delta`; see [`volume_step`].
fn brightness_step(state: &mut SystemState, delta: f32) -> Option<(ServiceCommand, Osd)> {
    let level = state.brightness.as_mut()?;
    // Never fully dark: a black screen looks like a hang.
    *level = (*level + delta).clamp(0.01, 1.0);
    Some((ServiceCommand::SetBrightness(*level), Osd::Brightness { level: *level }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rapid_level_keys_build_on_each_other() {
        let mut state = SystemState {
            audio: Some(nimbus_services::Audio { volume: 0.5, muted: false }),
            brightness: Some(0.5),
            ..Default::default()
        };
        let commands: Vec<ServiceCommand> = (0..2)
            .filter_map(|_| volume_step(&mut state, LEVEL_STEP).map(|(command, _)| command))
            .collect();
        assert!(matches!(
            commands[..],
            [ServiceCommand::SetVolume(a), ServiceCommand::SetVolume(b)]
                if (a - 0.55).abs() < 1e-6 && (b - 0.60).abs() < 1e-6
        ));
        let mutes: Vec<Option<Osd>> = (0..2).map(|_| toggle_mute_state(&mut state)).collect();
        assert!(matches!(
            mutes[..],
            [Some(Osd::Volume { muted: true, .. }), Some(Osd::Volume { muted: false, .. })]
        ));
        brightness_step(&mut state, -LEVEL_STEP);
        brightness_step(&mut state, -LEVEL_STEP);
        assert!(state.brightness.is_some_and(|b| (b - 0.40).abs() < 1e-6));
    }
}
