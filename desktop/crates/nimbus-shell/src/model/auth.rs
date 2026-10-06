// SPDX-License-Identifier: MIT

//! polkit authentication requests in the shared model, shown one at a time in a dialog on one output.

use std::rc::Rc;

use nimbus_services::{
    AuthenticationCommand, AuthenticationEvent, AuthenticationRequest, Secret, ServiceCommand,
};
use slint::{ModelRc, SharedString, VecModel};

use super::Model;
use crate::{AuthState, ShellAction};

const AUTH_FAILED: &str = "Authentication failed, please try again";

/// A request and what its dialog shows.
struct Request {
    request: AuthenticationRequest,
    identity: usize,
    prompt: String,
    echo: bool,
    /// Waiting for the helper: for its first prompt, or its verdict on a response.
    busy: bool,
    error: String,
    info: String,
}

/// The requests in the order they arrived; the dialog shows the first.
#[derive(Default)]
pub struct Requests {
    list: Vec<Request>,
    /// The output the dialog of the first request shows on.
    output: Option<String>,
}

impl Requests {
    fn get(&mut self, id: u32) -> Option<&mut Request> {
        self.list.iter_mut().find(|r| r.request.id == id)
    }

    fn is_first(&self, id: u32) -> bool {
        self.list.first().is_some_and(|r| r.request.id == id)
    }
}

/// What the dialog's field turns a PAM prompt such as "Password: " into.
fn placeholder(prompt: &str) -> String {
    prompt.trim().trim_end_matches(':').trim_end().to_owned()
}

impl Model {
    pub fn handle_authentication(&self, event: &AuthenticationEvent) {
        let (first, reset) = {
            let mut state = self.state.borrow_mut();
            let requests = &mut state.auth;
            match event {
                AuthenticationEvent::Started(request) => {
                    requests.list.push(Request {
                        request: request.clone(),
                        identity: request.selected,
                        prompt: String::new(),
                        echo: false,
                        busy: true,
                        error: String::new(),
                        info: String::new(),
                    });
                    (requests.list.len() == 1, false)
                }
                AuthenticationEvent::Prompt { id, identity, prompt, echo } => {
                    let Some(request) = requests.get(*id) else { return };
                    request.identity = *identity;
                    request.prompt = placeholder(prompt);
                    request.echo = *echo;
                    request.busy = false;
                    (false, requests.is_first(*id))
                }
                AuthenticationEvent::Message { id, text, error } => {
                    let Some(request) = requests.get(*id) else { return };
                    if *error {
                        request.error.clone_from(text);
                    } else {
                        request.info.clone_from(text);
                    }
                    (false, false)
                }
                AuthenticationEvent::Failed { id } => {
                    let Some(request) = requests.get(*id) else { return };
                    if request.error.is_empty() {
                        request.error = AUTH_FAILED.into();
                    }
                    (false, requests.is_first(*id))
                }
                AuthenticationEvent::Ended { id } => {
                    let was_first = requests.is_first(*id);
                    requests.list.retain(|r| r.request.id != *id);
                    (was_first && !requests.list.is_empty(), was_first)
                }
            }
        };
        if first {
            self.open_auth();
        }
        self.show_auth();
        if reset {
            self.reset_auth_response();
        }
    }

    /// Picks the output for the first request's dialog, the focused window's, and clears the way for it there.
    fn open_auth(&self) {
        let output = {
            let mut state = self.state.borrow_mut();
            let output =
                state.windows.focused().map(|w| w.output.clone()).filter(|o| !o.is_empty());
            state.auth.output.clone_from(&output);
            output
        };
        let view = self.auth_view_for(output.as_deref());
        if let Some(view) = view {
            view.close_everything();
        }
    }

    fn auth_view_for(&self, output: Option<&str>) -> Option<Rc<crate::view::View>> {
        let views = self.views();
        let pinned = output.and_then(|o| views.iter().position(|v| v.output() == o));
        views.into_iter().nth(pinned.unwrap_or(0))
    }

    pub fn shows_auth(&self, output: &str) -> bool {
        let pinned = {
            let state = self.state.borrow();
            if state.auth.list.is_empty() || state.desktop.locked {
                return false;
            }
            state.auth.output.clone()
        };
        self.auth_view_for(pinned.as_deref()).is_some_and(|view| view.output() == output)
    }

    fn show_auth(&self) {
        {
            let mut state = self.state.borrow_mut();
            let state = &mut *state;
            let auth = match state.auth.list.first() {
                Some(r) => {
                    let icon = Some(r.request.icon_name.as_str()).filter(|i| !i.is_empty());
                    let visual = state.icons.visual(&r.request.action_id, icon);
                    let identities: Vec<SharedString> =
                        r.request.identities.iter().map(|i| i.as_str().into()).collect();
                    AuthState {
                        open: true,
                        message: r.request.message.as_str().into(),
                        visual,
                        identities: ModelRc::from(Rc::new(VecModel::from(identities))),
                        identity: i32::try_from(r.identity).unwrap_or(0),
                        prompt: r.prompt.as_str().into(),
                        echo: r.echo,
                        busy: r.busy,
                        error: r.error.as_str().into(),
                        info: r.info.as_str().into(),
                    }
                }
                None => AuthState::default(),
            };
            state.desktop.auth = auth;
        }
        self.publish();
    }

    fn reset_auth_response(&self) {
        for view in self.views() {
            if let Some(dialog) = view.auth_window() {
                dialog.set_response(SharedString::new());
                dialog.invoke_focus_response();
            }
        }
    }

    pub fn auth_responded(&self, response: String) {
        let id = {
            let mut state = self.state.borrow_mut();
            let Some(request) = state.auth.list.first_mut() else { return };
            if request.busy || response.is_empty() {
                return;
            }
            request.busy = true;
            request.error.clear();
            request.info.clear();
            request.request.id
        };
        self.show_auth();
        self.reset_auth_response();
        let response = Secret::from(response);
        self.emit_auth(AuthenticationCommand::Respond { id, response });
    }

    pub fn auth_identity_selected(&self, identity: usize) {
        let id = {
            let mut state = self.state.borrow_mut();
            let Some(request) = state.auth.list.first_mut() else { return };
            if identity == request.identity || identity >= request.request.identities.len() {
                return;
            }
            request.identity = identity;
            request.prompt.clear();
            request.busy = true;
            request.error.clear();
            request.info.clear();
            request.request.id
        };
        self.show_auth();
        self.reset_auth_response();
        self.emit_auth(AuthenticationCommand::SelectIdentity { id, identity });
    }

    /// Cancels the first request, and shows the next one right away.
    pub fn auth_cancelled(&self) {
        let Some(id) = self.state.borrow().auth.list.first().map(|r| r.request.id) else {
            return;
        };
        self.handle_authentication(&AuthenticationEvent::Ended { id });
        self.emit_auth(AuthenticationCommand::Cancel { id });
    }

    fn emit_auth(&self, command: AuthenticationCommand) {
        self.emit(ShellAction::Service(ServiceCommand::Authentication(command)));
    }
}
