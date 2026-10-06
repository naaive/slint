// SPDX-License-Identifier: MIT

//! The polkit agent against a fake polkitd and logind on a private `dbus-daemon`,
//! with a script standing in for `polkit-agent-helper-1`.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nimbus_services::{
    AuthenticationCommand, AuthenticationEvent, BusAddress, Secret, ServiceCommand, ServiceEvent,
    Services, ServicesBuilder, ServicesConfig,
};
use nimbus_test_support::PrivateBus;
use nix::unistd::{Uid, User};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio::time::timeout;
use zbus::message::Header;
use zbus::zvariant::{OwnedValue, Value};
use zbus::{Connection, interface};

const WAIT: Duration = Duration::from_secs(10);
const AGENT_PATH: &str = "/org/freedesktop/PolicyKit1/AuthenticationAgent";
const AGENT_INTERFACE: &str = "org.freedesktop.PolicyKit1.AuthenticationAgent";

/// Checks the cookie, asks for one password, and succeeds if it's `secret`.
const FAKE_HELPER: &str = r#"#!/bin/sh
read -r cookie
case "$cookie" in cookie-*) ;; *) echo FAILURE; exit 1 ;; esac
echo "PAM_PROMPT_ECHO_OFF Password: "
read -r password || exit 1
if [ "$password" = "secret" ]; then echo SUCCESS; else echo FAILURE; fi
"#;

type Subject = (String, HashMap<String, OwnedValue>);

/// Who registered as an agent: their unique name, session id, and object path.
type Registration = (String, String, String);

#[derive(Clone, Default)]
struct FakeAuthority {
    registered: Arc<Mutex<Option<Registration>>>,
}

#[interface(name = "org.freedesktop.PolicyKit1.Authority")]
impl FakeAuthority {
    async fn register_authentication_agent(
        &self,
        subject: Subject,
        _locale: String,
        object_path: String,
        #[zbus(header)] header: Header<'_>,
    ) -> zbus::fdo::Result<()> {
        let session = match subject.1.get("session-id").map(|value| &**value) {
            Some(Value::Str(session)) if subject.0 == "unix-session" => session.to_string(),
            _ => return Err(zbus::fdo::Error::InvalidArgs("not a session subject".into())),
        };
        let sender = header.sender().map(|sender| sender.to_string()).unwrap_or_default();
        *self.registered.lock().unwrap() = Some((sender, session, object_path));
        Ok(())
    }

    async fn unregister_authentication_agent(
        &self,
        _subject: Subject,
        _object_path: String,
    ) -> zbus::fdo::Result<()> {
        Ok(())
    }
}

struct FakeSession;

#[interface(name = "org.freedesktop.login1.Session")]
impl FakeSession {
    #[zbus(property)]
    fn id(&self) -> String {
        "c7".into()
    }
}

