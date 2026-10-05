// SPDX-License-Identifier: MIT

//! The system clipboard and primary selection, served by a worker thread.
//! Clipboard calls can wait on other clients, so they never run on the UI thread.

use std::sync::mpsc;

/// Which selection to use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardKind {
    /// The clipboard of Copy and Paste.
    Clipboard,
    /// The primary selection: the last selected text, pasted with the middle button.
    Selection,
}

type Reply = Box<dyn FnOnce(Option<String>) + Send>;

enum Request {
    Set(ClipboardKind, String),
    Get(ClipboardKind, Reply),
}

/// A handle to the clipboard worker. Without a clipboard service, reads give `None` and writes do nothing.
pub struct Clipboard {
    requests: Option<mpsc::Sender<Request>>,
}

#[cfg(target_os = "linux")]
fn linux_kind(kind: ClipboardKind) -> arboard::LinuxClipboardKind {
    match kind {
        ClipboardKind::Clipboard => arboard::LinuxClipboardKind::Clipboard,
        ClipboardKind::Selection => arboard::LinuxClipboardKind::Primary,
    }
}

fn serve(requests: &mpsc::Receiver<Request>) {
    let mut clipboard = match arboard::Clipboard::new() {
        Ok(clipboard) => Some(clipboard),
        Err(error) => {
            tracing::warn!("the clipboard is unavailable: {error}");
            None
        }
    };
    while let Ok(request) = requests.recv() {
        match (request, clipboard.as_mut()) {
            (Request::Set(kind, text), Some(clipboard)) => {
                #[cfg(target_os = "linux")]
                let result = {
                    use arboard::SetExtLinux as _;
                    clipboard.set().clipboard(linux_kind(kind)).text(text)
                };
                #[cfg(not(target_os = "linux"))]
                let result = match kind {
                    ClipboardKind::Clipboard => clipboard.set_text(text),
                    ClipboardKind::Selection => Ok(()),
                };
                if let Err(error) = result {
                    tracing::warn!("cannot copy to the clipboard: {error}");
                }
            }
            (Request::Get(kind, reply), Some(clipboard)) => {
                #[cfg(target_os = "linux")]
                let text = {
                    use arboard::GetExtLinux as _;
                    clipboard.get().clipboard(linux_kind(kind)).text()
                };
                #[cfg(not(target_os = "linux"))]
                let text = match kind {
                    ClipboardKind::Clipboard => clipboard.get_text(),
                    ClipboardKind::Selection => Err(arboard::Error::ContentNotAvailable),
                };
                reply(text.ok());
            }
            (Request::Get(_, reply), None) => reply(None),
            (Request::Set(..), None) => {}
        }
    }
}

impl Clipboard {
    /// Starts the worker thread.
    pub fn start() -> Self {
        let (tx, rx) = mpsc::channel();
        let started =
            std::thread::Builder::new().name("clipboard".into()).spawn(move || serve(&rx));
        match started {
            Ok(_) => Self { requests: Some(tx) },
            Err(error) => {
                tracing::warn!("cannot start the clipboard thread: {error}");
                Self { requests: None }
            }
        }
    }

    pub fn set(&self, kind: ClipboardKind, text: String) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(Request::Set(kind, text));
        }
    }

    /// Reads the text of `kind` and passes it to `reply` on the worker thread.
    pub fn get(&self, kind: ClipboardKind, reply: impl FnOnce(Option<String>) + Send + 'static) {
        let reply: Reply = Box::new(reply);
        match &self.requests {
            Some(requests) => {
                if let Err(mpsc::SendError(Request::Get(_, reply))) =
                    requests.send(Request::Get(kind, reply))
                {
                    reply(None);
                }
            }
            None => reply(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn requests_always_get_an_answer() {
        let clipboard = Clipboard::start();
        clipboard.set(ClipboardKind::Clipboard, "nimbus".into());
        let (tx, rx) = mpsc::channel();
        clipboard.get(ClipboardKind::Selection, move |text| {
            let _ = tx.send(text);
        });
        assert!(rx.recv_timeout(Duration::from_secs(10)).is_ok());
    }

    #[test]
    fn a_stopped_worker_answers_none() {
        let clipboard = Clipboard { requests: None };
        let (tx, rx) = mpsc::channel();
        clipboard.get(ClipboardKind::Clipboard, move |text| {
            let _ = tx.send(text);
        });
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).ok(), Some(None));
        clipboard.set(ClipboardKind::Clipboard, "ignored".into());
    }
}
