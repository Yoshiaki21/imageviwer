//! The window contents: one image, letterboxed on black (spec §5), driven by
//! the keyboard and mouse (spec §4), with a caption naming the folder and file
//! for a few seconds after each change.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use crate::config::{Config, LastOpened, WindowConfig};
use crate::library::{self, Step};
use crate::media;
use crate::natural_sort::natural_cmp_paths;
use crate::pointer::{self, ClickArea, WheelNotches};
use crate::report;

/// How long the caption takes to fade out at the end of its display time.
const CAPTION_FADE: Duration = Duration::from_millis(300);

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
        DeleteImage,
        RandomImage,
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
        KeyBinding::new("delete", DeleteImage, Some(KEY_CONTEXT)),
        KeyBinding::new("r", RandomImage, Some(KEY_CONTEXT)),
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

/// The caption shown at the bottom for a while: where the new image is, or
/// why nothing changed.
struct Caption {
    text: CaptionText,
    /// Distinguishes each showing, so a new one restarts the fade animation.
    showing: usize,
    fading: bool,
}

enum CaptionText {
    /// After the image changes: the folder, and the file within it.
    Location {
        folder: SharedString,
        file: SharedString,
    },
    /// A one-line notice, e.g. that there is no further sibling folder.
    Notice(SharedString),
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
    /// From `[overlay] duration_ms`; zero means no caption.
    caption_duration: Duration,
    caption: Option<Caption>,
    /// Hides the caption when it fires; replacing it cancels the old timer.
    caption_timer: Option<Task<()>>,
    caption_showings: usize,
    /// A single click waiting to see whether a second one makes it a double
    /// click. Dropping the task cancels the click.
    pending_click: Option<(ClickArea, Task<()>)>,
    wheel: WheelNotches,
}

