//! Decoded images kept around the one on screen, so that moving to a
//! neighbour shows it at once instead of waiting for its decode.
//!
//! This module only keeps the books: which files are wanted and in what
//! priority, which are decoded, which to decode next and which to let go. The
//! decoding itself runs on GPUI's background threads, driven by the viewer,
//! which also releases the GPU textures of the images let go here.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use gpui_kit::RenderImage;

use crate::library::Step;
use crate::media;

/// One version of a file on disk. A decode made from a different version is
/// out of date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

impl FileStamp {
    pub fn read(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::metadata(path)?;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

/// The outcome of decoding one file.
#[derive(Clone)]
pub enum Outcome {
    Image(Arc<RenderImage>),
    /// Deleted, moved or renamed since the folder was listed.
    Missing,
    /// There, but unreadable or corrupt.
    Unusable,
}

/// A decode, with the version of the file it was made from.
pub struct Decoded {
    pub outcome: Outcome,
    pub stamp: Option<FileStamp>,
}

impl Decoded {
    /// Pixel memory held, which is what the budget limits.
    fn bytes(&self) -> usize {
        match &self.outcome {
            Outcome::Image(image) => image.as_bytes(0).map_or(0, <[u8]>::len),
            Outcome::Missing | Outcome::Unusable => 0,
        }
    }
}

/// Decodes one file. Safe to run on any thread.
pub fn decode(path: &Path) -> Decoded {
    // Read before decoding, so a change made during the decode shows up as a
    // mismatch later rather than being missed.
    let stamp = FileStamp::read(path).ok();
    let started = Instant::now();
    let outcome = match media::decode(path) {
        Ok(decoded) => {
            log::debug!(
                "decoded {} ({}x{}) in {} ms",
                path.display(),
                decoded.width(),
                decoded.height(),
                started.elapsed().as_millis()
            );
            Outcome::Image(decoded.into_render_image())
        }
        // Expected whenever the folder changed since it was listed.
        Err(error) if media::is_missing(&error) => {
            log::debug!("{} is gone", path.display());
            Outcome::Missing
        }
        Err(error) => {
            log::error!("skipping {}: {error:#}", path.display());
            Outcome::Unusable
        }
    };
    Decoded { outcome, stamp }
}

/// What is known about a file, checked against the disk.
pub enum Lookup {
    Image(Arc<RenderImage>),
    Unusable,
    /// Not on disk (any more).
    Missing,
    /// Not decoded yet, or decoded from a version that has since changed.
    Absent,
}

/// How many decodes to run at once: one core is left for the UI and the rest
/// of the system, and more than four would only race for memory bandwidth.
pub fn default_parallelism() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |cores| cores.get().saturating_sub(1))
        .clamp(1, 4)
}

/// The listing indices worth keeping decoded around `cursor`: the cursor
/// itself, then `ahead` images in the direction of travel, then `behind` the
/// other way, wrapping past either end like ← / → do. Most wanted first.
pub fn window_indices(
    len: usize,
    cursor: usize,
    direction: Step,
    ahead: usize,
    behind: usize,
) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }
    let at = |distance: usize, step: Step| match step {
        Step::Forward => (cursor + distance) % len,
        Step::Backward => (cursor + len - distance % len) % len,
    };
    let reverse = match direction {
        Step::Forward => Step::Backward,
        Step::Backward => Step::Forward,
    };

    let mut indices = vec![cursor];
    let nearby = (1..=ahead)
        .map(|distance| at(distance, direction))
        .chain((1..=behind).map(|distance| at(distance, reverse)));
    for index in nearby {
        if !indices.contains(&index) {
            indices.push(index);
        }
    }
    indices
}

pub struct Prefetcher {
    decoded: HashMap<PathBuf, Decoded>,
    /// Handed out by `next_to_decode` and not yet back through `finish`.
    in_flight: HashSet<PathBuf>,
    /// Files to keep decoded, most wanted first.
    wanted: Vec<PathBuf>,
    budget_bytes: usize,
    max_parallel: usize,
    /// Images let go since the last `take_released`. Their textures may still
    /// be in the GPU atlas, which only the viewer can clear.
    released: Vec<Arc<RenderImage>>,
}

impl Prefetcher {
    pub fn new(budget_bytes: usize, max_parallel: usize) -> Self {
        Self {
            decoded: HashMap::new(),
            in_flight: HashSet::new(),
            wanted: Vec::new(),
            budget_bytes,
            max_parallel: max_parallel.max(1),
            released: Vec::new(),
        }
    }

    /// Replaces the wanted files, most wanted first, and lets go of decodes
    /// no longer wanted.
    pub fn set_wanted(&mut self, wanted: Vec<PathBuf>) {
        if self.wanted != wanted {
            self.wanted = wanted;
            self.trim();
        }
    }

