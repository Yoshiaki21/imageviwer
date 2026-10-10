//! The window contents: one image, letterboxed on black (spec §5), driven by
//! the keyboard and mouse (spec §4), with a caption naming the folder and file
//! for a few seconds after each change.

use std::cmp::Ordering;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use crate::config::{Config, LastOpened, PrefetchConfig, WindowConfig};
use crate::folder_history::FolderHistory;
use crate::library::{self, Step};
use crate::natural_sort::natural_cmp_paths;
use crate::pointer::{self, ClickArea, WheelNotches};
use crate::prefetch::{self, Decoded, FileStamp, Lookup, Outcome, Prefetcher};
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
        RefreshListing,
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
        KeyBinding::new("f5", RefreshListing, Some(KEY_CONTEXT)),
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
    /// Natural-ordered listing, read on entering the folder and kept while
    /// moving within it. It is re-read when a file in it turns out to be gone
    /// and on F5; files added meanwhile only show up after that.
    entries: Vec<PathBuf>,
    /// The cursor: the image last moved to. Ahead of `shown` while a move is
    /// still waiting for its decode.
    index: usize,
    /// The image on screen, and the file it came from.
    image: Arc<RenderImage>,
    shown: PathBuf,
}

impl Gallery {
    fn new(folder: PathBuf, entries: Vec<PathBuf>, index: usize, image: Arc<RenderImage>) -> Self {
        let shown = entries[index].clone();
        Self {
            folder,
            entries,
            index,
            image,
            shown,
        }
    }
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
    /// Decoded images around the cursor, decoded on background threads.
    prefetch: Prefetcher,
    prefetch_ahead: usize,
    prefetch_behind: usize,
    /// The way the last move went, which is the way prefetching looks ahead.
    direction: Step,
    /// A move waiting for the decode of the image it is heading for.
    seek: Option<Seek>,
    /// Images taken off screen that nothing keeps any more; their textures
    /// are freed by the next `release_textures`.
    orphaned: Vec<Arc<RenderImage>>,
    /// The image last shown in each folder left so far, for coming back.
    folder_history: FolderHistory,
}

