// SPDX-License-Identifier: MIT

//! Checks lock screen passwords through PAM on a worker thread, since PAM modules block and may sleep on failure.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::sync::mpsc;
use std::thread::JoinHandle;

use zeroize::Zeroizing;

/// The PAM service shipped in `data/pam.d/nimbus`.
pub const SERVICE: &str = "nimbus";
/// The service used when `nimbus` isn't installed; every distribution has it.
pub const FALLBACK_SERVICE: &str = "login";

/// `nimbus` when its PAM configuration is installed with an account section, otherwise `login`.
/// Without an account section, `pam_acct_mgmt` falls through to the `other` service, which usually denies.
pub fn service_name() -> &'static str {
    let installed = ["/etc/pam.d", "/usr/lib/pam.d", "/usr/local/etc/pam.d"].iter().any(|dir| {
        std::fs::read_to_string(Path::new(dir).join(SERVICE)).is_ok_and(|c| has_account_section(&c))
    });
    if installed { SERVICE } else { FALLBACK_SERVICE }
}

/// Whether a PAM service file configures the account management group, directly or through an include.
fn has_account_section(config: &str) -> bool {
    config.lines().map(str::trim_start).any(|line| {
        let first = line.split_whitespace().next().unwrap_or_default();
        matches!(first.trim_start_matches('-'), "account" | "@include")
    })
}