    /// What is known about `path`, after checking the file is still the
    /// version that was decoded.
    pub fn lookup(&mut self, path: &Path) -> Lookup {
        let Some(decoded) = self.decoded.get(path) else {
            return Lookup::Absent;
        };
        let current = match FileStamp::read(path) {
            Ok(stamp) => Some(stamp),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Lookup::Missing,
            // Unreadable metadata: compare as unknown, like `decode` records it.
            Err(_) => None,
        };
        if decoded.stamp != current {
            log::debug!("{} changed since it was decoded", path.display());
            self.forget(path);
            return Lookup::Absent;
        }
        match &decoded.outcome {
            Outcome::Image(image) => Lookup::Image(image.clone()),
            Outcome::Unusable => Lookup::Unusable,
            Outcome::Missing => Lookup::Missing,
        }
    }

    /// The next file to decode, if a decode slot and budget are free. The
    /// caller must hand the result back through [`Self::finish`].
    pub fn next_to_decode(&mut self) -> Option<PathBuf> {
        if self.in_flight.len() >= self.max_parallel {
            return None;
        }
        let mut held = 0;
        for path in &self.wanted {
            // Same rule as `trim`, so a decode started here is also kept.
            if held >= self.budget_bytes {
                break;
            }
            match self.decoded.get(path) {
                Some(decoded) => held += decoded.bytes(),
                None if !self.in_flight.contains(path) => {
                    self.in_flight.insert(path.clone());
                    return Some(path.clone());
                }
                None => {}
            }
        }
        None
    }

    /// Takes back a decode handed out by `next_to_decode`. It is kept only if
    /// the file is still wanted.
    pub fn finish(&mut self, path: PathBuf, decoded: Decoded) {
        self.in_flight.remove(&path);
        self.insert(path, decoded);
    }

    /// Records a decode made elsewhere, such as the image shown at startup.
    pub fn insert(&mut self, path: PathBuf, decoded: Decoded) {
        if !self.wanted.contains(&path) {
            self.release(decoded);
            return;
        }
        if let Some(previous) = self.decoded.insert(path, decoded) {
            self.release(previous);
        }
        self.trim();
    }

    /// Drops what is known about `path`, e.g. after moving it to the trash.
    pub fn forget(&mut self, path: &Path) {
        if let Some(decoded) = self.decoded.remove(path) {
            self.release(decoded);
        }
    }

    /// Drops every decode, so everything is read from disk again (F5).
    /// Decodes still running are kept track of and land as usual.
    pub fn clear(&mut self) {
        for (_, decoded) in std::mem::take(&mut self.decoded) {
            self.release(decoded);
        }
    }

    /// Whether `image` is one of the decodes held.
    pub fn holds(&self, image: &Arc<RenderImage>) -> bool {
        self.decoded.values().any(|decoded| match &decoded.outcome {
            Outcome::Image(held) => Arc::ptr_eq(held, image),
            Outcome::Missing | Outcome::Unusable => false,
        })
    }

    /// Images let go since the previous call, for their textures to be freed.
    pub fn take_released(&mut self) -> Vec<Arc<RenderImage>> {
        std::mem::take(&mut self.released)
    }

    /// Keeps the decodes of wanted files, most wanted first, while they fit in
    /// the budget. The most wanted one is kept whatever its size.
    fn trim(&mut self) {
        let mut keep = HashSet::new();
        let mut held = 0;
        for path in &self.wanted {
            let Some(decoded) = self.decoded.get(path) else {
                continue;
            };
            let bytes = decoded.bytes();
            if bytes > 0 && held >= self.budget_bytes {
                continue;
            }
            held += bytes;
            keep.insert(path.clone());
        }

        let dropped: Vec<PathBuf> = self
            .decoded
            .keys()
            .filter(|path| !keep.contains(*path))
            .cloned()
            .collect();
        for path in dropped {
            self.forget(&path);
        }
    }

