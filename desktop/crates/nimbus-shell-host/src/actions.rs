// SPDX-License-Identifier: MIT

//! What the user asks the shell for: compositor requests, service commands, and launching applications.

use crate::ipc::log_failure;
use crate::state::State;
use anyhow::anyhow;
use nimbus_ipc::{Request, Response};
use nimbus_services::ServiceCommand;
use nimbus_shell::ShellAction;
use nimbus_xdg::{AppIndex, DesktopEntry, IconResolver};
use smithay_client_toolkit::activation::{ActivationHandler, RequestDataExt};
use smithay_client_toolkit::reexports::calloop::LoopHandle;
use smithay_client_toolkit::reexports::calloop::channel::{self, Event as ChannelEvent, Sender};
use wayland_client::protocol::wl_seat::WlSeat;
use wayland_client::protocol::wl_surface::WlSurface;

/// What an activation token is for, so the started application may raise its window.
pub enum TokenPurpose {
    Launch(Box<DesktopEntry>),
    NotificationAction { id: u32, action: String },
}

/// An `xdg-activation` token request, proven by the serial of the user's last press.
pub struct TokenRequest {
    seat_and_serial: Option<(WlSeat, u32)>,
    purpose: TokenPurpose,
}

impl RequestDataExt for TokenRequest {
    fn app_id(&self) -> Option<&str> {
        None
    }

    fn seat_and_serial(&self) -> Option<(&WlSeat, u32)> {
        self.seat_and_serial.as_ref().map(|(seat, serial)| (seat, *serial))
    }

    fn surface(&self) -> Option<&WlSurface> {
        None
    }
}

/// The installed applications and icons, scanned in the background.
pub struct Apps {
    index: Option<AppIndex>,
    sender: Sender<(AppIndex, IconResolver)>,
}

impl Apps {
    pub fn new(handle: &LoopHandle<'static, State>) -> anyhow::Result<Self> {
        let (sender, receiver) = channel::channel::<(AppIndex, IconResolver)>();
        handle
            .insert_source(receiver, |event, _, state| {
                if let ChannelEvent::Msg((apps, icons)) = event {
                    tracing::info!(count = apps.entries.len(), "applications loaded");
                    state.model.set_apps(&apps, &icons);
                    state.apps.index = Some(apps);
                }
            })
            .map_err(|e| anyhow!("cannot receive applications: {e}"))?;
        Ok(Self { index: None, sender })
    }
}

impl State {
    /// Scans applications and loads the icon theme in the background, then hands them to the shell.
    pub fn reload_apps(&self) {
        let sender = self.apps.sender.clone();
        let icon_theme = self.settings.current.appearance.icon_theme.clone();
        let spawned = std::thread::Builder::new().name("nimbus-apps".into()).spawn(move || {
            let apps = AppIndex::scan();
            let icons = IconResolver::new(&icon_theme);
            // The receiver only goes away when the shell exits.
            let _ = sender.send((apps, icons));
        });
        if let Err(err) = spawned {
            tracing::error!("cannot start scanning applications: {err}");
        }
    }

    /// Handles the actions the views queued; actions can lead to more, for example through state the views react to.
    pub fn process_actions(&mut self) {
        for _ in 0..16 {
            let actions: Vec<ShellAction> = self.actions.borrow_mut().drain(..).collect();
            if actions.is_empty() {
                break;
            }
            for action in actions {
                self.handle_action(action);
            }
        }
    }