impl ViewerView {
    pub fn new(
        startup: Startup,
        caption_duration: Duration,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = match startup {
            Startup::Image(path) => Self::open_file(&path),
            Startup::Restored { folder, index } => Self::open_folder_at(&folder, index),
            Startup::Message(text) => Content::Message(text.into()),
        };

        let mut view = Self {
            focus_handle: cx.focus_handle(),
            content,
            windowed_bounds: None,
            caption_duration,
            caption: None,
            caption_timer: None,
            caption_showings: 0,
            pending_click: None,
            wheel: WheelNotches::default(),
        };
        view.show_caption(cx);
        view
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

    /// Moves the image on screen to the OS trash and shows the one that
    /// followed it, wrapping to the first image like → does.
    fn on_delete_image(&mut self, _: &DeleteImage, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Gallery(gallery) = &self.content else {
            return;
        };
        let deleted = gallery.entries[gallery.index].clone();

        if let Err(error) = trash::delete(&deleted) {
            report::error(&format!(
                "ごみ箱に移動できませんでした。\n{}\n\n{error}",
                deleted.display()
            ));
            return;
        }
        log::debug!("moved {} to the trash", deleted.display());

        let folder = gallery.folder.clone();
        let entries = library::list_images(&folder);
        // The first file sorting after the deleted one is its successor.
        let successor = entries
            .partition_point(|entry| natural_cmp_paths(entry, &deleted) != Ordering::Greater);
        let start = if successor < entries.len() {
            successor
        } else {
            0
        };
        let content = match load_first(
            &entries,
            wrapping_indices(entries.len(), start, Step::Forward),
        ) {
            Some((index, image)) => Content::Gallery(Gallery {
                folder,
                entries,
                index,
                image,
            }),
            None => Content::Message(message_for_empty_folder(&folder)),
        };
        self.replace_content(content, window, cx);
    }

    fn on_random_image(&mut self, _: &RandomImage, window: &mut Window, cx: &mut Context<Self>) {
        self.show_random_image(window, cx);
    }

    /// Jumps to a randomly chosen other image in the current folder. Files
    /// that vanished or fail to decode are skipped by drawing again (spec §8).
    fn show_random_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("random image");
        let Content::Gallery(gallery) = &self.content else {
            return;
        };

        // Re-read the folder so deletions made elsewhere are noticed.
        let entries = library::list_images(&gallery.folder);
        let current = position_of(&entries, &gallery.entries, gallery.index);
        let Some((index, image)) = load_first(&entries, shuffled_others(entries.len(), current))
        else {
            log::debug!("no other image in this folder");
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

    fn on_quit(&mut self, _: &Quit, window: &mut Window, cx: &mut Context<Self>) {
        self.save_state(window, cx);
        cx.quit();
    }

    /// Left button: a single click acts on the quarter it landed in, a double
    /// click toggles fullscreen (spec §4). A single click only acts once the
    /// double-click interval has passed without a second click, so a double
    /// click never also changes the image.
    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.click_count {
            1 => {
                // A click still waiting when a new one starts (too far away to
                // pair with it) is a single click in its own right.
                if let Some((area, _)) = self.pending_click.take() {
                    self.click_area(area, window, cx);
                }
                let area = ClickArea::at(event.position, window.viewport_size());
                let wait = cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(pointer::double_click_interval())
                        .await;
                    let _ = this.update_in(cx, |view, window, cx| {
                        if let Some((area, this_task)) = view.pending_click.take() {
                            // Dropping would cancel the task that is running.
                            this_task.detach();
                            view.click_area(area, window, cx);
                        }
                    });
                });
                self.pending_click = Some((area, wait));
            }
            2 => {
                self.pending_click = None;
                log::debug!("toggling fullscreen (double click)");
                window.toggle_fullscreen();
            }
            // A third click in a row would toggle straight back.
            _ => {}
        }
    }

    /// Right button: the bottom quarters show a random image, like R. There is
    /// no right double click, so this acts at once.
    fn on_right_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let area = ClickArea::at(event.position, window.viewport_size());
        log::debug!("right click in {area:?}");
        match area {
            ClickArea::BottomLeft | ClickArea::BottomRight => self.show_random_image(window, cx),
            ClickArea::TopLeft | ClickArea::TopRight => {}
        }
    }

    /// The single-click action of each quarter, mirroring the arrow keys.
    fn click_area(&mut self, area: ClickArea, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("click in {area:?}");
        match area {
            ClickArea::TopLeft => self.step_folder(Step::Backward, window, cx),
            ClickArea::BottomLeft => self.step_image(Step::Backward, window, cx),
            ClickArea::BottomRight => self.step_image(Step::Forward, window, cx),
            ClickArea::TopRight => self.step_folder(Step::Forward, window, cx),
        }
    }

    /// Wheel down = →, wheel up = ←, one image per notch (spec §4).
    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(step) = self.wheel.feed(&event.delta) {
            self.step_image(step, window, cx);
        }
    }

    /// Moves one image within the current folder, wrapping from the last image
    /// to the first and vice versa. Files that vanished or fail to decode are
    /// skipped (spec §8).
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

        // Every other image once, starting next to the current one; the
        // current image itself is the last index and is left out.
        let others = wrapping_indices(entries.len(), current, step)
            .skip(1)
            .take(entries.len() - 1);
        let Some((index, image)) = load_first(&entries, others) else {
            log::debug!("no other image in this folder");
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
        let notice = match step {
            Step::Backward => "前のフォルダはありません",
            Step::Forward => "次のフォルダはありません",
        };
        self.display_caption(CaptionText::Notice(notice.into()), cx);
    }

    /// Swaps in new content, releasing the texture the old image held.
    fn replace_content(&mut self, content: Content, window: &mut Window, cx: &mut Context<Self>) {
        if let Content::Gallery(previous) = &self.content {
            let _ = window.drop_image(previous.image.clone());
        }
        self.content = content;
        self.show_caption(cx);
        cx.notify();
    }

    /// Shows the caption for the current image, restarting its timer.
    fn show_caption(&mut self, cx: &mut Context<Self>) {
        let Content::Gallery(gallery) = &self.content else {
            self.caption = None;
            self.caption_timer = None;
            return;
        };
        let text = CaptionText::Location {
            folder: gallery.folder.display().to_string().into(),
            file: caption_file_line(gallery).into(),
        };
        self.display_caption(text, cx);
    }

    /// Puts `text` up for `[overlay] duration_ms`, replacing whatever caption
    /// is showing and restarting the timer.
    fn display_caption(&mut self, text: CaptionText, cx: &mut Context<Self>) {
        self.caption = None;
        self.caption_timer = None;
        if self.caption_duration.is_zero() {
            return;
        }

        self.caption_showings += 1;
        self.caption = Some(Caption {
            text,
            showing: self.caption_showings,
            fading: false,
        });
        cx.notify();

        let fade = CAPTION_FADE.min(self.caption_duration);
        let hold = self.caption_duration - fade;
        self.caption_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(hold).await;
            let _ = this.update(cx, |view, cx| {
                if let Some(caption) = &mut view.caption {
                    caption.fading = true;
                    cx.notify();
                }
            });
            cx.background_executor().timer(fade).await;
            let _ = this.update(cx, |view, cx| {
                view.caption = None;
                cx.notify();
            });
        }));
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
        img(image.clone())
            .object_fit(ObjectFit::Contain)
            .size_full()
    }

    /// The caption box, centred near the bottom. It has no mouse handlers, so
    /// clicks on it fall through to the quarter underneath.
    fn render_caption(&self, caption: &Caption) -> AnyElement {
        let line = || {
            div()
                .w_full()
                .text_center()
                .whitespace_nowrap()
                .overflow_hidden()
        };
        let panel = div()
            .max_w(relative(0.9))
            .px_4()
            .py_2()
            .rounded_md()
            .bg(rgba(0x000000b3))
            .text_color(rgb(0xFFFFFF))
            .text_sm()
            .flex()
            .flex_col()
            .items_center();
        let panel = match &caption.text {
            CaptionText::Location { folder, file } => panel
                // Long paths keep their end: the folder being browsed.
                .child(line().text_ellipsis_start().child(folder.clone()))
                .child(line().text_ellipsis_middle().child(file.clone())),
            CaptionText::Notice(text) => panel.child(line().child(text.clone())),
        };

        let panel = if caption.fading {
            let fade = CAPTION_FADE.min(self.caption_duration);
            panel
                .with_animation(
                    ("caption-fade", caption.showing),
                    Animation::new(fade),
                    |panel, progress| panel.opacity(1. - progress),
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        };

        div()
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(32.))
            .flex()
            .justify_center()
            .child(panel)
            .into_any_element()
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
            .on_action(cx.listener(Self::on_delete_image))
            .on_action(cx.listener(Self::on_random_image))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_right_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_action(cx.listener(Self::on_quit))
            .size_full()
            .relative()
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
            .children(
                self.caption
                    .as_ref()
                    .map(|caption| self.render_caption(caption)),
            )
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
        Step::Backward => (0..=start.min(entries.len().saturating_sub(1)))
            .rev()
            .collect(),
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

/// All indices of a listing of `len` entries, starting at `start` and
/// wrapping past either end in the direction of `step`.
fn wrapping_indices(len: usize, start: usize, step: Step) -> impl Iterator<Item = usize> {
    (0..len).map(move |offset| match step {
        Step::Forward => (start + offset) % len,
        Step::Backward => (start + len - offset) % len,
    })
}

/// Every index of a listing of `len` entries except `current`, in random
/// order.
fn shuffled_others(len: usize, current: Option<usize>) -> Vec<usize> {
    let mut others: Vec<usize> = (0..len).filter(|&index| Some(index) != current).collect();
    fastrand::shuffle(&mut others);
    others
}

/// Loads the first usable image among `indices`, skipping files that no
/// longer exist or fail to decode (spec §8).
fn load_first(
    entries: &[PathBuf],
    indices: impl IntoIterator<Item = usize>,
) -> Option<(usize, Arc<RenderImage>)> {
    indices
        .into_iter()
        .find_map(|index| try_load(&entries[index]).map(|image| (index, image)))
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

/// "name.png    3 / 25": the file and where it sits in the folder.
fn caption_file_line(gallery: &Gallery) -> String {
    let name = gallery.entries[gallery.index]
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(
        "{name}    {} / {}",
        gallery.index + 1,
        gallery.entries.len()
    )
}

fn message_for_missing(path: &Path) -> SharedString {
    format!("画像を表示できませんでした。\n{}", path.display()).into()
}

fn message_for_empty_folder(folder: &Path) -> SharedString {
    format!("表示できる画像がありません。\n{}", folder.display()).into()
}

#[cfg(test)]
mod tests {
    // Deliberately not `use super::*`: that would re-export GPUI's own `test`
    // attribute over Rust's.
    use super::{
        load_first, load_nearest, load_scanning, position_of, shuffled_others, wrapping_indices,
    };
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
    fn wrapping_forward_continues_from_the_first_index() {
        let order: Vec<usize> = wrapping_indices(4, 2, Step::Forward).collect();
        assert_eq!(order, [2, 3, 0, 1]);
    }

    #[test]
    fn wrapping_backward_continues_from_the_last_index() {
        let order: Vec<usize> = wrapping_indices(4, 1, Step::Backward).collect();
        assert_eq!(order, [1, 0, 3, 2]);
    }

    #[test]
    fn wrapping_an_empty_listing_yields_nothing() {
        assert_eq!(wrapping_indices(0, 0, Step::Forward).count(), 0);
    }

    #[test]
    fn stepping_past_the_last_image_wraps_to_the_first_usable_one() {
        let (_tree, entries) = folder_with_a_broken_middle("wrap-forward");
        let others = wrapping_indices(entries.len(), 2, Step::Forward).skip(1);
        let (index, _) = load_first(&entries, others).expect("an image after wrapping");
        assert_eq!(index, 0);
    }

    #[test]
    fn stepping_before_the_first_image_wraps_past_a_broken_one() {
        let (_tree, entries) = folder_with_a_broken_middle("wrap-backward");
        // ← on 1.png wraps round to 3.png.
        let others = wrapping_indices(entries.len(), 0, Step::Backward).skip(1);
        let (index, _) = load_first(&entries, others).expect("an image after wrapping");
        assert_eq!(index, 2);
    }

    #[test]
    fn shuffled_others_leaves_out_only_the_current_image() {
        let mut others = shuffled_others(5, Some(2));
        others.sort_unstable();
        assert_eq!(others, [0, 1, 3, 4]);
    }

    #[test]
    fn shuffled_others_of_a_single_image_is_empty() {
        assert!(shuffled_others(1, Some(0)).is_empty());
    }

    #[test]
    fn shuffled_others_without_a_current_image_offers_every_index() {
        let mut others = shuffled_others(3, None);
        others.sort_unstable();
        assert_eq!(others, [0, 1, 2]);
    }

    #[test]
    fn a_random_pick_skips_an_undecodable_file() {
        let (_tree, entries) = folder_with_a_broken_middle("random-broken");
        // From 1.png the only usable other image is 3.png, whatever the order.
        for _ in 0..20 {
            let (index, _) = load_first(&entries, shuffled_others(entries.len(), Some(0)))
                .expect("another image");
            assert_eq!(index, 2);
        }
    }

    #[test]
    fn position_of_tracks_a_file_that_moved_in_the_listing() {
        let previous = vec![PathBuf::from("a.png"), PathBuf::from("b.png")];
        let entries = vec![PathBuf::from("b.png")];
        assert_eq!(position_of(&entries, &previous, 1), Some(0));
        assert_eq!(position_of(&entries, &previous, 0), None);
    }
}
