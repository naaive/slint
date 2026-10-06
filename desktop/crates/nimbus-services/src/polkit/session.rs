// SPDX-License-Identifier: MIT

//! One authentication: runs the helper for the chosen identity until it succeeds,
//! starting it again after a wrong response or another choice of identity.

use std::path::Path;

use tokio::sync::mpsc::UnboundedReceiver;

use super::helper::{Helper, Message};
use super::{AuthenticationEvent, Secret};

/// What the user does in the dialog.
#[derive(Debug)]
pub(crate) enum Input {
    Respond(Secret),
    SelectIdentity(usize),
    Cancel,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Authenticated,
    Cancelled,
    /// The helper couldn't run or refused without asking anything, so trying again wouldn't help.
    Failed(String),
}

/// How one run of the helper ended.
enum Run {
    Done(Outcome),
    /// A wrong response, or a new identity: start the helper again.
    Again,
}

/// Authenticates as one of `users`, starting with `selected`, and reports progress for request `id` through `emit`.
pub(crate) async fn authenticate(
    id: u32,
    helper: &Path,
    cookie: &str,
    users: &[String],
    mut selected: usize,
    inputs: &mut UnboundedReceiver<Input>,
    emit: &(dyn Fn(AuthenticationEvent) + Sync),
) -> Outcome {
    loop {
        let Some(user) = users.get(selected) else {
            return Outcome::Failed("no identity to authenticate as".into());
        };
        let mut process = match Helper::spawn(helper, user, cookie).await {
            Ok(process) => process,
            Err(err) => return Outcome::Failed(format!("can't run {}: {err}", helper.display())),
        };
        match run(id, &mut process, selected, inputs, emit).await {
            (Run::Done(outcome), _) => return outcome,
            (Run::Again, Some(identity)) => selected = identity,
            (Run::Again, None) => {}
        }
    }
}

