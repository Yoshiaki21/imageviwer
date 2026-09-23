//! `config.toml` next to the executable (spec §9).
//!
//! The file is written by hand rather than through `toml::to_string`, because
//! spec §10 requires the `verbose` key to ship as a commented-out sample with
//! its explanation, and a serializer cannot emit comments.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::app_paths;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub window: Option<WindowConfig>,
    #[serde(default)]
    pub last_opened: Option<LastOpened>,
    #[serde(default)]
    pub overlay: OverlayConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Position and size of the window at the previous exit, in screen pixels.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The folder shown at the previous exit and the position within it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastOpened {
    pub folder: PathBuf,
    pub index: usize,
}

/// The folder / file caption shown when the image changes.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverlayConfig {
    /// How long the caption stays up, in milliseconds. 0 turns it off.
    #[serde(default = "default_overlay_duration_ms")]
    pub duration_ms: u64,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            duration_ms: default_overlay_duration_ms(),
        }
    }
}

impl OverlayConfig {
    pub fn duration(&self) -> Duration {
        Duration::from_millis(self.duration_ms)
    }
}

fn default_overlay_duration_ms() -> u64 {
    5000
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    /// Whether to write `viewer.log` at all. Defaults to on.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Detailed logging in release builds. Debug builds are always detailed.
    #[serde(default)]
    pub verbose: Option<bool>,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            verbose: None,
        }
    }
}

fn default_enabled() -> bool {
    true
}

impl Config {
    /// Reads `config.toml`, falling back to defaults when it is absent or
    /// unreadable. A malformed file is reported but never blocks startup.
    pub fn load() -> Self {
        Self::load_from(&app_paths::config_path())
    }

    pub fn load_from(path: &Path) -> Self {
        let contents = match std::fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::default();
            }
            Err(error) => {
                log::error!("cannot read {}: {error}", path.display());
                return Self::default();
            }
        };

        match toml::from_str::<Self>(&contents) {
            Ok(config) => config,
            Err(error) => {
                log::error!("cannot parse {}: {error}", path.display());
                Self::default()
            }
        }
    }

    pub fn save(&self) {
        self.save_to(&app_paths::config_path());
    }

    pub fn save_to(&self, path: &Path) {
        if let Err(error) = std::fs::write(path, self.to_toml()) {
            log::error!("cannot write {}: {error}", path.display());
        }
    }

    /// Whether detailed logging is on: always in debug builds, otherwise only
    /// when `[logging] verbose = true` (spec §10).
    pub fn verbose_logging(&self) -> bool {
        cfg!(debug_assertions) || self.logging.verbose.unwrap_or(false)
    }

    /// Forgets the last opened folder, used when that folder turned out to be
    /// unusable at startup (spec §8).
    pub fn clear_last_opened(&mut self) {
        self.last_opened = None;
    }

    fn to_toml(&self) -> String {
        let mut out = String::new();

        if let Some(window) = &self.window {
            out.push_str("[window]\n");
            out.push_str(&format!("x = {}\n", window.x));
            out.push_str(&format!("y = {}\n", window.y));
            out.push_str(&format!("width = {}\n", window.width));
            out.push_str(&format!("height = {}\n", window.height));
            out.push('\n');
        }

        if let Some(last_opened) = &self.last_opened {
            out.push_str("[last_opened]\n");
            out.push_str(&format!(
                "folder = {}\n",
                quote(&last_opened.folder.to_string_lossy())
            ));
            out.push_str(&format!("index = {}\n", last_opened.index));
            out.push('\n');
        }

        out.push_str("[overlay]\n");
        out.push_str(
            "# 画像切り替え時にフォルダ名・ファイル名を表示する時間（ミリ秒）。0 で表示しない。\n",
        );
        out.push_str(&format!("duration_ms = {}\n", self.overlay.duration_ms));
        out.push('\n');

        out.push_str("[logging]\n");
        out.push_str(&format!(
            "enabled = {}          # ログ出力する/しない（未指定時のデフォルト: true）\n",
            self.logging.enabled
        ));
        out.push('\n');
        out.push_str("# 詳細ログ（デバッグ情報）を出力するかどうかの設定。\n");
        out.push_str("# デバッグビルドでは常にこの設定に関わらず詳細ログを出力する。\n");
        out.push_str(
            "# リリースビルドでは、この項目が未設定または false の場合はエラーのみ出力する。\n",
        );
        out.push_str(
            "# 実運用中に不具合の手がかりが欲しい場合のみ、以下のコメントを外して true にしてください。\n",
        );
        match self.logging.verbose {
            // Only an explicit `true` is worth keeping as a live key; anything
            // else stays the documented, commented-out sample.
            Some(true) => out.push_str("verbose = true\n"),
            _ => out.push_str("# verbose = false\n"),
        }

        out
    }
}

