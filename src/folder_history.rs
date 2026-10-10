//! The image last shown in each folder, so that ↑ / ↓ back into a folder
//! resumes where it was left (spec §4). Kept in `config.toml` under
//! `[folder_history]`, most recently left first.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderHistory {
    /// How many folders to remember. 0 remembers none.
    #[serde(default = "default_max_folders")]
    pub max_folders: usize,
    /// Most recently left first, one per folder.
    #[serde(default)]
    pub entries: Vec<FolderPosition>,
}

/// The file name (not the path) shown when `folder` was last left.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FolderPosition {
    pub folder: PathBuf,
    pub file: String,
}

impl Default for FolderHistory {
    fn default() -> Self {
        Self {
            max_folders: default_max_folders(),
            entries: Vec::new(),
        }
    }
}

fn default_max_folders() -> usize {
    100
}

impl FolderHistory {
    /// Records `shown` as the image last seen in `folder`, replacing any
    /// earlier record for the folder and forgetting the folder left longest
    /// ago once over `max_folders`.
    pub fn remember(&mut self, folder: &Path, shown: &Path) {
        let Some(file) = shown.file_name() else {
            return;
        };
        self.entries
            .retain(|entry| !same_folder(&entry.folder, folder));
        self.entries.insert(
            0,
            FolderPosition {
                folder: folder.to_path_buf(),
                file: file.to_string_lossy().into_owned(),
            },
        );
        self.entries.truncate(self.max_folders);
    }

    /// The file name last seen in `folder`, if it is remembered.
    pub fn recall(&self, folder: &Path) -> Option<&str> {
        self.entries
            .iter()
            .take(self.max_folders)
            .find(|entry| same_folder(&entry.folder, folder))
            .map(|entry| entry.file.as_str())
    }
}

/// Whether two paths name the same folder. Windows paths are compared without
/// regard to case, since a file association and the sibling listing may spell
/// the same folder differently.
fn same_folder(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        let fold = |path: &Path| -> Vec<String> {
            path.components()
                .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
                .collect()
        };
        fold(a) == fold(b)
    } else {
        a == b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(max_folders: usize) -> FolderHistory {
        FolderHistory {
            max_folders,
            entries: Vec::new(),
        }
    }

    fn folders(history: &FolderHistory) -> Vec<&Path> {
        history
            .entries
            .iter()
            .map(|entry| entry.folder.as_path())
            .collect()
    }

    #[test]
    fn a_remembered_folder_recalls_its_file_name() {
        let mut history = history(10);
        history.remember(Path::new("/pics/a"), Path::new("/pics/a/3.png"));
        assert_eq!(history.recall(Path::new("/pics/a")), Some("3.png"));
        assert_eq!(history.recall(Path::new("/pics/b")), None);
    }

    #[test]
    fn the_same_folder_is_overwritten_and_moved_to_the_front() {
        let mut history = history(10);
        history.remember(Path::new("/a"), Path::new("/a/1.png"));
        history.remember(Path::new("/b"), Path::new("/b/1.png"));
        history.remember(Path::new("/a"), Path::new("/a/7.png"));
        assert_eq!(folders(&history), [Path::new("/a"), Path::new("/b")]);
        assert_eq!(history.recall(Path::new("/a")), Some("7.png"));
    }

    #[test]
    fn the_folder_left_longest_ago_is_forgotten_first() {
        let mut history = history(2);
        history.remember(Path::new("/a"), Path::new("/a/1.png"));
        history.remember(Path::new("/b"), Path::new("/b/1.png"));
        history.remember(Path::new("/a"), Path::new("/a/2.png"));
        history.remember(Path::new("/c"), Path::new("/c/1.png"));
        assert_eq!(folders(&history), [Path::new("/c"), Path::new("/a")]);
    }

    #[test]
    fn zero_remembers_nothing() {
        let mut history = history(0);
        history.remember(Path::new("/a"), Path::new("/a/1.png"));
        assert!(history.entries.is_empty());
        assert_eq!(history.recall(Path::new("/a")), None);
    }

    #[test]
    fn a_lowered_limit_trims_on_the_next_remember() {
        let mut history = history(3);
        for name in ["/a", "/b", "/c"] {
            let folder = Path::new(name);
            history.remember(folder, &folder.join("1.png"));
        }
        history.max_folders = 1;
        assert_eq!(history.recall(Path::new("/a")), None, "beyond the limit");
        history.remember(Path::new("/d"), Path::new("/d/1.png"));
        assert_eq!(folders(&history), [Path::new("/d")]);
    }

    #[test]
    fn a_trailing_separator_is_the_same_folder() {
        let mut history = history(10);
        history.remember(Path::new("/pics/a/"), Path::new("/pics/a/2.png"));
        assert_eq!(history.recall(Path::new("/pics/a")), Some("2.png"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_folders_compare_without_case() {
        let mut history = history(10);
        history.remember(Path::new(r"C:\Pics\A"), Path::new(r"C:\Pics\A\2.png"));
        assert_eq!(history.recall(Path::new(r"c:\pics\a")), Some("2.png"));
    }

    #[cfg(unix)]
    #[test]
    fn unix_folders_compare_with_case() {
        let mut history = history(10);
        history.remember(Path::new("/Pics/A"), Path::new("/Pics/A/2.png"));
        assert_eq!(history.recall(Path::new("/pics/a")), None);
    }
}