    fn handle_action(&mut self, action: ShellAction) {
        if self.model.is_locked() && !allowed_while_locked(&action) {
            tracing::warn!("ignoring a shell action while locked: {action:?}");
            return;
        }
        match action {
            ShellAction::Compositor(request) => {
                self.request(&request, log_failure("a shell request"));
            }
            ShellAction::Service(ServiceCommand::InvokeNotificationAction { id, action }) => {
                self.with_activation_token(TokenPurpose::NotificationAction { id, action });
            }
            ShellAction::Service(command) => self.services.send(command),
            ShellAction::Launch(id) => {
                match self.apps.index.as_ref().and_then(|apps| apps.get(&id)).cloned() {
                    Some(entry) => {
                        self.with_activation_token(TokenPurpose::Launch(Box::new(entry)))
                    }
                    None => {
                        tracing::warn!("cannot launch '{id}': no such application");
                        self.show_error(
                            format!("Couldn't start {id}"),
                            "The application isn't installed.".into(),
                        );
                    }
                }
            }
            ShellAction::OpenSettings(page) => {
                let mut command = String::from("nimbus-settings");
                if let Some(page) = page.as_deref().filter(|p| is_page_name(p)) {
                    command.push_str(" --page ");
                    command.push_str(page);
                }
                self.request(&Request::Spawn { command }, |state, response| {
                    if let Response::Error { message } = response {
                        tracing::warn!("cannot open Settings: {message}");
                        state.show_error("Couldn't open Settings".into(), message);
                    }
                });
            }
        }
    }

    /// Asks the compositor for an activation token, then carries out `purpose`, with no token if there's no way to get one.
    fn with_activation_token(&mut self, purpose: TokenPurpose) {
        match &self.activation {
            Some(activation) => {
                let seat_and_serial = self.input.seat.clone().zip(self.input.last_serial);
                activation
                    .request_token_with_data(&self.qh, TokenRequest { seat_and_serial, purpose });
            }
            None => self.carry_out(&purpose, None),
        }
    }

    fn carry_out(&mut self, purpose: &TokenPurpose, token: Option<String>) {
        match purpose {
            TokenPurpose::Launch(entry) => {
                if let Err(err) = nimbus_xdg::launch(entry, &[], token.as_deref()) {
                    tracing::warn!("cannot launch '{}': {err}", entry.id);
                    self.show_error(format!("Couldn't start {}", entry.name), err.to_string());
                }
            }
            TokenPurpose::NotificationAction { id, action } => {
                let (id, action) = (*id, action.clone());
                self.services.send(match token {
                    Some(activation_token) => ServiceCommand::InvokeNotificationActionWithToken {
                        id,
                        action,
                        activation_token,
                    },
                    None => ServiceCommand::InvokeNotificationAction { id, action },
                });
            }
        }
    }
}

impl ActivationHandler for State {
    type RequestData = TokenRequest;

    fn new_token(&mut self, token: String, data: &TokenRequest) {
        self.carry_out(&data.purpose, Some(token));
    }
}

/// The lock screen only locks again and closes notifications; everything else waits for an unlock.
fn allowed_while_locked(action: &ShellAction) -> bool {
    matches!(
        action,
        ShellAction::Compositor(Request::Lock)
            | ShellAction::Service(
                ServiceCommand::LockSession | ServiceCommand::CloseNotification { .. }
            )
    )
}

/// Settings page names are plain identifiers such as `appearance`.
fn is_page_name(page: &str) -> bool {
    !page.is_empty()
        && !page.starts_with('-')
        && page.len() <= 64
        && page.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lock_screen_blocks_desktop_actions() {
        assert!(!allowed_while_locked(&ShellAction::Launch("org.example.App".into())));
        assert!(!allowed_while_locked(&ShellAction::OpenSettings(None)));
        assert!(!allowed_while_locked(&ShellAction::Service(ServiceCommand::SetWifiEnabled(
            false
        ))));
        assert!(!allowed_while_locked(&ShellAction::Compositor(Request::Spawn {
            command: "sh".into()
        })));
        assert!(allowed_while_locked(&ShellAction::Compositor(Request::Lock)));
    }

    #[test]
    fn page_names_are_validated() {
        assert!(is_page_name("appearance"));
        assert!(is_page_name("night-light_2"));
        assert!(!is_page_name(""));
        assert!(!is_page_name("--evil"));
        assert!(!is_page_name("a b"));
        assert!(!is_page_name("a;rm"));
    }
}
