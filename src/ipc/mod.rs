//! Single-instance control and handing a file path to the running viewer
//! (spec §3).
//!
//! The observable behaviour is identical on both platforms: the first process
//! owns the only window, and every later launch forwards its argument to that
//! window and exits. The mechanism differs — a lock file plus a Unix domain
//! socket on Linux, a named mutex plus a named pipe on Windows — because each
//! is the natural primitive there.

use std::path::{Path, PathBuf};

use anyhow::Result;

#[cfg_attr(unix, path = "unix.rs")]
#[cfg_attr(windows, path = "windows.rs")]
mod platform;

/// The role this process takes after trying to become the single instance.
pub enum Instance {
    /// This process owns the window and listens for paths from later launches.
    Primary(Primary),
    /// A viewer is already running; hand it the path and exit.
    Secondary(Secondary),
}

/// The listening side, held by the process that owns the window.
pub struct Primary(platform::Primary);

/// The sending side, held by a process that is about to exit.
pub struct Secondary(platform::Secondary);

/// Decides whether this process is the primary instance.
pub fn acquire() -> Result<Instance> {
    Ok(match platform::acquire()? {
        platform::Role::Primary(primary) => Instance::Primary(Primary(primary)),
        platform::Role::Secondary(secondary) => Instance::Secondary(Secondary(secondary)),
    })
}

impl Primary {
    /// Starts listening in the background. `on_path` runs on a worker thread
    /// for every launch request that reaches this process; `None` means the
    /// other process was started without a file argument.
    pub fn serve(self, on_path: impl Fn(Option<PathBuf>) + Send + 'static) {
        self.0.serve(on_path);
    }
}

impl Secondary {
    /// Sends `path` to the running viewer.
    pub fn send(&self, path: Option<&Path>) -> Result<()> {
        self.0.send(path)
    }
}
