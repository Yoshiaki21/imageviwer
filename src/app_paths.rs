//! Locations of the files the viewer owns.
//!
//! `config.toml` and `viewer.log` both live next to the executable (spec §9/§10),
//! so that a copied program directory carries its settings and log with it.

use std::path::PathBuf;

pub const CONFIG_FILE_NAME: &str = "config.toml";
pub const LOG_FILE_NAME: &str = "viewer.log";

/// Directory holding the running executable, falling back to the working
/// directory when the executable path cannot be resolved.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_path() -> PathBuf {
    exe_dir().join(CONFIG_FILE_NAME)
}

pub fn log_path() -> PathBuf {
    exe_dir().join(LOG_FILE_NAME)
}