    fn release(&mut self, decoded: Decoded) {
        if let Outcome::Image(image) = decoded.outcome {
            self.released.push(image);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{window_indices, Decoded, FileStamp, Lookup, Outcome, Prefetcher};
    use crate::library::Step;
    use crate::test_support::TempTree;
    use gpui_kit::RenderImage;
    use image::{Frame, RgbaImage};
    use smallvec::SmallVec;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// A decoded image of `bytes` bytes of pixels.
    fn image(bytes: u32) -> Decoded {
        let pixels = RgbaImage::new(bytes / 4, 1);
        Decoded {
            outcome: Outcome::Image(Arc::new(RenderImage::new(SmallVec::from_elem(
                Frame::new(pixels),
                1,
            )))),
            stamp: None,
        }
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn the_window_looks_ahead_first_then_behind() {
        assert_eq!(
            window_indices(10, 5, Step::Forward, 3, 2),
            [5, 6, 7, 8, 4, 3]
        );
        assert_eq!(
            window_indices(10, 5, Step::Backward, 3, 2),
            [5, 4, 3, 2, 6, 7]
        );
    }

    #[test]
    fn the_window_wraps_past_either_end() {
        assert_eq!(window_indices(5, 4, Step::Forward, 2, 1), [4, 0, 1, 3]);
        assert_eq!(window_indices(5, 0, Step::Backward, 2, 1), [0, 4, 3, 1]);
    }

    #[test]
    fn a_small_folder_is_not_listed_twice() {
        assert_eq!(window_indices(3, 1, Step::Forward, 4, 2), [1, 2, 0]);
        assert_eq!(window_indices(1, 0, Step::Forward, 4, 2), [0]);
        assert!(window_indices(0, 0, Step::Forward, 4, 2).is_empty());
    }

    #[test]
    fn decodes_are_handed_out_most_wanted_first_up_to_the_parallel_limit() {
        let mut prefetcher = Prefetcher::new(usize::MAX, 2);
        prefetcher.set_wanted(paths(&["a", "b", "c"]));
        assert_eq!(prefetcher.next_to_decode(), Some(PathBuf::from("a")));
        assert_eq!(prefetcher.next_to_decode(), Some(PathBuf::from("b")));
        assert_eq!(prefetcher.next_to_decode(), None, "two are already running");

        prefetcher.finish(PathBuf::from("a"), image(16));
        assert_eq!(prefetcher.next_to_decode(), Some(PathBuf::from("c")));
        assert_eq!(prefetcher.next_to_decode(), None);
    }

    #[test]
    fn a_decode_that_is_no_longer_wanted_is_let_go() {
        let mut prefetcher = Prefetcher::new(usize::MAX, 4);
        prefetcher.set_wanted(paths(&["a"]));
        let a = prefetcher.next_to_decode().expect("a");
        prefetcher.set_wanted(paths(&["b"]));

        prefetcher.finish(a, image(16));
        assert!(!prefetcher.decoded.contains_key(&PathBuf::from("a")));
        assert_eq!(prefetcher.take_released().len(), 1);
    }

    #[test]
    fn moving_on_lets_go_of_what_left_the_window() {
        let mut prefetcher = Prefetcher::new(usize::MAX, 4);
        prefetcher.set_wanted(paths(&["a", "b"]));
        prefetcher.insert(PathBuf::from("a"), image(16));
        prefetcher.insert(PathBuf::from("b"), image(16));
        assert!(prefetcher.take_released().is_empty());

        prefetcher.set_wanted(paths(&["b", "c"]));
        assert_eq!(prefetcher.take_released().len(), 1, "a left the window");
    }

    #[test]
    fn the_budget_drops_the_least_wanted_and_stops_new_decodes() {
        let mut prefetcher = Prefetcher::new(100, 4);
        prefetcher.set_wanted(paths(&["a", "b", "c", "d"]));
        prefetcher.insert(PathBuf::from("a"), image(60));
        prefetcher.insert(PathBuf::from("b"), image(60));
        // a + b already reach the budget: c is not worth decoding...
        assert_eq!(prefetcher.next_to_decode(), None);
        // ...and a decode of it that lands anyway is dropped again.
        prefetcher.insert(PathBuf::from("c"), image(60));
        assert!(!prefetcher.decoded.contains_key(&PathBuf::from("c")));
        assert!(prefetcher.decoded.contains_key(&PathBuf::from("b")));
    }

    #[test]
    fn the_most_wanted_image_is_kept_even_over_budget() {
        let mut prefetcher = Prefetcher::new(10, 4);
        prefetcher.set_wanted(paths(&["a"]));
        prefetcher.insert(PathBuf::from("a"), image(400));
        assert!(prefetcher.decoded.contains_key(&PathBuf::from("a")));
    }

    #[test]
    fn a_changed_or_vanished_file_is_noticed() {
        let tree = TempTree::new("prefetch-stamp");
        let path = tree.write("a.png", b"one");
        let mut prefetcher = Prefetcher::new(usize::MAX, 4);
        prefetcher.set_wanted(vec![path.clone()]);

        let mut decoded = image(16);
        decoded.stamp = FileStamp::read(&path).ok();
        prefetcher.insert(path.clone(), decoded);
        assert!(matches!(prefetcher.lookup(&path), Lookup::Image(_)));

        tree.write("a.png", b"a longer version");
        assert!(matches!(prefetcher.lookup(&path), Lookup::Absent));

        let mut decoded = image(16);
        decoded.stamp = FileStamp::read(&path).ok();
        prefetcher.insert(path.clone(), decoded);
        std::fs::remove_file(&path).expect("remove");
        assert!(matches!(prefetcher.lookup(&path), Lookup::Missing));
    }

    #[test]
    fn holds_recognises_only_images_it_keeps() {
        let mut prefetcher = Prefetcher::new(usize::MAX, 4);
        prefetcher.set_wanted(paths(&["a"]));
        let decoded = image(16);
        let Outcome::Image(kept) = decoded.outcome.clone() else {
            unreachable!()
        };
        prefetcher.insert(PathBuf::from("a"), decoded);
        assert!(prefetcher.holds(&kept));

        prefetcher.clear();
        assert!(!prefetcher.holds(&kept));
    }
}
