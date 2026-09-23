//! The window contents: one image, letterboxed on black (spec §5), driven by
//! the arrow keys and Enter (spec §4).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_kit::*;

use crate::config::{Config, LastOpened, WindowConfig};
use crate::library::{self, Step};
use crate::media;

/// Key context for the viewer's bindings.
pub const KEY_CONTEXT: &str = "ImageViewer";

gpui_kit::actions!(
    imageviewer,
    [
        NextImage,
        PreviousImage,
        NextFolder,
        PreviousFolder,
        ToggleFullscreen,
        Quit,
    ]
);

/// Installs the viewer's key bindings. Call once, before the window opens.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("right", NextImage, Some(KEY_CONTEXT)),
        KeyBinding::new("left", PreviousImage, Some(KEY_CONTEXT)),
        KeyBinding::new("down", NextFolder, Some(KEY_CONTEXT)),
        KeyBinding::new("up", PreviousFolder, Some(KEY_CONTEXT)),
        KeyBinding::new("enter", ToggleFullscreen, Some(KEY_CONTEXT)),
        // Not in the spec, but a fullscreen window has no close button.
        KeyBinding::new("ctrl-q", Quit, Some(KEY_CONTEXT)),
        KeyBinding::new("escape", Quit, Some(KEY_CONTEXT)),
    ]);
}

/// What the window should show when it opens.
pub enum Startup {
    /// A specific file, from the command line or a file association.
    Image(PathBuf),
    /// The folder and position recorded at the previous exit (spec §8).
    Restored { folder: PathBuf, index: usize },
    /// Nothing to show; the text explains why and the window is dismissible.
    Message(String),
}

/// The folder currently being browsed and the image on screen.
struct Gallery {
    folder: PathBuf,
    /// Natural-ordered listing, re-read whenever the folder changes.
    entries: Vec<PathBuf>,
    index: usize,
    image: Arc<RenderImage>,
}

enum Content {
    Gallery(Gallery),
    Message(SharedString),
}

pub struct ViewerView {
    focus_handle: FocusHandle,
    content: Content,
    /// Last geometry seen while the window was *not* fullscreen.
    ///
    /// X11 reports the live fullscreen rectangle from `window_bounds()`
    /// instead of the restore rectangle (Wayland reports the restore one), so
    /// the size to save on exit has to be remembered here.
    windowed_bounds: Option<Bounds<Pixels>>,
}

impl ViewerView {
    pub fn new(startup: Startup, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let content = match startup {
            Startup::Image(path) => Self::open_file(&path),
            Startup::Restored { folder, index } => Self::open_folder_at(&folder, index),
            Startup::Message(text) => Content::Message(text.into()),
        };

        Self {
            focus_handle: cx.focus_handle(),
            content,
            windowed_bounds: None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
    }

    /// Handles a launch request forwarded by a second process (spec §3). The
    /// window's position and size are deliberately left alone.
    pub fn handle_launch_request(
        &mut self,
        path: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = path else {
            // A bare re-launch just raises the window; the caller does that.
            log::debug!("launch request without a path");
            return;
        };

        log::debug!("launch request for {}", path.display());
        let content = Self::open_file(&path);
        self.replace_content(content, window, cx);
    }

    /// Records the window geometry and reading position for the next launch
    /// (spec §7/§9).
    pub fn save_state(&self, window: &mut Window, _cx: &mut Context<Self>) {
        let mut config = Config::load();

        // Startup is always windowed (spec 7), so the geometry to save is the
        // windowed one. While fullscreen that has to come from the cache,
        // because X11 reports the fullscreen rectangle instead.
        let bounds = if window.is_fullscreen() {
            self.windowed_bounds
                .unwrap_or_else(|| window.window_bounds().get_bounds())
        } else {
            window.window_bounds().get_bounds()
        };
        config.window = Some(WindowConfig {
            x: f32::from(bounds.origin.x).round() as i32,
            y: f32::from(bounds.origin.y).round() as i32,
            width: f32::from(bounds.size.width).round().max(0.) as u32,
            height: f32::from(bounds.size.height).round().max(0.) as u32,
        });

        if let Content::Gallery(gallery) = &self.content {
            config.last_opened = Some(LastOpened {
                folder: gallery.folder.clone(),
                index: gallery.index,
            });
        }

        config.save();
    }

    fn on_next_image(&mut self, _: &NextImage, window: &mut Window, cx: &mut Context<Self>) {
        self.step_image(Step::Forward, window, cx);
    }

    fn on_previous_image(
        &mut self,
        _: &PreviousImage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_image(Step::Backward, window, cx);
    }

    fn on_next_folder(&mut self, _: &NextFolder, window: &mut Window, cx: &mut Context<Self>) {
        self.step_folder(Step::Forward, window, cx);
    }

    fn on_previous_folder(
        &mut self,
        _: &PreviousFolder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.step_folder(Step::Backward, window, cx);
    }

    fn on_toggle_fullscreen(
        &mut self,
        _: &ToggleFullscreen,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        log::debug!("toggling fullscreen");
        window.toggle_fullscreen();
    }

    fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.save_state(window, cx);
        cx.quit();
    }