fn helper_script(dir: &Path) -> PathBuf {
    let path = dir.join("polkit-agent-helper-1");
    std::fs::write(&path, FAKE_HELPER).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

struct Fixture {
    bus: PrivateBus,
    _dir: tempfile::TempDir,
    /// The connection of the fake polkitd and logind.
    daemon: Connection,
    agent: String,
    services: Services,
    events: UnboundedReceiver<ServiceEvent>,
}

impl Fixture {
    async fn start() -> Option<Self> {
        let bus = PrivateBus::start()?;
        let daemon = bus.connect().await;
        let authority = FakeAuthority::default();
        let server = daemon.object_server();
        server.at("/org/freedesktop/PolicyKit1/Authority", authority.clone()).await.unwrap();
        server.at("/org/freedesktop/login1/session/auto", FakeSession).await.unwrap();
        daemon.request_name("org.freedesktop.PolicyKit1").await.unwrap();
        daemon.request_name("org.freedesktop.login1").await.unwrap();

        let dir = tempfile::tempdir().unwrap();
        let (sender, events) = unbounded_channel();
        let services = ServicesBuilder::new(ServicesConfig {
            notifications: false,
            upower: false,
            network_manager: false,
            audio: false,
            backlight: false,
            mpris: false,
            bluetooth: false,
            logind: false,
            polkit: true,
        })
        .session_bus(BusAddress::Disabled)
        .system_bus(BusAddress::Address(bus.address.clone()))
        .polkit_helper(helper_script(dir.path()))
        .spawn(move |event| {
            let _ = sender.send(event);
        });

        let registration = timeout(WAIT, async {
            loop {
                if let Some(registration) = authority.registered.lock().unwrap().clone() {
                    return registration;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the agent registers");
        let (agent, session, path) = registration;
        assert_eq!(session, "c7");
        assert_eq!(path, AGENT_PATH);
        Some(Self { bus, _dir: dir, daemon, agent, services, events })
    }

    /// Calls `BeginAuthentication` from `caller` for the current user, without waiting for the reply.
    fn begin(
        &self,
        caller: &Connection,
        cookie: &str,
    ) -> tokio::task::JoinHandle<zbus::Result<zbus::Message>> {
        let caller = caller.clone();
        let agent = self.agent.clone();
        let cookie = cookie.to_owned();
        tokio::spawn(async move {
            let identity =
                ("unix-user", HashMap::from([("uid", Value::from(Uid::current().as_raw()))]));
            let body = (
                "org.example.reboot",
                "Authentication is required to reboot",
                "system-reboot",
                HashMap::<&str, &str>::new(),
                cookie.as_str(),
                vec![identity],
            );
            caller
                .call_method(
                    Some(agent.as_str()),
                    AGENT_PATH,
                    Some(AGENT_INTERFACE),
                    "BeginAuthentication",
                    &body,
                )
                .await
        })
    }

    async fn cancel(&self, cookie: &str) {
        self.daemon
            .call_method(
                Some(self.agent.as_str()),
                AGENT_PATH,
                Some(AGENT_INTERFACE),
                "CancelAuthentication",
                &(cookie,),
            )
            .await
            .expect("the agent cancels");
    }

    /// The next authentication event.
    async fn event(&mut self) -> AuthenticationEvent {
        loop {
            let event = timeout(WAIT, self.events.recv()).await.expect("an event in time");
            if let Some(ServiceEvent::Authentication(event)) = event {
                return event;
            }
        }
    }

    fn respond(&self, id: u32, response: &str) {
        let response = Secret::from(response);
        let command = AuthenticationCommand::Respond { id, response };
        self.services.send(ServiceCommand::Authentication(command));
    }
}

fn error_name(result: zbus::Result<zbus::Message>) -> String {
    match result {
        Err(zbus::Error::MethodError(name, _, _)) => name.to_string(),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn begin_authentication_completes_after_the_right_password() {
    let Some(mut f) = Fixture::start().await else { return };
    let call = f.begin(&f.daemon, "cookie-1");

    let AuthenticationEvent::Started(request) = f.event().await else {
        panic!("the request starts first");
    };
    let user = User::from_uid(Uid::current()).unwrap().unwrap().name;
    assert_eq!(request.identities, [user]);
    assert_eq!(request.selected, 0);
    assert_eq!(request.message, "Authentication is required to reboot");
    assert_eq!(request.icon_name, "system-reboot");
    assert_eq!(request.action_id, "org.example.reboot");
    let id = request.id;
    let prompt =
        AuthenticationEvent::Prompt { id, identity: 0, prompt: "Password: ".into(), echo: false };
    assert_eq!(f.event().await, prompt);

    f.respond(id, "wrong");
    assert_eq!(f.event().await, AuthenticationEvent::Failed { id });
    assert_eq!(f.event().await, prompt);
    f.respond(id, "secret");
    assert_eq!(f.event().await, AuthenticationEvent::Ended { id });
    timeout(WAIT, call).await.unwrap().unwrap().expect("BeginAuthentication succeeds");
}

#[tokio::test]
async fn polkitd_and_the_user_can_cancel() {
    let Some(mut f) = Fixture::start().await else { return };

    let call = f.begin(&f.daemon, "cookie-1");
    assert!(matches!(f.event().await, AuthenticationEvent::Started(_)));
    assert!(matches!(f.event().await, AuthenticationEvent::Prompt { .. }));
    f.cancel("cookie-1").await;
    assert!(matches!(f.event().await, AuthenticationEvent::Ended { .. }));
    let result = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(error_name(result), "org.freedesktop.PolicyKit1.Error.Cancelled");

    let call = f.begin(&f.daemon, "cookie-2");
    let AuthenticationEvent::Started(request) = f.event().await else {
        panic!("the request starts first");
    };
    let id = request.id;
    assert!(matches!(f.event().await, AuthenticationEvent::Prompt { .. }));
    f.services.send(ServiceCommand::Authentication(AuthenticationCommand::Cancel { id }));
    assert_eq!(f.event().await, AuthenticationEvent::Ended { id });
    let result = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(error_name(result), "org.freedesktop.PolicyKit1.Error.Cancelled");
}

#[tokio::test]
async fn only_polkitd_may_begin_authentication() {
    let Some(f) = Fixture::start().await else { return };
    let intruder = f.bus.connect().await;
    let result = timeout(WAIT, f.begin(&intruder, "cookie-1")).await.unwrap().unwrap();
    assert_eq!(error_name(result), "org.freedesktop.PolicyKit1.Error.NotAuthorized");
}
