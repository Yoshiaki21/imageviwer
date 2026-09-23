//! Linux/Unix single-instance control.
//!
//! A lock file decides who is primary — `flock` is released by the kernel even
//! if the process is killed, so a crashed viewer never blocks the next launch.
//! The winner then binds a Unix domain socket; later launches connect to it,
//! write the path and exit.

use std::fs::{File, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

pub enum Role {
    Primary(Primary),
    Secondary(Secondary),
}

pub struct Primary {
    /// Held open for the lifetime of the process: dropping it releases the
    /// lock and would let a second viewer start.
    lock: Option<File>,
    listener: UnixListener,
    socket_path: PathBuf,
}

pub struct Secondary {
    socket_path: PathBuf,
}

pub fn acquire() -> Result<Role> {
    let socket_path = socket_path();
    let lock_path = socket_path.with_extension("lock");

    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("opening lock file {}", lock_path.display()))?;

    match lock.try_lock() {
        Ok(()) => Ok(Role::Primary(bind(Some(lock), socket_path)?)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(Role::Secondary(Secondary { socket_path })),
        Err(std::fs::TryLockError::Error(error)) => {
            // Some filesystems cannot lock. Fall back to probing the socket:
            // if something answers, it is the running viewer.
            log::warn!("cannot lock {}: {error}", lock_path.display());
            if UnixStream::connect(&socket_path).is_ok() {
                Ok(Role::Secondary(Secondary { socket_path }))
            } else {
                Ok(Role::Primary(bind(None, socket_path)?))
            }
        }
    }
}

/// Binds the socket, clearing the stale one a previous run may have left.
fn bind(lock: Option<File>, socket_path: PathBuf) -> Result<Primary> {
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("binding {}", socket_path.display()))?;
    Ok(Primary {
        lock,
        listener,
        socket_path,
    })
}

impl Primary {
    pub fn serve(self, on_path: impl Fn(Option<PathBuf>) + Send + 'static) {
        let Primary {
            lock,
            listener,
            socket_path,
        } = self;

        std::thread::Builder::new()
            .name("imageviewer-ipc".into())
            .spawn(move || {
                // Moved into the thread so the lock and socket outlive `serve`.
                let _lock = lock;
                let _cleanup = SocketCleanup(socket_path);

                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => match read_request(stream) {
                            Ok(path) => on_path(path),
                            Err(error) => log::error!("bad launch request: {error}"),
                        },
                        Err(error) => log::error!("accept failed: {error}"),
                    }
                }
            })
            .expect("spawning the IPC listener thread");
    }
}

impl Secondary {
    pub fn send(&self, path: Option<&Path>) -> Result<()> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .with_context(|| format!("connecting to {}", self.socket_path.display()))?;
        if let Some(path) = path {
            stream
                .write_all(path.as_os_str().as_bytes())
                .context("sending the path")?;
        }
        stream.flush().context("flushing the path")?;
        // Closing the write half is what tells the primary the message ended.
        stream
            .shutdown(std::net::Shutdown::Write)
            .context("closing the request")?;
        Ok(())
    }
}

/// One connection carries exactly one request: the path's raw bytes, or
/// nothing at all when the other process had no file argument.
fn read_request(mut stream: UnixStream) -> Result<Option<PathBuf>> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).context("reading request")?;
    if bytes.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(bytes))))
}

/// Removes the socket file when the listener thread ends.
struct SocketCleanup(PathBuf);

impl Drop for SocketCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `$XDG_RUNTIME_DIR/imageviewer-<user>.sock`, falling back to the temp
/// directory. The user name keeps two accounts on one machine apart when the
/// fallback is used.
fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|dir| dir.is_dir())
        .unwrap_or_else(std::env::temp_dir);

    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "default".to_string());

    dir.join(format!("imageviewer-{user}.sock"))
}
