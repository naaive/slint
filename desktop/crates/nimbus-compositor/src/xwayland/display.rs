// SPDX-License-Identifier: MIT

//! An X11 display number with its lock file and listening socket, laid out as X servers do.
//!
//! In `/tmp`, display `n` owns the lock file `.X<n>-lock`, holding the owner's process id,
//! the socket `.X11-unix/X<n>`, and an abstract socket of the same name.
//! There's no Xauthority cookie, since `xwayland-satellite` can't pass one to Xwayland.
//! Instead, only the user may connect to the socket file.
//! The abstract socket refuses connections, which sends clients on to the file,
//! and is bound so that no other process can listen there.

use socket2::{Domain, SockAddr, Socket, Type};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

const LAST_DISPLAY: u32 = 32;

/// Parses an X11 display name such as `:1` into its number.
pub fn parse_display(name: &str) -> Result<u32, String> {
    name.strip_prefix(':')
        .and_then(|number| number.parse().ok())
        .ok_or_else(|| format!("'{name}' isn't an X11 display such as ':1'"))
}

/// A display this process holds until it's dropped, which removes the socket and the lock file.
pub struct X11Display {
    number: u32,
    listener: UnixListener,
    _abstract_name: Socket,
    _socket: RemoveOnDrop,
    _lock: RemoveOnDrop,
}

impl X11Display {
    /// Takes `preferred` when it's free, or else the lowest free display, in `dir`, which is `/tmp` outside tests.
    pub fn allocate(dir: &Path, preferred: Option<u32>) -> io::Result<Self> {
        let socket_dir = dir.join(".X11-unix");
        create_socket_dir(&socket_dir)?;
        for number in preferred.into_iter().chain(0..=LAST_DISPLAY) {
            if let Some(display) = Self::take(dir, &socket_dir, number)? {
                return Ok(display);
            }
        }
        Err(io::Error::new(io::ErrorKind::AddrInUse, "every X11 display up to :32 is taken"))
    }

    /// Takes display `number`, or returns `None` when another process holds it.
    fn take(dir: &Path, socket_dir: &Path, number: u32) -> io::Result<Option<Self>> {
        let Some(lock) = take_lock(&dir.join(format!(".X{number}-lock")))? else {
            return Ok(None);
        };
        let path = socket_dir.join(format!("X{number}"));
        let abstract_name = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        let address = [b"\0", path.as_os_str().as_bytes()].concat();
        match abstract_name.bind(&SockAddr::unix(OsStr::from_bytes(&address))?) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::AddrInUse => return Ok(None),
            Err(err) => return Err(err),
        }
        // Holding the lock makes a socket file left behind stale, unless another user owns it.
        match remove_if_present(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
            Err(err) => return Err(err),
        }
        let socket = Socket::new(Domain::UNIX, Type::STREAM, None)?;
        socket.bind(&SockAddr::unix(&path)?)?;
        let socket_file = RemoveOnDrop(path);
        fs::set_permissions(&socket_file.0, fs::Permissions::from_mode(0o600))?;
        socket.listen(libc::SOMAXCONN)?;
        Ok(Some(Self {
            number,
            listener: UnixListener::from(std::os::fd::OwnedFd::from(socket)),
            _abstract_name: abstract_name,
            _socket: socket_file,
            _lock: lock,
        }))
    }

    /// The name for `DISPLAY`, such as `:1`.
    pub fn name(&self) -> String {
        format!(":{}", self.number)
    }

    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub fn raw_fd(&self) -> RawFd {
        self.listener.as_raw_fd()
    }
}

/// Creates the shared socket directory, sticky and writable by everyone, as X servers expect it,
/// or checks that an existing one is such a directory, belonging to root or this user.
fn create_socket_dir(dir: &Path) -> io::Result<()> {
    match fs::create_dir(dir) {
        Ok(()) => return fs::set_permissions(dir, fs::Permissions::from_mode(0o1777)),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err),
    }
    let metadata = fs::symlink_metadata(dir)?;
    // SAFETY: `geteuid` has no preconditions.
    let owned = metadata.uid() == 0 || metadata.uid() == unsafe { libc::geteuid() };
    let shared_without_sticky_bit = metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0;
    if !metadata.is_dir() || !owned || shared_without_sticky_bit {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} isn't a directory of root or this user, sticky when others may write to it",
                dir.display()
            ),
        ));
    }
    Ok(())
}