    /// Moves one image within the current folder. Files that vanished or fail
    /// to decode are skipped (spec §8).
    fn step_image(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("step image {step:?}");
        let Content::Gallery(gallery) = &self.content else {
            return;
        };

        // Re-read the folder so deletions made elsewhere are noticed.
        let entries = library::list_images(&gallery.folder);
        let Some(current) = position_of(&entries, &gallery.entries, gallery.index) else {
            // The current file is gone; land on the nearest survivor instead.
            let folder = gallery.folder.clone();
            let index = gallery.index;
            let content = Self::open_folder_at(&folder, index);
            self.replace_content(content, window, cx);
            return;
        };

        let start = match step {
            Step::Forward => current + 1,
            Step::Backward => match current.checked_sub(1) {
                Some(previous) => previous,
                // Already at the first image; nothing before it.
                None => return,
            },
        };

        let Some((index, image)) = load_scanning(&entries, start, step) else {
            log::debug!("no further image in this direction");
            return;
        };

        let folder = gallery.folder.clone();
        self.replace_content(
            Content::Gallery(Gallery {
                folder,
                entries,
                index,
                image,
            }),
            window,
            cx,
        );
    }

    /// Moves to the previous/next sibling folder that holds images and shows
    /// its first image (spec §4).
    fn step_folder(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("step folder {step:?}");
        let Content::Gallery(gallery) = &self.content else {
            return;
        };

        let mut from = gallery.folder.clone();
        while let Some(folder) = library::next_folder_with_images(&from, step) {
            let entries = library::list_images(&folder);
            if let Some((index, image)) = load_scanning(&entries, 0, Step::Forward) {
                self.replace_content(
                    Content::Gallery(Gallery {
                        folder,
                        entries,
                        index,
                        image,
                    }),
                    window,
                    cx,
                );
                return;
            }
            // The folder listed images but none of them could be decoded, so
            // it is as good as empty: keep looking in the same direction.
            log::debug!("no decodable image in {}", folder.display());
            from = folder;
        }

        log::debug!("no further folder with images in this direction");
    }

    /// Swaps in new content, releasing the texture the old image held.
    fn replace_content(&mut self, content: Content, window: &mut Window, cx: &mut Context<Self>) {
        if let Content::Gallery(previous) = &self.content {
            let _ = window.drop_image(previous.image.clone());
        }
        self.content = content;
        cx.notify();
    }

    /// Opens the folder containing `path` and shows `path` itself, or the
    /// nearest usable image if it cannot be displayed.
    fn open_file(path: &Path) -> Content {
        if path.is_dir() {
            return Self::open_folder_at(path, 0);
        }

        let Some(folder) = path.parent().map(PathBuf::from) else {
            return Content::Message(message_for_missing(path));
        };

        let entries = library::list_images(&folder);
        let target = entries.iter().position(|entry| entry == path).unwrap_or(0);

        match load_nearest(&entries, target) {
            Some((index, image)) => Content::Gallery(Gallery {
                folder,
                entries,
                index,
                image,
            }),
            None => Content::Message(message_for_missing(path)),
        }
    }

