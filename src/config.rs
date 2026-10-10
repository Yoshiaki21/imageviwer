//! `config.toml` next to the executable (spec §9).
//!
//! The file is written by hand rather than through `toml::to_string`, because
//! spec §10 requires the `verbose` key to ship as a commented-out sample with
//! its explanation, and a serializer cannot emit comments.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use crate::app_paths;
use crate::folder_history::FolderHistory;

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
    pub prefetch: PrefetchConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub folder_history: FolderHistory,
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

/// Decoding the images around the one on screen ahead of time.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrefetchConfig {
    /// Images decoded ahead in the direction of travel.
    #[serde(default = "default_prefetch_ahead")]
    pub ahead: usize,
    /// Images kept decoded the other way, for going back.
    #[serde(default = "default_prefetch_behind")]
    pub behind: usize,
    /// Upper limit on the memory the decoded images may take, in megabytes.
    #[serde(default = "default_prefetch_memory_mb")]
    pub memory_mb: usize,
}

impl Default for PrefetchConfig {
    fn default() -> Self {
        Self {
            ahead: default_prefetch_ahead(),
            behind: default_prefetch_behind(),
            memory_mb: default_prefetch_memory_mb(),
        }
    }
}

impl PrefetchConfig {
    pub fn memory_bytes(&self) -> usize {
        self.memory_mb.saturating_mul(1024 * 1024)
    }
}

fn default_prefetch_ahead() -> usize {
    4
}

fn default_prefetch_behind() -> usize {
    2
}

fn default_prefetch_memory_mb() -> usize {
    1024
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

        out.push_str("[prefetch]\n");
        out.push_str(
            "# 表示中の画像の前後を別スレッドで先にデコードしておき、切り替えを速くする。\n",
        );
        out.push_str(&format!(
            "ahead = {}          # 進行方向に先読みする枚数\n",
            self.prefetch.ahead
        ));
        out.push_str(&format!(
            "behind = {}         # 逆方向（戻る側）にデコード済みで残しておく枚数\n",
            self.prefetch.behind
        ));
        out.push_str(&format!(
            "memory_mb = {}   # 先読みに使うメモリの上限（MB）。超えたら遠い画像から捨てる\n",
            self.prefetch.memory_mb
        ));
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
        out.push('\n');

        // Last, so that a long list of folders does not bury the settings.
        out.push_str("[folder_history]\n");
        out.push_str(
            "# ↑↓ でフォルダに戻ったとき、前回見ていた画像から表示するために覚えておくフォルダの数。0 で覚えない。\n",
        );
        out.push_str(&format!(
            "max_folders = {}\n",
            self.folder_history.max_folders
        ));
        if !self.folder_history.entries.is_empty() {
            out.push('\n');
            out.push_str("# 以下は自動で記録される（最後に見た順）。\n");
        }
        for entry in &self.folder_history.entries {
            out.push_str("[[folder_history.entries]]\n");
            out.push_str(&format!(
                "folder = {}\n",
                quote(&entry.folder.to_string_lossy())
            ));
            out.push_str(&format!("file = {}\n", quote(&entry.file)));
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
            prefetch: PrefetchConfig::default(),
            logging: LoggingConfig::default(),
            folder_history: FolderHistory::default(),
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
    fn prefetch_settings_default_and_survive_a_roundtrip() {
        let defaults = roundtrip(&Config::default()).prefetch;
        assert_eq!(
            (defaults.ahead, defaults.behind, defaults.memory_mb),
            (4, 2, 1024)
        );

        let mut config = Config::default();
        config.prefetch.ahead = 8;
        config.prefetch.behind = 3;
        config.prefetch.memory_mb = 2048;
        let parsed = roundtrip(&config).prefetch;
        assert_eq!((parsed.ahead, parsed.behind, parsed.memory_mb), (8, 3, 2048));
    }

    #[test]
    fn folder_history_defaults_and_survives_a_roundtrip() {
        let defaults = roundtrip(&Config::default()).folder_history;
        assert_eq!(defaults.max_folders, 100);
        assert!(defaults.entries.is_empty());

        let mut config = Config::default();
        config.folder_history.max_folders = 5;
        config
            .folder_history
            .remember(Path::new(r"C:\Pics\a"), Path::new(r#"C:\Pics\a\img "1".png"#));
        config
            .folder_history
            .remember(Path::new("/pics/b"), Path::new("/pics/b/画像.jpg"));
        let parsed = roundtrip(&config).folder_history;
        assert_eq!(parsed.max_folders, 5);
        assert_eq!(parsed.entries, config.folder_history.entries);
    }

    #[test]
    fn a_file_without_a_folder_history_section_uses_the_defaults() {
        let parsed: Config = toml::from_str("[overlay]\nduration_ms = 100\n").expect("parses");
        assert_eq!(parsed.folder_history.max_folders, 100);
        assert!(parsed.folder_history.entries.is_empty());
    }

    #[test]
    fn a_file_without_a_prefetch_section_uses_the_defaults() {
        let parsed: Config = toml::from_str("[overlay]\nduration_ms = 100\n").expect("parses");
        assert_eq!(parsed.prefetch.ahead, 4);
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