/// Renders `value` as a TOML basic string, escaping what TOML requires.
fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(config: &Config) -> Config {
        toml::from_str(&config.to_toml()).expect("generated TOML parses")
    }

    #[test]
    fn defaults_enable_logging_without_verbose() {
        let config = Config::default();
        assert!(config.logging.enabled);
        assert_eq!(config.logging.verbose, None);
    }

    #[test]
    fn generated_file_keeps_verbose_commented_out_by_default() {
        let toml = Config::default().to_toml();
        assert!(toml.contains("# verbose = false"));
        assert!(!toml.contains("\nverbose ="));
    }

    #[test]
    fn explicit_verbose_is_written_as_a_live_key() {
        let mut config = Config::default();
        config.logging.verbose = Some(true);
        let toml = config.to_toml();
        assert!(toml.contains("\nverbose = true"));
        assert_eq!(roundtrip(&config).logging.verbose, Some(true));
    }

    #[test]
    fn window_and_last_opened_survive_a_roundtrip() {
        let config = Config {
            window: Some(WindowConfig {
                x: 100,
                y: 120,
                width: 800,
                height: 600,
            }),
            last_opened: Some(LastOpened {
                folder: PathBuf::from(r"C:\Users\yoshiaki\Pictures\sample"),
                index: 5,
            }),
            overlay: OverlayConfig { duration_ms: 1500 },
            logging: LoggingConfig::default(),
        };

        let parsed = roundtrip(&config);
        let window = parsed.window.expect("window section");
        assert_eq!((window.x, window.y, window.width, window.height), (100, 120, 800, 600));
        let last = parsed.last_opened.expect("last_opened section");
        assert_eq!(last.folder, PathBuf::from(r"C:\Users\yoshiaki\Pictures\sample"));
        assert_eq!(last.index, 5);
        assert_eq!(parsed.overlay.duration_ms, 1500);
    }

    #[test]
    fn overlay_duration_defaults_to_five_seconds() {
        let config: Config = toml::from_str("[overlay]\n").expect("parses");
        assert_eq!(config.overlay.duration_ms, 5000);
        assert_eq!(Config::default().overlay.duration_ms, 5000);
    }

    #[test]
    fn a_zero_overlay_duration_is_kept() {
        let config: Config = toml::from_str("[overlay]\nduration_ms = 0\n").expect("parses");
        assert!(config.overlay.duration().is_zero());
        assert!(config.to_toml().contains("\nduration_ms = 0\n"));
    }

    #[test]
    fn a_broken_file_falls_back_to_defaults() {
        let path = std::env::temp_dir().join(format!("imageviewer-bad-{}.toml", std::process::id()));
        std::fs::write(&path, "this is not toml = = =").expect("write");
        let config = Config::load_from(&path);
        assert!(config.last_opened.is_none());
        assert!(config.logging.enabled);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn verbose_logging_follows_the_build_profile() {
        let mut config = Config::default();
        assert_eq!(config.verbose_logging(), cfg!(debug_assertions));
        config.logging.verbose = Some(true);
        assert!(config.verbose_logging());
    }
}