    /// Opens `folder` at the saved index, or the nearest usable image to it
    /// (spec §8.3).
    fn open_folder_at(folder: &Path, index: usize) -> Content {
        let entries = library::list_images(folder);
        match load_nearest(&entries, index) {
            Some((index, image)) => Content::Gallery(Gallery {
                folder: folder.to_path_buf(),
                entries,
                index,
                image,
            }),
            None => Content::Message(message_for_empty_folder(folder)),
        }
    }

    /// The image surface. Zoom would be added here: the element already owns
    /// the full window, so a future zoom only changes how the image is fitted
    /// and offset inside it (spec §5).
    fn render_image(&self, image: &Arc<RenderImage>) -> impl IntoElement {
        img(image.clone()).object_fit(ObjectFit::Contain).size_full()
    }
}

impl Render for ViewerView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !window.is_fullscreen() {
            self.windowed_bounds = Some(window.window_bounds().get_bounds());
        }

        div()
            .key_context(KEY_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_next_image))
            .on_action(cx.listener(Self::on_previous_image))
            .on_action(cx.listener(Self::on_next_folder))
            .on_action(cx.listener(Self::on_previous_folder))
            .on_action(cx.listener(Self::on_toggle_fullscreen))
            .on_action(cx.listener(Self::on_quit))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgb(0x000000))
            .child(match &self.content {
                Content::Gallery(gallery) => self.render_image(&gallery.image).into_any_element(),
                Content::Message(text) => div()
                    .p_8()
                    .text_color(rgb(0xE6E6E6))
                    .child(text.clone())
                    .into_any_element(),
            })
    }
}

/// Finds where the previously displayed file ended up in a freshly read
/// listing, so navigation stays anchored across changes on disk.
fn position_of(entries: &[PathBuf], previous: &[PathBuf], index: usize) -> Option<usize> {
    let current = previous.get(index)?;
    entries.iter().position(|entry| entry == current)
}

/// Loads the first image at or after `start` in the given direction, skipping
/// files that no longer exist or fail to decode (spec §8).
fn load_scanning(
    entries: &[PathBuf],
    start: usize,
    step: Step,
) -> Option<(usize, Arc<RenderImage>)> {
    let indices: Vec<usize> = match step {
        Step::Forward => (start..entries.len()).collect(),
        Step::Backward => (0..=start.min(entries.len().saturating_sub(1))).rev().collect(),
    };

    if entries.is_empty() {
        return None;
    }

    for index in indices {
        if let Some(image) = try_load(&entries[index]) {
            return Some((index, image));
        }
    }
    None
}

/// Loads the image closest to `target`, preferring the lower index when the
/// distance is equal (spec §8.3).
fn load_nearest(entries: &[PathBuf], target: usize) -> Option<(usize, Arc<RenderImage>)> {
    if entries.is_empty() {
        return None;
    }

    let target = target.min(entries.len() - 1);
    for distance in 0..entries.len() {
        // Lower index first: on a tie the earlier file wins.
        if let Some(index) = target.checked_sub(distance) {
            if let Some(image) = try_load(&entries[index]) {
                return Some((index, image));
            }
        }
        if distance > 0 {
            let index = target + distance;
            if index < entries.len() {
                if let Some(image) = try_load(&entries[index]) {
                    return Some((index, image));
                }
            }
        }
    }
    None
}

/// Decodes one file, treating any failure as "this image is not there".
fn try_load(path: &Path) -> Option<Arc<RenderImage>> {
    match media::decode(path) {
        Ok(decoded) => {
            log::debug!(
                "showing {} ({}x{})",
                path.display(),
                decoded.width(),
                decoded.height()
            );
            Some(decoded.into_render_image())
        }
        Err(error) => {
            log::error!("skipping {}: {error:#}", path.display());
            None
        }
    }
}

fn message_for_missing(path: &Path) -> SharedString {
    format!(
        "画像を表示できませんでした。\n{}",
        path.display()
    )
    .into()
}