impl ViewerView {
    pub fn new(
        startup: Startup,
        caption_duration: Duration,
        prefetch: &PrefetchConfig,
        folder_history: FolderHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let content = match startup {
            Startup::Image(path) => open_file(&path, &folder_history),
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
            prefetch: Prefetcher::new(prefetch.memory_bytes(), prefetch::default_parallelism()),
            prefetch_ahead: prefetch.ahead,
            prefetch_behind: prefetch.behind,
            direction: Step::Forward,
            seek: None,
            orphaned: Vec::new(),
            folder_history,
        };
        view.content_changed(window, cx);
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
        let content = open_file(&path, &self.folder_history);
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

        // Entries come from this session, the limit from the file, so that an
        // edit made while running takes effect.
        config.folder_history.entries = self.folder_history.entries.clone();
        if let Content::Gallery(gallery) = &self.content {
            config
                .folder_history
                .remember(&gallery.folder, &gallery.shown);

            // The image the user last saw, not a move still waiting for it.
            let index = gallery
                .entries
                .iter()
                .position(|entry| *entry == gallery.shown)
                .unwrap_or(gallery.index);
            config.last_opened = Some(LastOpened {
                folder: gallery.folder.clone(),
                index,
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
        let Content::Gallery(gallery) = &mut self.content else {
            return;
        };
        // The image the user is looking at, even if a move away from it is
        // still waiting for its decode.
        let deleted = gallery.shown.clone();
        let Some(index) = gallery.entries.iter().position(|entry| *entry == deleted) else {
            return;
        };

        if let Err(error) = trash::delete(&deleted) {
            report::error(&format!(
                "ごみ箱に移動できませんでした。\n{}\n\n{error}",
                deleted.display()
            ));
            return;
        }
        log::debug!("moved {} to the trash", deleted.display());

        // The listing held is still right apart from the deleted file, so
        // take that out rather than re-reading the folder.
        gallery.entries.remove(index);
        let order = candidates(Pick::Successor, &gallery.entries, &deleted, index);
        self.prefetch.forget(&deleted);
        self.direction = Step::Forward;
        self.seek = Some(Seek {
            pick: Pick::Successor,
            anchor: deleted,
            anchor_index: index,
            order,
            reread: false,
        });
        self.resolve(window, cx);
    }

    /// F5: re-reads the folder, for files added since it was entered. Every
    /// decode is dropped, so the image on screen is decoded again too in case
    /// it was overwritten.
    fn on_refresh_listing(
        &mut self,
        _: &RefreshListing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Content::Gallery(gallery) = &mut self.content else {
            return;
        };
        log::debug!("refreshing {}", gallery.folder.display());

        // If the image on screen is gone, its old index is where to look.
        let anchor = gallery.shown.clone();
        let anchor_index = gallery
            .entries
            .iter()
            .position(|entry| *entry == anchor)
            .unwrap_or(gallery.index);
        gallery.entries = library::list_images(&gallery.folder);
        let order = candidates(Pick::Nearest, &gallery.entries, &anchor, anchor_index);
        self.prefetch.clear();
        self.seek = Some(Seek {
            pick: Pick::Nearest,
            anchor,
            anchor_index,
            order,
            reread: true,
        });
        self.resolve(window, cx);
    }

    fn on_random_image(&mut self, _: &RandomImage, window: &mut Window, cx: &mut Context<Self>) {
        self.show_random_image(window, cx);
    }

    /// Jumps to a randomly chosen other image in the current folder. Files
    /// that fail to decode are skipped by drawing again (spec §8).
    fn show_random_image(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("random image");
        self.start_seek(Pick::Random, window, cx);
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
        self.direction = step;
        self.start_seek(Pick::Step(step), window, cx);
    }

    /// Starts a move within the current folder, measured from the cursor: the
    /// image last moved to, which is ahead of the one on screen while its
    /// decode is still running. A move made while another is waiting replaces
    /// it, so holding a key down always heads for the latest target.
    fn start_seek(&mut self, pick: Pick, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Gallery(gallery) = &self.content else {
            return;
        };
        let anchor = gallery.entries[gallery.index].clone();
        let order = candidates(pick, &gallery.entries, &anchor, gallery.index);
        self.seek = Some(Seek {
            pick,
            anchor,
            anchor_index: gallery.index,
            order,
            reread: false,
        });
        self.resolve(window, cx);
    }

    /// Works through the pending move's candidates: shows the first one that
    /// is decoded, skips corrupt ones, and waits for a decode when it reaches
    /// one that is not ready yet ([`Self::on_decoded`] resumes it).
    ///
    /// The listing read on entering the folder is reused throughout. A listed
    /// file that no longer exists means the folder has changed: the listing is
    /// then re-read once and the candidates worked out again from it, which
    /// looks the same to the user as re-reading on every move.
    fn resolve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(mut seek) = self.seek.take() {
            while let Content::Gallery(gallery) = &mut self.content {
                let Some(&index) = seek.order.front() else {
                    self.settle_without_moving(window, cx);
                    break;
                };
                // The cursor follows the candidate, so the images around it
                // are the ones decoded next.
                gallery.index = index;
                let path = gallery.entries[index].clone();
                self.update_wanted();

                match self.prefetch.lookup(&path) {
                    Lookup::Image(image) => {
                        self.show(image, cx);
                        break;
                    }
                    Lookup::Unusable => {
                        seek.order.pop_front();
                    }
                    Lookup::Missing if !seek.reread => {
                        log::debug!("{} is gone; re-reading the folder", path.display());
                        let Content::Gallery(gallery) = &mut self.content else {
                            break;
                        };
                        gallery.entries = library::list_images(&gallery.folder);
                        seek.order = candidates(
                            seek.pick,
                            &gallery.entries,
                            &seek.anchor,
                            seek.anchor_index,
                        );
                        seek.reread = true;
                    }
                    Lookup::Missing => {
                        seek.order.pop_front();
                    }
                    Lookup::Absent => {
                        self.seek = Some(seek);
                        break;
                    }
                }
            }
        }
        self.release_textures(window);
        self.schedule_decodes(window, cx);
    }

    /// The pending move found nothing to show: the image on screen stays if it
    /// is still in the folder, otherwise the folder is as good as empty.
    fn settle_without_moving(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Content::Gallery(gallery) = &mut self.content else {
            return;
        };
        match gallery
            .entries
            .iter()
            .position(|entry| *entry == gallery.shown)
        {
            Some(index) => {
                log::debug!("no other image in this folder");
                gallery.index = index;
                self.update_wanted();
            }
            None => {
                let message = message_for_empty_folder(&gallery.folder);
                self.replace_content(Content::Message(message), window, cx);
            }
        }
    }

    /// Puts the image at the cursor on screen, with its caption.
    fn show(&mut self, image: Arc<RenderImage>, cx: &mut Context<Self>) {
        let Content::Gallery(gallery) = &mut self.content else {
            return;
        };
        gallery.shown = gallery.entries[gallery.index].clone();
        log::debug!("showing {}", gallery.shown.display());
        let previous = std::mem::replace(&mut gallery.image, image);
        // The old texture stays in the atlas while the image is still kept
        // decoded, so that coming back to it is instant; otherwise it is freed
        // with the next `release_textures`.
        if !self.prefetch.holds(&previous) && !Arc::ptr_eq(&previous, &gallery.image) {
            self.orphaned.push(previous);
        }
        self.show_caption(cx);
        cx.notify();
    }

    /// Tells the prefetcher which files to keep decoded: the cursor's
    /// neighbourhood in the current folder, or nothing.
    fn update_wanted(&mut self) {
        let wanted = match &self.content {
            Content::Gallery(gallery) => prefetch::window_indices(
                gallery.entries.len(),
                gallery.index,
                self.direction,
                self.prefetch_ahead,
                self.prefetch_behind,
            )
            .into_iter()
            .map(|index| gallery.entries[index].clone())
            .collect(),
            Content::Message(_) => Vec::new(),
        };
        self.prefetch.set_wanted(wanted);
    }

    /// Starts background decodes while there are free slots and wanted files
    /// not decoded yet.
    fn schedule_decodes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some(path) = self.prefetch.next_to_decode() {
            let job = cx.background_executor().spawn({
                let path = path.clone();
                async move { prefetch::decode(&path) }
            });
            // Never cancelled: a decode that is no longer wanted when it lands
            // is simply dropped by the prefetcher, and the slot it held stays
            // accounted for until then.
            cx.spawn_in(window, async move |this, cx| {
                let decoded = job.await;
                let _ = this.update_in(cx, |view, window, cx| {
                    view.on_decoded(path, decoded, window, cx);
                });
            })
            .detach();
        }
    }

    fn on_decoded(
        &mut self,
        path: PathBuf,
        decoded: Decoded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefetch.finish(path, decoded);
        // A pending move may have been waiting for exactly this decode.
        self.resolve(window, cx);
    }

    /// Frees the GPU textures of images that are neither on screen nor kept
    /// decoded any more.
    fn release_textures(&mut self, window: &mut Window) {
        let shown = match &self.content {
            Content::Gallery(gallery) => Some(gallery.image.clone()),
            Content::Message(_) => None,
        };
        let released = self.prefetch.take_released();
        for image in self.orphaned.drain(..).chain(released) {
            if shown
                .as_ref()
                .is_some_and(|shown| Arc::ptr_eq(shown, &image))
            {
                continue;
            }
            let _ = window.drop_image(image);
        }
    }

    /// Moves to the previous/next sibling folder that holds images and shows
    /// the image last seen there, or its first image (spec §4).
    fn step_folder(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        log::debug!("step folder {step:?}");
        let Content::Gallery(gallery) = &self.content else {
            return;
        };

        let mut from = gallery.folder.clone();
        while let Some(folder) = library::next_folder_with_images(&from, step) {
            let entries = library::list_images(&folder);
            let remembered = self.folder_history.recall(&folder);
            if let Some((index, image)) = load_remembered(&entries, remembered) {
                self.replace_content(
                    Content::Gallery(Gallery::new(folder, entries, index, image)),
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

    /// Swaps in new content decoded on the spot (startup, ↑ / ↓, a launch
    /// request), dropping any pending move, and starts prefetching around it.
    fn replace_content(&mut self, content: Content, window: &mut Window, cx: &mut Context<Self>) {
        self.seek = None;
        self.direction = Step::Forward;
        if let Content::Gallery(previous) = &self.content {
            self.folder_history
                .remember(&previous.folder, &previous.shown);
            if !self.prefetch.holds(&previous.image) {
                self.orphaned.push(previous.image.clone());
            }
        }
        self.content = content;
        self.content_changed(window, cx);
    }

    /// After the content was set without going through the prefetcher: lets
    /// it know about the image on screen and what to decode around it.
    fn content_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.update_wanted();
        if let Content::Gallery(gallery) = &self.content {
            // Kept, so that coming back to it needs no second decode.
            self.prefetch.insert(
                gallery.shown.clone(),
                Decoded {
                    outcome: Outcome::Image(gallery.image.clone()),
                    stamp: FileStamp::read(&gallery.shown).ok(),
                },
            );
        }
        self.release_textures(window);
        self.schedule_decodes(window, cx);
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

    /// Opens `folder` at the saved index, or the nearest usable image to it
    /// (spec §8.3).
    fn open_folder_at(folder: &Path, index: usize) -> Content {
        gallery_near(folder.to_path_buf(), library::list_images(folder), index)
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
            .on_action(cx.listener(Self::on_refresh_listing))
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

/// A move within the folder that is still looking for an image to show.
struct Seek {
    pick: Pick,
    /// The file the move is measured from (for Delete, the deleted file), so
    /// the candidates can be worked out again after re-reading the listing.
    anchor: PathBuf,
    /// Where `anchor` was, for when it is no longer in the listing.
    anchor_index: usize,
    /// Listing indices still to try, in order.
    order: VecDeque<usize>,
    /// Whether the listing was already re-read for this move. It is re-read
    /// at most once; after that a missing file is simply skipped.
    reread: bool,
}

/// Which images a move tries, in what order.
#[derive(Clone, Copy, Debug)]
enum Pick {
    /// ← / →: every other image once, wrapping (spec §4).
    Step(Step),
    /// R: every other image once, in random order.
    Random,
    /// After Delete: the image that followed the deleted one, wrapping.
    Successor,
    /// F5: the image on screen again, else the one nearest to it.
    Nearest,
}

/// The listing indices a move tries, in order.
///
/// Measured from `anchor` wherever it now sits in `entries`. If it is no
/// longer listed, the move lands on the image nearest to where it was,
/// preferring the earlier one (spec §8.3).
fn candidates(
    pick: Pick,
    entries: &[PathBuf],
    anchor: &Path,
    anchor_index: usize,
) -> VecDeque<usize> {
    let len = entries.len();
    if len == 0 {
        return VecDeque::new();
    }
    if let Pick::Successor = pick {
        // The first file sorting after the deleted one is its successor.
        let successor =
            entries.partition_point(|entry| natural_cmp_paths(entry, anchor) != Ordering::Greater);
        let start = if successor < len { successor } else { 0 };
        return wrapping_indices(len, start, Step::Forward).collect();
    }

    let Some(position) = entries.iter().position(|entry| entry == anchor) else {
        return nearest_indices(len, anchor_index).collect();
    };
    match pick {
        // The anchor itself comes last round and is left out.
        Pick::Step(step) => wrapping_indices(len, position, step).skip(1).collect(),
        Pick::Random => shuffled_others(len, position).into(),
        Pick::Nearest => nearest_indices(len, position).collect(),
        Pick::Successor => unreachable!("handled above"),
    }
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
    load_first(entries, indices)
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
fn shuffled_others(len: usize, current: usize) -> Vec<usize> {
    let mut others: Vec<usize> = (0..len).filter(|&index| index != current).collect();
    fastrand::shuffle(&mut others);
    others
}

/// Every index of a listing of `len` entries, nearest to `target` first and
/// the lower index first on a tie (spec §8.3). A `target` past the end counts
/// as the last index.
fn nearest_indices(len: usize, target: usize) -> impl Iterator<Item = usize> {
    let target = target.min(len.saturating_sub(1));
    (0..len).flat_map(move |distance| {
        let lower = target.checked_sub(distance);
        let upper = Some(target + distance).filter(|&index| distance > 0 && index < len);
        lower.into_iter().chain(upper)
    })
}

/// Opens the folder containing `path` and shows `path` itself, or the nearest
/// usable image if it cannot be displayed. A folder path opens at the image
/// last seen there, as ↑ / ↓ would.
fn open_file(path: &Path, folder_history: &FolderHistory) -> Content {
    if path.is_dir() {
        let entries = library::list_images(path);
        return match load_remembered(&entries, folder_history.recall(path)) {
            Some((index, image)) => {
                Content::Gallery(Gallery::new(path.to_path_buf(), entries, index, image))
            }
            None => Content::Message(message_for_empty_folder(path)),
        };
    }

    let Some(folder) = path.parent().map(PathBuf::from) else {
        return Content::Message(message_for_missing(path));
    };

    let entries = library::list_images(&folder);
    let target = entries.iter().position(|entry| entry == path).unwrap_or(0);

    match load_nearest(&entries, target) {
        Some((index, image)) => Content::Gallery(Gallery::new(folder, entries, index, image)),
        None => Content::Message(message_for_missing(path)),
    }
}

/// Loads the image named `remembered` if it is listed and usable, otherwise
/// the first usable image: a remembered image that is gone sends the user to
/// the start of the folder, not next to where it was (spec §4).
fn load_remembered(
    entries: &[PathBuf],
    remembered: Option<&str>,
) -> Option<(usize, Arc<RenderImage>)> {
    let remembered = remembered.and_then(|name| {
        entries
            .iter()
            .position(|entry| entry.file_name().is_some_and(|file| file == name))
    });
    remembered
        .and_then(|index| try_load(&entries[index]).map(|image| (index, image)))
        .or_else(|| load_scanning(entries, 0, Step::Forward))
}

/// A gallery of `entries` showing the usable image nearest to `target`, or
/// the empty-folder message if there is none.
fn gallery_near(folder: PathBuf, entries: Vec<PathBuf>, target: usize) -> Content {
    match load_nearest(&entries, target) {
        Some((index, image)) => Content::Gallery(Gallery::new(folder, entries, index, image)),
        None => Content::Message(message_for_empty_folder(&folder)),
    }
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
    load_first(entries, nearest_indices(entries.len(), target))
}

/// Decodes one file on the spot, treating any failure as "this image is not
/// there".
fn try_load(path: &Path) -> Option<Arc<RenderImage>> {
    match prefetch::decode(path).outcome {
        Outcome::Image(image) => Some(image),
        Outcome::Missing | Outcome::Unusable => None,
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
        candidates, load_first, load_nearest, load_remembered, load_scanning, nearest_indices,
        shuffled_others, wrapping_indices, Pick,
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
    fn a_remembered_image_is_shown_again() {
        let (_tree, entries) = folder_with_a_broken_middle("remembered-shown");
        let (index, _) = load_remembered(&entries, Some("3.png")).expect("an image");
        assert_eq!(index, 2);
    }

    #[test]
    fn a_remembered_image_that_is_gone_starts_from_the_first() {
        let (_tree, entries) = folder_with_a_broken_middle("remembered-gone");
        let (index, _) = load_remembered(&entries, Some("4.png")).expect("an image");
        assert_eq!(index, 0);
    }

    #[test]
    fn a_remembered_image_that_is_broken_starts_from_the_first() {
        let (_tree, entries) = folder_with_a_broken_middle("remembered-broken");
        let (index, _) = load_remembered(&entries, Some("2.png")).expect("an image");
        assert_eq!(index, 0, "not the nearest usable image, 3.png");
    }

    #[test]
    fn nothing_remembered_starts_from_the_first() {
        let (_tree, entries) = folder_with_a_broken_middle("remembered-none");
        let (index, _) = load_remembered(&entries, None).expect("an image");
        assert_eq!(index, 0);
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
        let mut others = shuffled_others(5, 2);
        others.sort_unstable();
        assert_eq!(others, [0, 1, 3, 4]);
    }

    #[test]
    fn shuffled_others_of_a_single_image_is_empty() {
        assert!(shuffled_others(1, 0).is_empty());
    }

    #[test]
    fn a_random_pick_skips_an_undecodable_file() {
        let (_tree, entries) = folder_with_a_broken_middle("random-broken");
        // From 1.png the only usable other image is 3.png, whatever the order.
        for _ in 0..20 {
            let (index, _) =
                load_first(&entries, shuffled_others(entries.len(), 0)).expect("another image");
            assert_eq!(index, 2);
        }
    }

    fn listing(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    fn order(pick: Pick, entries: &[PathBuf], anchor: &str, anchor_index: usize) -> Vec<usize> {
        candidates(pick, entries, &PathBuf::from(anchor), anchor_index).into()
    }

    #[test]
    fn stepping_tries_every_other_image_once_from_the_anchor() {
        let entries = listing(&["a", "b", "c", "d"]);
        assert_eq!(
            order(Pick::Step(Step::Forward), &entries, "c", 2),
            [3, 0, 1]
        );
        assert_eq!(
            order(Pick::Step(Step::Backward), &entries, "a", 0),
            [3, 2, 1]
        );
    }

    #[test]
    fn stepping_follows_the_anchor_to_where_it_now_sits() {
        // "c" was at 2; after "a" vanished and the listing was re-read it is
        // at 1, and stepping carries on from there.
        let entries = listing(&["b", "c", "d"]);
        assert_eq!(order(Pick::Step(Step::Forward), &entries, "c", 2), [2, 0]);
    }

    #[test]
    fn a_vanished_anchor_lands_on_the_nearest_image() {
        let entries = listing(&["a", "b", "d", "e"]);
        // "c" was at 2: 2 ("d") is nearest, then 1 before 3.
        assert_eq!(
            order(Pick::Step(Step::Forward), &entries, "c", 2),
            [2, 1, 3, 0]
        );
    }

    #[test]
    fn a_random_move_offers_every_other_image() {
        let entries = listing(&["a", "b", "c"]);
        let mut others = order(Pick::Random, &entries, "b", 1);
        others.sort_unstable();
        assert_eq!(others, [0, 2]);
    }

    #[test]
    fn after_delete_the_successor_comes_first_then_wraps() {
        // "c" was deleted from a, b, c, d, e.
        let entries = listing(&["a", "b", "d", "e"]);
        assert_eq!(order(Pick::Successor, &entries, "c", 2), [2, 3, 0, 1]);
        // Deleting the last image wraps to the first.
        let entries = listing(&["a", "b"]);
        assert_eq!(order(Pick::Successor, &entries, "c", 2), [0, 1]);
    }

    #[test]
    fn refreshing_tries_the_image_on_screen_first() {
        let entries = listing(&["a", "b", "c", "d"]);
        assert_eq!(order(Pick::Nearest, &entries, "c", 0), [2, 1, 3, 0]);
    }

    #[test]
    fn an_empty_listing_has_no_candidates() {
        for pick in [
            Pick::Step(Step::Forward),
            Pick::Random,
            Pick::Successor,
            Pick::Nearest,
        ] {
            assert!(order(pick, &[], "a", 0).is_empty());
        }
    }

    #[test]
    fn nearest_indices_prefer_the_lower_index_on_a_tie() {
        let indices: Vec<usize> = nearest_indices(5, 2).collect();
        assert_eq!(indices, [2, 1, 3, 0, 4]);
        let clamped: Vec<usize> = nearest_indices(3, 9).collect();
        assert_eq!(clamped, [2, 1, 0]);
    }
}
