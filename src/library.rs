//! Reading the folders the viewer navigates (spec §4).
//!
//! Every listing is taken fresh from disk, because files can be deleted or
//! moved while the viewer is open.

use std::path::{Path, PathBuf};

use crate::media;
use crate::natural_sort::natural_cmp_paths;

/// Which way ← / → and ↑ / ↓ move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Backward,
    Forward,
}

/// Displayable images directly inside `folder`, in natural order.
pub fn list_images(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        log::debug!("cannot read folder {}", folder.display());
        return Vec::new();
    };

    let mut images: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| media::is_supported(path) && path.is_file())
        .collect();
    images.sort_by(|a, b| natural_cmp_paths(a, b));
    images
}

/// Whether `folder` holds at least one displayable image. Folders that do not
/// are invisible to ↑ / ↓ navigation (spec §4).
pub fn has_images(folder: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| media::is_supported(&entry.path()) && entry.path().is_file())
}

/// Folders sharing a parent with `folder`, in natural order, including
/// `folder` itself.
pub fn sibling_folders(folder: &Path) -> Vec<PathBuf> {
    let Some(parent) = folder.parent() else {
        return vec![folder.to_path_buf()];
    };
    let Ok(entries) = std::fs::read_dir(parent) else {
        return vec![folder.to_path_buf()];
    };

    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort_by(|a, b| natural_cmp_paths(a, b));
    folders
}

/// The next sibling folder in `step` that holds at least one image, skipping
/// image-less folders entirely.
pub fn next_folder_with_images(folder: &Path, step: Step) -> Option<PathBuf> {
    let siblings = sibling_folders(folder);
    let current = position_of(&siblings, folder)?;

    let candidates: Vec<&PathBuf> = match step {
        Step::Forward => siblings.iter().skip(current + 1).collect(),
        Step::Backward => siblings.iter().take(current).rev().collect(),
    };

    candidates
        .into_iter()
        .find(|candidate| has_images(candidate))
        .cloned()
}

/// Index of `folder` within `siblings`, comparing canonical paths so that
/// `.`, symlinks and trailing separators still match.
fn position_of(siblings: &[PathBuf], folder: &Path) -> Option<usize> {
    let target = std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf());
    siblings.iter().position(|sibling| {
        std::fs::canonicalize(sibling).unwrap_or_else(|_| sibling.clone()) == target
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempTree;

    #[test]
    fn images_are_listed_in_natural_order() {
        let tree = TempTree::new("list");
        let folder = tree.dir("pics");
        tree.image("pics/img10.jpg", 8, 8, [10, 10, 10]);
        tree.image("pics/img2.jpg", 8, 8, [20, 20, 20]);
        tree.image("pics/img1.png", 8, 8, [30, 30, 30]);
        tree.write("pics/notes.txt", b"not an image");

        let names: Vec<String> = list_images(&folder)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["img1.png", "img2.jpg", "img10.jpg"]);
    }

    #[test]
    fn folders_without_images_are_skipped() {
        let tree = TempTree::new("skip");
        let a = tree.dir("a");
        tree.dir("b");
        tree.dir("c");
        tree.image("a/1.png", 8, 8, [1, 1, 1]);
        tree.write("b/readme.txt", b"no images here");
        tree.image("c/1.jpg", 8, 8, [2, 2, 2]);

        let next = next_folder_with_images(&a, Step::Forward).expect("a folder after a");
        assert_eq!(next.file_name().unwrap(), "c");
    }

    #[test]
    fn there_is_nothing_before_the_first_folder() {
        let tree = TempTree::new("first");
        let a = tree.dir("a");
        tree.image("a/1.png", 8, 8, [1, 1, 1]);
        assert_eq!(next_folder_with_images(&a, Step::Backward), None);
    }

    #[test]
    fn backward_finds_the_nearest_preceding_folder_with_images() {
        let tree = TempTree::new("back");
        tree.dir("a");
        tree.dir("b");
        let c = tree.dir("c");
        tree.image("a/1.png", 8, 8, [1, 1, 1]);
        tree.image("c/1.png", 8, 8, [2, 2, 2]);

        let prev = next_folder_with_images(&c, Step::Backward).expect("a folder before c");
        assert_eq!(prev.file_name().unwrap(), "a");
    }

    #[test]
    fn has_images_ignores_unsupported_files() {
        let tree = TempTree::new("has");
        let folder = tree.dir("only-text");
        tree.write("only-text/a.txt", b"x");
        tree.write("only-text/b.gif", b"x");
        assert!(!has_images(&folder));

        tree.image("only-text/c.png", 4, 4, [0, 0, 0]);
        assert!(has_images(&folder));
    }

    #[test]
    fn sibling_folders_are_in_natural_order() {
        let tree = TempTree::new("siblings");
        let d1 = tree.dir("dir1");
        tree.dir("dir10");
        tree.dir("dir2");

        let names: Vec<String> = sibling_folders(&d1)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["dir1", "dir2", "dir10"]);
    }
}