fn message_for_empty_folder(folder: &Path) -> SharedString {
    format!(
        "表示できる画像がありません。\n{}",
        folder.display()
    )
    .into()
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that would re-export GPUI's own `test`
    // attribute over Rust's.
    use super::{load_nearest, load_scanning, position_of};
    use crate::library::{self, Step};
    use crate::test_support::TempTree;
    use std::path::PathBuf;

    /// A folder whose middle image is corrupt, plus its natural-order listing.
    fn folder_with_a_broken_middle(name: &str) -> (TempTree, Vec<PathBuf>) {
        let tree = TempTree::new(name);
        let folder = tree.dir("pics");
        tree.image("pics/1.png", 8, 8, [10, 10, 10]);
        tree.write("pics/2.png", b"not really a png");
        tree.image("pics/3.png", 8, 8, [30, 30, 30]);
        let entries = library::list_images(&folder);
        assert_eq!(entries.len(), 3);
        (tree, entries)
    }

    #[test]
    fn scanning_forward_skips_an_undecodable_file() {
        let (_tree, entries) = folder_with_a_broken_middle("scan-forward");
        let (index, _) = load_scanning(&entries, 1, Step::Forward).expect("an image after 1.png");
        assert_eq!(index, 2, "the corrupt 2.png must be treated as absent");
    }

    #[test]
    fn scanning_backward_skips_an_undecodable_file() {
        let (_tree, entries) = folder_with_a_broken_middle("scan-backward");
        let (index, _) = load_scanning(&entries, 1, Step::Backward).expect("an image before 3.png");
        assert_eq!(index, 0);
    }

    #[test]
    fn scanning_past_the_end_finds_nothing() {
        let (_tree, entries) = folder_with_a_broken_middle("scan-end");
        assert!(load_scanning(&entries, 3, Step::Forward).is_none());
    }

    #[test]
    fn nearest_prefers_the_lower_index_on_a_tie() {
        let (_tree, entries) = folder_with_a_broken_middle("nearest-tie");
        // The saved index points at the corrupt file; 1.png and 3.png are both
        // one step away, so the earlier one wins (spec 8.3).
        let (index, _) = load_nearest(&entries, 1).expect("a nearby image");
        assert_eq!(index, 0);
    }

    #[test]
    fn nearest_returns_the_exact_index_when_it_is_usable() {
        let (_tree, entries) = folder_with_a_broken_middle("nearest-exact");
        let (index, _) = load_nearest(&entries, 2).expect("the requested image");
        assert_eq!(index, 2);
    }

    #[test]
    fn nearest_clamps_an_index_past_the_end_of_the_folder() {
        let tree = TempTree::new("nearest-clamp");
        let folder = tree.dir("pics");
        tree.image("pics/1.png", 8, 8, [10, 10, 10]);
        tree.image("pics/2.png", 8, 8, [20, 20, 20]);
        let entries = library::list_images(&folder);

        let (index, _) = load_nearest(&entries, 99).expect("the last image");
        assert_eq!(index, 1);
    }

    #[test]
    fn an_empty_folder_yields_nothing() {
        assert!(load_nearest(&[], 0).is_none());
        assert!(load_scanning(&[], 0, Step::Forward).is_none());
    }

    #[test]
    fn a_folder_of_only_broken_files_yields_nothing() {
        let tree = TempTree::new("all-broken");
        let folder = tree.dir("pics");
        tree.write("pics/1.png", b"broken");
        tree.write("pics/2.jpg", b"broken");
        let entries = library::list_images(&folder);
        assert_eq!(entries.len(), 2);
        assert!(load_nearest(&entries, 0).is_none());
    }

    #[test]
    fn position_of_tracks_a_file_that_moved_in_the_listing() {
        let previous = vec![PathBuf::from("a.png"), PathBuf::from("b.png")];
        let entries = vec![PathBuf::from("b.png")];
        assert_eq!(position_of(&entries, &previous, 1), Some(0));
        assert_eq!(position_of(&entries, &previous, 0), None);
    }
}
