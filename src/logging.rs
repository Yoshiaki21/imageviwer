//! Append-only logging to `viewer.log` next to the executable (spec §10).
//!
//! There is no rotation by design; the file is meant to be deleted by hand.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

use crate::app_paths;

/// The crate's own log target prefix, used to keep GPUI's very chatty debug
/// output out of `viewer.log`.
const OWN_TARGET: &str = "imageviewer";

static LOGGER: OnceLock<FileLogger> = OnceLock::new();

struct FileLogger {
    file: Mutex<Option<File>>,
    verbose: bool,
}

impl Log for FileLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        match metadata.level() {
            // Problems are always worth recording, whoever reported them.
            Level::Error | Level::Warn => true,
            // Detail is only useful from this crate; GPUI's own debug output
            // would bury it.
            _ => self.verbose && metadata.target().starts_with(OWN_TARGET),
        }
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        // GPUI's own helpers log with an empty target, so fall back to the
        // source file to keep every line traceable.
        let origin = match record.target() {
            "" => record.file().unwrap_or("unknown"),
            target => target,
        };

        let line = format!(
            "{} [{:<5}] {}{} - {}\n",
            timestamp(),
            record.level(),
            origin,
            record
                .line()
                .map(|line| format!(":{line}"))
                .unwrap_or_default(),
            record.args()
        );

        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.write_all(line.as_bytes());
                let _ = file.flush();
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// Installs the logger. Safe to call once; later calls are ignored.
///
/// `verbose` follows spec §10: debug builds are always detailed, release
/// builds only when `config.toml` asks for it.
pub fn init(enabled: bool, verbose: bool) {
    let path = app_paths::log_path();
    let file = if enabled { open_log(&path) } else { None };

    let logger = LOGGER.get_or_init(|| FileLogger {
        file: Mutex::new(file),
        verbose,
    });

    let level = if !enabled {
        LevelFilter::Off
    } else if verbose {
        LevelFilter::Debug
    } else {
        LevelFilter::Warn
    };

    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}

fn open_log(path: &Path) -> Option<File> {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(file) => Some(file),
        Err(error) => {
            // Not being able to log is not a reason to refuse to show images,
            // and printing here would panic in a GUI-subsystem process. Record
            // the reason so any later error dialog can mention it.
            crate::report::set_log_unavailable(format!("{}: {error}", path.display()));
            None
        }
    }
}

/// Local time, formatted for a log line.
fn timestamp() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%S%.3f")
        .to_string()
}