/// Drives one helper process; returns how it ended and the newly selected identity, if any.
async fn run(
    id: u32,
    process: &mut Helper,
    identity: usize,
    inputs: &mut UnboundedReceiver<Input>,
    emit: &(dyn Fn(AuthenticationEvent) + Sync),
) -> (Run, Option<usize>) {
    let mut prompted = false;
    let mut error = None;
    loop {
        tokio::select! {
            message = process.next() => {
                let failed = match message {
                    Ok(Some(Message::Prompt { text, echo })) => {
                        prompted = true;
                        emit(AuthenticationEvent::Prompt { id, identity, prompt: text, echo });
                        continue;
                    }
                    Ok(Some(Message::Info(text))) => {
                        emit(AuthenticationEvent::Message { id, text, error: false });
                        continue;
                    }
                    Ok(Some(Message::Error(text))) => {
                        error = Some(text.clone());
                        emit(AuthenticationEvent::Message { id, text, error: true });
                        continue;
                    }
                    Ok(Some(Message::Success)) => return (Run::Done(Outcome::Authenticated), None),
                    Ok(Some(Message::Failure)) => "authentication failed".to_owned(),
                    Ok(None) => "polkit-agent-helper-1 exited without a verdict".to_owned(),
                    Err(err) => format!("can't read from polkit-agent-helper-1: {err}"),
                };
                if !prompted {
                    return (Run::Done(Outcome::Failed(error.unwrap_or(failed))), None);
                }
                emit(AuthenticationEvent::Failed { id });
                return (Run::Again, None);
            }
            input = inputs.recv() => match input {
                Some(Input::Respond(response)) => {
                    if let Err(err) = process.respond(&response).await {
                        tracing::debug!("Can't answer polkit-agent-helper-1: {err}");
                        emit(AuthenticationEvent::Failed { id });
                        return (Run::Again, None);
                    }
                }
                Some(Input::SelectIdentity(selected)) => return (Run::Again, Some(selected)),
                Some(Input::Cancel) | None => return (Run::Done(Outcome::Cancelled), None),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
    use tokio::time::timeout;

    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    /// A stand-in for polkit-agent-helper-1: it checks the cookie, asks for one password, and succeeds if it's `secret`.
    /// `alice` gets an info message first.
    const FAKE_HELPER: &str = r#"#!/bin/sh
read -r cookie
[ "$cookie" = "cookie-1" ] || { echo FAILURE; exit 1; }
[ "$1" = "alice" ] && printf '%s\n' 'PAM_TEXT_INFO Hello\tAlice'
echo "PAM_PROMPT_ECHO_OFF Password for $1: "
read -r password || exit 1
if [ "$password" = "secret" ]; then echo SUCCESS; else echo "PAM_ERROR_MSG Wrong"; echo FAILURE; fi
"#;

    /// A helper that refuses without asking, as for a locked account.
    const REFUSING_HELPER: &str =
        "#!/bin/sh\nread -r cookie\necho 'PAM_ERROR_MSG Account locked'\necho FAILURE\n";

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        // Writing an executable while another thread forks can make exec fail with ETXTBSY; keep writes apart.
        static WRITING: Mutex<()> = Mutex::new(());
        let _guard = WRITING.lock().unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    struct Session {
        inputs: UnboundedSender<Input>,
        events: tokio::sync::mpsc::UnboundedReceiver<AuthenticationEvent>,
        outcome: tokio::task::JoinHandle<Outcome>,
    }

    fn start(helper: PathBuf, users: &[&str], selected: usize) -> Session {
        let (inputs, mut input_receiver) = unbounded_channel();
        let (event_sender, events) = unbounded_channel();
        let users: Vec<String> = users.iter().map(|user| (*user).to_owned()).collect();
        let outcome = tokio::spawn(async move {
            let emit = move |event| {
                let _ = event_sender.send(event);
            };
            authenticate(7, &helper, "cookie-1", &users, selected, &mut input_receiver, &emit).await
        });
        Session { inputs, events, outcome }
    }

    impl Session {
        async fn event(&mut self) -> AuthenticationEvent {
            timeout(WAIT, self.events.recv()).await.expect("an event in time").expect("an event")
        }

        async fn outcome(self) -> Outcome {
            timeout(WAIT, self.outcome).await.expect("an outcome in time").unwrap()
        }

        fn send(&self, input: Input) {
            self.inputs.send(input).unwrap();
        }
    }

    fn prompt(identity: usize, user: &str) -> AuthenticationEvent {
        let prompt = format!("Password for {user}: ");
        AuthenticationEvent::Prompt { id: 7, identity, prompt, echo: false }
    }

    #[tokio::test]
    async fn retries_until_the_password_is_right() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = start(script(dir.path(), "helper", FAKE_HELPER), &["bob"], 0);
        assert_eq!(session.event().await, prompt(0, "bob"));
        session.send(Input::Respond(Secret::from("wrong")));
        assert_eq!(
            session.event().await,
            AuthenticationEvent::Message { id: 7, text: "Wrong".into(), error: true }
        );
        assert_eq!(session.event().await, AuthenticationEvent::Failed { id: 7 });
        assert_eq!(session.event().await, prompt(0, "bob"));
        session.send(Input::Respond(Secret::from("secret")));
        assert_eq!(session.outcome().await, Outcome::Authenticated);
    }

    #[tokio::test]
    async fn switching_identity_restarts_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = start(script(dir.path(), "helper", FAKE_HELPER), &["bob", "alice"], 0);
        assert_eq!(session.event().await, prompt(0, "bob"));
        session.send(Input::SelectIdentity(1));
        assert_eq!(
            session.event().await,
            AuthenticationEvent::Message { id: 7, text: "Hello\tAlice".into(), error: false }
        );
        assert_eq!(session.event().await, prompt(1, "alice"));
        session.send(Input::Respond(Secret::from("secret")));
        assert_eq!(session.outcome().await, Outcome::Authenticated);
    }

    #[tokio::test]
    async fn cancelling_stops_the_helper() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = start(script(dir.path(), "helper", FAKE_HELPER), &["bob"], 0);
        assert_eq!(session.event().await, prompt(0, "bob"));
        session.send(Input::Cancel);
        assert_eq!(session.outcome().await, Outcome::Cancelled);
    }

    #[tokio::test]
    async fn a_line_break_in_the_response_counts_as_wrong() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = start(script(dir.path(), "helper", FAKE_HELPER), &["bob"], 0);
        assert_eq!(session.event().await, prompt(0, "bob"));
        session.send(Input::Respond(Secret::from("secret\nsecret")));
        assert_eq!(session.event().await, AuthenticationEvent::Failed { id: 7 });
        assert_eq!(session.event().await, prompt(0, "bob"));
        session.send(Input::Cancel);
        assert_eq!(session.outcome().await, Outcome::Cancelled);
    }

    #[tokio::test]
    async fn a_refusal_without_a_prompt_fails_for_good() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = start(script(dir.path(), "helper", REFUSING_HELPER), &["bob"], 0);
        assert_eq!(
            session.event().await,
            AuthenticationEvent::Message { id: 7, text: "Account locked".into(), error: true }
        );
        assert_eq!(session.outcome().await, Outcome::Failed("Account locked".into()));
    }

    #[tokio::test]
    async fn a_missing_helper_fails() {
        let session = start("/nonexistent/polkit-agent-helper-1".into(), &["bob"], 0);
        assert!(matches!(session.outcome().await, Outcome::Failed(_)));
    }
}