/// The login name of the user running the shell.
pub fn current_user() -> Option<String> {
    match nix::unistd::User::from_uid(nix::unistd::Uid::current()) {
        Ok(Some(user)) => Some(user.name),
        Ok(None) => std::env::var("USER").ok().filter(|u| !u.is_empty()),
        Err(err) => {
            tracing::warn!("cannot look up the current user: {err}");
            std::env::var("USER").ok().filter(|u| !u.is_empty())
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authentication failed: {0}")]
    Rejected(String),
    #[error("PAM is unavailable: {0}")]
    Unavailable(String),
}

/// Checks a user's password.
pub trait Authenticator: Send + 'static {
    fn authenticate(&self, user: &str, password: &str) -> Result<(), AuthError>;
}

/// Answers PAM's prompts with the user name and the typed password.
struct Credentials {
    user: String,
    password: Zeroizing<String>,
}

impl Credentials {
    /// The reply to one PAM message: `Ok(None)` for messages that only inform, `Err` when a reply can't be built.
    fn reply(&self, style: c_int, message: &CStr) -> Result<Option<CString>, ()> {
        match style {
            ffi::PAM_PROMPT_ECHO_ON => CString::new(self.user.as_str()).map(Some).map_err(|_| ()),
            ffi::PAM_PROMPT_ECHO_OFF => {
                CString::new(self.password.as_str()).map(Some).map_err(|_| ())
            }
            ffi::PAM_ERROR_MSG => {
                tracing::info!("PAM: {}", message.to_string_lossy());
                Ok(None)
            }
            ffi::PAM_TEXT_INFO => {
                tracing::debug!("PAM: {}", message.to_string_lossy());
                Ok(None)
            }
            _ => Err(()),
        }
    }
}

/// Bindings for the parts of Linux-PAM (`<security/pam_appl.h>`) used here.
mod ffi {
    use std::ffi::{c_char, c_int, c_void};

    pub const PAM_SUCCESS: c_int = 0;
    pub const PAM_BUF_ERR: c_int = 5;
    pub const PAM_CONV_ERR: c_int = 19;
    pub const PAM_NEW_AUTHTOK_REQD: c_int = 12;
    pub const PAM_PROMPT_ECHO_OFF: c_int = 1;
    pub const PAM_PROMPT_ECHO_ON: c_int = 2;
    pub const PAM_ERROR_MSG: c_int = 3;
    pub const PAM_TEXT_INFO: c_int = 4;
    pub const PAM_SILENT: c_int = 0x8000;
    pub const PAM_DISALLOW_NULL_AUTHTOK: c_int = 0x0001;
    pub const PAM_REINITIALIZE_CRED: c_int = 0x0008;

    #[repr(C)]
    pub struct PamHandle {
        _private: [u8; 0],
    }

    #[repr(C)]
    pub struct PamMessage {
        pub msg_style: c_int,
        pub msg: *const c_char,
    }

    #[repr(C)]
    pub struct PamResponse {
        pub resp: *mut c_char,
        pub resp_retcode: c_int,
    }

    pub type ConvFn = unsafe extern "C" fn(
        num_msg: c_int,
        msg: *mut *const PamMessage,
        resp: *mut *mut PamResponse,
        appdata_ptr: *mut c_void,
    ) -> c_int;

    #[repr(C)]
    pub struct PamConv {
        pub conv: Option<ConvFn>,
        pub appdata_ptr: *mut c_void,
    }

    #[link(name = "pam")]
    unsafe extern "C" {
        pub fn pam_start(
            service_name: *const c_char,
            user: *const c_char,
            pam_conversation: *const PamConv,
            pamh: *mut *mut PamHandle,
        ) -> c_int;
        pub fn pam_authenticate(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_acct_mgmt(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_setcred(pamh: *mut PamHandle, flags: c_int) -> c_int;
        pub fn pam_end(pamh: *mut PamHandle, pam_status: c_int) -> c_int;
        pub fn pam_strerror(pamh: *mut PamHandle, errnum: c_int) -> *const c_char;
    }
}

/// The conversation callback PAM calls for prompts and messages.
///
/// # Safety
///
/// PAM passes `num_msg` valid message pointers and an `appdata_ptr` that is the `Credentials` given to `pam_start`.
/// Linux-PAM frees the response array and each response string with `free`, so both come from `malloc`.
unsafe extern "C" fn conversation(
    num_msg: c_int,
    msg: *mut *const ffi::PamMessage,
    resp: *mut *mut ffi::PamResponse,
    appdata_ptr: *mut c_void,
) -> c_int {
    let Ok(count) = usize::try_from(num_msg) else {
        return ffi::PAM_CONV_ERR;
    };
    if count == 0 || msg.is_null() || resp.is_null() || appdata_ptr.is_null() {
        return ffi::PAM_CONV_ERR;
    }
    // SAFETY: `appdata_ptr` is the `Credentials` that outlives the PAM transaction; see `PamAuthenticator::authenticate`.
    let credentials = unsafe { &*(appdata_ptr as *const Credentials) };
    // SAFETY: calloc returns zeroed memory for `count` responses, or null.
    let responses = unsafe { libc::calloc(count, std::mem::size_of::<ffi::PamResponse>()) }
        as *mut ffi::PamResponse;
    if responses.is_null() {
        return ffi::PAM_BUF_ERR;
    }
    for i in 0..count {
        // SAFETY: PAM provides `count` message pointers.
        let message = unsafe { *msg.add(i) };
        let reply = if message.is_null() {
            Err(())
        } else {
            // SAFETY: a non-null message points to a valid `pam_message` with a C string or null.
            let (style, text) = unsafe { ((*message).msg_style, (*message).msg) };
            let text = if text.is_null() { c"" } else { unsafe { CStr::from_ptr(text) } };
            credentials.reply(style, text)
        };
        match reply {
            Ok(Some(reply)) => {
                // SAFETY: strdup copies the NUL-terminated reply into malloc'd memory, which PAM frees.
                let copy = unsafe { libc::strdup(reply.as_ptr()) };
                let mut bytes = reply.into_bytes();
                zeroize::Zeroize::zeroize(&mut bytes);
                if copy.is_null() {
                    // SAFETY: frees what this call allocated so far.
                    unsafe { free_responses(responses, i) };
                    return ffi::PAM_BUF_ERR;
                }
                // SAFETY: `i < count`, inside the calloc'd array.
                unsafe { (*responses.add(i)).resp = copy };
            }
            Ok(None) => {}
            Err(()) => {
                // SAFETY: frees what this call allocated so far.
                unsafe { free_responses(responses, i) };
                return ffi::PAM_CONV_ERR;
            }
        }
    }
    // SAFETY: `resp` is a valid out pointer from PAM.
    unsafe { *resp = responses };
    ffi::PAM_SUCCESS
}

/// Frees the first `filled` response strings, wiping them, and the array itself.
///
/// # Safety
///
/// `responses` must be a calloc'd array of at least `filled` responses whose strings are null or malloc'd.
unsafe fn free_responses(responses: *mut ffi::PamResponse, filled: usize) {
    for i in 0..filled {
        // SAFETY: guaranteed by the caller.
        let text = unsafe { (*responses.add(i)).resp };
        if !text.is_null() {
            // SAFETY: `text` is a NUL-terminated malloc'd string.
            unsafe {
                let len = libc::strlen(text);
                std::ptr::write_bytes(text, 0, len);
                libc::free(text.cast());
            }
        }
    }
    // SAFETY: `responses` came from calloc.
    unsafe { libc::free(responses.cast()) };
}

/// Authenticates against a PAM service.
#[derive(Clone, Debug)]
pub struct PamAuthenticator {
    service: String,
}

impl PamAuthenticator {
    pub fn new(service: impl Into<String>) -> Self {
        Self { service: service.into() }
    }
}

fn pam_error(handle: *mut ffi::PamHandle, code: c_int) -> String {
    // SAFETY: pam_strerror accepts any handle, including null, and returns a static string or null.
    let text: *const c_char = unsafe { ffi::pam_strerror(handle, code) };
    if text.is_null() {
        format!("PAM error {code}")
    } else {
        // SAFETY: a non-null result is a NUL-terminated static string.
        unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
    }
}

impl Authenticator for PamAuthenticator {
    fn authenticate(&self, user: &str, password: &str) -> Result<(), AuthError> {
        let service = CString::new(self.service.as_str())
            .map_err(|_| AuthError::Unavailable("bad service name".into()))?;
        let login = CString::new(user).map_err(|_| AuthError::Rejected("bad user name".into()))?;
        let credentials =
            Credentials { user: user.to_owned(), password: Zeroizing::new(password.to_owned()) };
        let conv = ffi::PamConv {
            conv: Some(conversation),
            appdata_ptr: &credentials as *const Credentials as *mut c_void,
        };
        let mut handle: *mut ffi::PamHandle = std::ptr::null_mut();
        // SAFETY: the strings and `conv` live until pam_end below; PAM copies the conversation struct.
        let status =
            unsafe { ffi::pam_start(service.as_ptr(), login.as_ptr(), &conv, &mut handle) };
        if status != ffi::PAM_SUCCESS || handle.is_null() {
            return Err(AuthError::Unavailable(pam_error(handle, status)));
        }
        // SAFETY: `handle` is a live transaction until pam_end.
        let mut status = unsafe {
            ffi::pam_authenticate(handle, ffi::PAM_SILENT | ffi::PAM_DISALLOW_NULL_AUTHTOK)
        };
        if status == ffi::PAM_SUCCESS {
            // Applies account policies such as expiry, pam_access, and pam_time.
            // SAFETY: as above.
            status = unsafe { ffi::pam_acct_mgmt(handle, ffi::PAM_SILENT) };
            if status == ffi::PAM_NEW_AUTHTOK_REQD {
                // A lock screen can't run a password change; the session already belongs to this user.
                tracing::info!("PAM asks for a new password; unlocking anyway");
                status = ffi::PAM_SUCCESS;
            }
        }
        let result = if status == ffi::PAM_SUCCESS {
            // Refreshes credentials such as Kerberos tickets; a failure doesn't undo a correct password.
            // SAFETY: as above.
            let refreshed =
                unsafe { ffi::pam_setcred(handle, ffi::PAM_SILENT | ffi::PAM_REINITIALIZE_CRED) };
            if refreshed != ffi::PAM_SUCCESS {
                tracing::debug!(
                    "PAM could not refresh credentials: {}",
                    pam_error(handle, refreshed)
                );
            }
            Ok(())
        } else {
            let message = pam_error(handle, status);
            Err(AuthError::Rejected(message))
        };
        if result.is_ok() {
            status = ffi::PAM_SUCCESS;
        }
        // SAFETY: ends the transaction started above; `handle` isn't used afterwards.
        unsafe { ffi::pam_end(handle, status) };
        result
    }
}

/// The authentication thread; dropping it stops the thread once any check in progress ends.
pub struct AuthWorker {
    passwords: Option<mpsc::Sender<Zeroizing<String>>>,
    thread: Option<JoinHandle<()>>,
}

impl AuthWorker {
    /// Starts a thread that checks each submitted password for `user` and reports whether it was right.
    pub fn spawn(
        authenticator: Box<dyn Authenticator>,
        user: Option<String>,
        on_result: impl Fn(bool) + Send + 'static,
    ) -> std::io::Result<Self> {
        let (passwords, receiver) = mpsc::channel::<Zeroizing<String>>();
        let thread = std::thread::Builder::new().name("nimbus-auth".into()).spawn(move || {
            while let Ok(password) = receiver.recv() {
                let ok = match &user {
                    Some(user) => match authenticator.authenticate(user, &password) {
                        Ok(()) => true,
                        Err(err) => {
                            tracing::info!("unlocking failed: {err}");
                            false
                        }
                    },
                    None => {
                        tracing::warn!("cannot unlock: the current user is unknown");
                        false
                    }
                };
                on_result(ok);
            }
        })?;
        Ok(Self { passwords: Some(passwords), thread: Some(thread) })
    }

    /// Queues a password for checking; the result arrives through the `on_result` callback.
    pub fn submitter(&self) -> impl Fn(String) + 'static {
        let passwords = self.passwords.clone();
        move |password| {
            let password = Zeroizing::new(password);
            if passwords.as_ref().is_none_or(|p| p.send(password).is_err()) {
                tracing::warn!("the authentication thread has stopped");
            }
        }
    }
}

impl Drop for AuthWorker {
    fn drop(&mut self) {
        self.passwords = None;
        // A PAM module can block for a long time; don't hold up shutdown for it.
        drop(self.thread.take());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct Fixed(&'static str);

    impl Authenticator for Fixed {
        fn authenticate(&self, user: &str, password: &str) -> Result<(), AuthError> {
            if user == "ada" && password == self.0 {
                Ok(())
            } else {
                Err(AuthError::Rejected("wrong password".into()))
            }
        }
    }

    fn results(worker_user: Option<&str>, passwords: &[&str]) -> Vec<bool> {
        let (tx, rx) = mpsc::channel();
        let worker = AuthWorker::spawn(
            Box::new(Fixed("secret")),
            worker_user.map(String::from),
            move |ok| {
                let _ = tx.send(ok);
            },
        )
        .unwrap();
        let submit = worker.submitter();
        for password in passwords {
            submit((*password).to_owned());
        }
        let results = (0..passwords.len())
            .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        drop(worker);
        results
    }

    #[test]
    fn reports_each_attempt_in_order() {
        assert_eq!(results(Some("ada"), &["nope", "secret", ""]), vec![false, true, false]);
    }

    #[test]
    fn unknown_user_never_unlocks() {
        assert_eq!(results(None, &["secret"]), vec![false]);
    }

    #[test]
    fn credentials_answer_prompts() {
        let credentials = Credentials { user: "ada".into(), password: Zeroizing::new("pw".into()) };
        let reply = |style| credentials.reply(style, c"prompt");
        assert_eq!(reply(ffi::PAM_PROMPT_ECHO_ON).unwrap().unwrap().as_bytes(), b"ada");
        assert_eq!(reply(ffi::PAM_PROMPT_ECHO_OFF).unwrap().unwrap().as_bytes(), b"pw");
        assert_eq!(reply(ffi::PAM_TEXT_INFO), Ok(None));
        assert_eq!(reply(ffi::PAM_ERROR_MSG), Ok(None));
        assert!(reply(99).is_err());
        let bad = Credentials { user: "a\0b".into(), password: Zeroizing::new("p\0".into()) };
        assert!(bad.reply(ffi::PAM_PROMPT_ECHO_ON, c"login:").is_err());
        assert!(bad.reply(ffi::PAM_PROMPT_ECHO_OFF, c"Password:").is_err());
    }

    #[test]
    fn conversation_answers_through_the_c_interface() {
        let credentials = Credentials { user: "ada".into(), password: Zeroizing::new("pw".into()) };
        let messages = [
            ffi::PamMessage { msg_style: ffi::PAM_PROMPT_ECHO_ON, msg: c"login:".as_ptr() },
            ffi::PamMessage { msg_style: ffi::PAM_TEXT_INFO, msg: c"hello".as_ptr() },
            ffi::PamMessage { msg_style: ffi::PAM_PROMPT_ECHO_OFF, msg: c"Password:".as_ptr() },
        ];
        let mut pointers: Vec<*const ffi::PamMessage> =
            messages.iter().map(|m| m as *const _).collect();
        let mut responses: *mut ffi::PamResponse = std::ptr::null_mut();
        let appdata = &credentials as *const Credentials as *mut c_void;
        // SAFETY: valid messages and out pointer, as PAM would pass them.
        let status = unsafe { conversation(3, pointers.as_mut_ptr(), &mut responses, appdata) };
        assert_eq!(status, ffi::PAM_SUCCESS);
        // SAFETY: the conversation returned three responses.
        unsafe {
            assert_eq!(CStr::from_ptr((*responses).resp).to_bytes(), b"ada");
            assert!((*responses.add(1)).resp.is_null());
            assert_eq!(CStr::from_ptr((*responses.add(2)).resp).to_bytes(), b"pw");
            free_responses(responses, 3);
        }
        let mut bad = vec![&ffi::PamMessage { msg_style: 42, msg: std::ptr::null() } as *const _];
        // SAFETY: as above; the unknown style makes the conversation fail without leaking.
        let status = unsafe { conversation(1, bad.as_mut_ptr(), &mut responses, appdata) };
        assert_eq!(status, ffi::PAM_CONV_ERR);
        // SAFETY: a zero count is rejected before any pointer is read.
        assert_eq!(
            unsafe { conversation(0, bad.as_mut_ptr(), &mut responses, appdata) },
            ffi::PAM_CONV_ERR
        );
    }

    #[test]
    fn account_sections_are_recognized() {
        assert!(!has_account_section("# comment\nauth include login\n"));
        assert!(has_account_section("auth include login\naccount include login\n"));
        assert!(has_account_section("-account optional pam_foo.so\n"));
        assert!(has_account_section("@include common-account\n"));
        assert!(!has_account_section("# account include login\n"));
    }

    #[test]
    fn shipped_service_checks_accounts() {
        let shipped = include_str!("../../../data/pam.d/nimbus");
        assert!(has_account_section(shipped));
        assert!(shipped.lines().any(|line| line.split_whitespace().next() == Some("auth")));
    }

    #[test]
    fn service_falls_back_to_login() {
        let service = service_name();
        assert!(service == SERVICE || service == FALLBACK_SERVICE);
        assert!(current_user().is_some());
    }

    #[test]
    fn pam_rejects_a_wrong_password() {
        // Runs the real PAM stack; a wrong password must never authenticate, whatever the system's configuration.
        let user = current_user().unwrap();
        let result = PamAuthenticator::new(service_name())
            .authenticate(&user, "definitely-not-the-password\u{1}");
        assert!(result.is_err());
    }
}