/// Creates the lock file `path` with this process's id, replacing a stale one.
fn take_lock(path: &Path) -> io::Result<Option<RemoveOnDrop>> {
    // A second attempt follows the removal of a stale lock; losing that race to another server means it's taken.
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).mode(0o444).open(path) {
            Ok(mut file) => {
                let lock = RemoveOnDrop(path.to_path_buf());
                writeln!(file, "{:>10}", std::process::id())?;
                return Ok(Some(lock));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if !is_stale(path) {
                    return Ok(None);
                }
                tracing::info!("removing the stale X11 lock file {}", path.display());
                match remove_if_present(path) {
                    Err(err) if err.kind() == io::ErrorKind::PermissionDenied => return Ok(None),
                    result => result?,
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(None)
}

/// Whether the lock file at `path` names a process that no longer exists.
///
/// An unreadable lock file counts as live, since its owner may still be writing it.
fn is_stale(path: &Path) -> bool {
    let Some(pid) = fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
        .filter(|&pid| pid > 0)
    else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0
        || io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    !alive
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Err(err) = remove_if_present(&self.0) {
            tracing::warn!("cannot remove {}: {err}", self.0.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixStream};

    fn abstract_address(socket: &Path) -> SocketAddr {
        SocketAddr::from_abstract_name(socket.as_os_str().as_bytes()).unwrap()
    }

    #[test]
    fn allocates_the_lowest_free_display_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let display = X11Display::allocate(dir.path(), None).unwrap();
        assert_eq!(display.name(), ":0");
        let lock = dir.path().join(".X0-lock");
        let socket = dir.path().join(".X11-unix/X0");
        assert_eq!(fs::read_to_string(&lock).unwrap(), format!("{:>10}\n", std::process::id()));
        let mode = fs::metadata(dir.path().join(".X11-unix")).unwrap().permissions().mode();
        assert_eq!(mode & 0o7777, 0o1777);

        UnixStream::connect(&socket).unwrap();
        assert_eq!(fs::metadata(&socket).unwrap().permissions().mode() & 0o777, 0o600);
        let address = abstract_address(&socket);
        let refused = UnixStream::connect_addr(&address).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::ConnectionRefused);

        let second = X11Display::allocate(dir.path(), None).unwrap();
        assert_eq!(second.name(), ":1");

        drop(display);
        assert!(!lock.exists() && !socket.exists());
        // A child that another test forks holds a copy of the abstract socket until it execs.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while UnixListener::bind_addr(&address).is_err() {
            assert!(std::time::Instant::now() < deadline, "the abstract name stays taken");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(dir.path().join(".X1-lock").exists());
    }

    #[test]
    fn a_listener_on_the_abstract_name_takes_the_display() {
        let dir = tempfile::tempdir().unwrap();
        let other = UnixListener::bind_addr(&abstract_address(&dir.path().join(".X11-unix/X0")));
        let _other = other.unwrap();
        assert_eq!(X11Display::allocate(dir.path(), None).unwrap().name(), ":1");
    }

    #[test]
    fn refuses_unsafe_socket_directories() {
        let dir = tempfile::tempdir().unwrap();
        let socket_dir = dir.path().join(".X11-unix");
        fs::create_dir(&socket_dir).unwrap();
        fs::set_permissions(&socket_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let err = X11Display::allocate(dir.path(), None).err().expect("a shared directory");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

        fs::remove_dir(&socket_dir).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), &socket_dir).unwrap();
        let err = X11Display::allocate(dir.path(), None).err().expect("a symbolic link");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn takes_the_preferred_display_when_free() {
        let dir = tempfile::tempdir().unwrap();
        let display = X11Display::allocate(dir.path(), Some(5)).unwrap();
        assert_eq!(display.name(), ":5");
        let fallback = X11Display::allocate(dir.path(), Some(5)).unwrap();
        assert_eq!(fallback.name(), ":0");
    }

    #[test]
    fn skips_live_locks_and_replaces_stale_ones() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".X11-unix")).unwrap();
        // Display 0 belongs to a live process, this one; display 1 to a process that exited.
        fs::write(dir.path().join(".X0-lock"), format!("{:>10}\n", std::process::id())).unwrap();
        let mut exited = std::process::Command::new("true").spawn().unwrap();
        let exited_pid = exited.id();
        exited.wait().unwrap();
        fs::write(dir.path().join(".X1-lock"), format!("{exited_pid:>10}\n")).unwrap();
        fs::write(dir.path().join(".X11-unix/X1"), "").unwrap();

        let display = X11Display::allocate(dir.path(), None).unwrap();
        assert_eq!(display.name(), ":1");
        assert!(dir.path().join(".X0-lock").exists());
        UnixStream::connect(dir.path().join(".X11-unix/X1")).unwrap();
    }

    #[test]
    fn unreadable_locks_count_as_taken() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".X0-lock"), "").unwrap();
        assert_eq!(X11Display::allocate(dir.path(), None).unwrap().name(), ":1");
    }

    #[test]
    fn display_names_parse() {
        assert_eq!(parse_display(":3"), Ok(3));
        assert!(parse_display("3").is_err());
        assert!(parse_display(":x").is_err());
    }
}
