//! imageviewer — a single-window image viewer for Windows 11 and Linux (KDE).
//!
//! See `imageviewer_instructions.md` for the behaviour this implements.

// A GUI program should never open a console window on Windows. This is
// deliberately *not* conditional on the build profile, so debug and release
// builds start the same way; problems are reported through `report` instead
// of a console. `not(test)` keeps `cargo test` output visible.
#![cfg_attr(all(target_os = "windows", not(test)), windows_subsystem = "windows")]

mod app_paths;
mod config;
mod ipc;
mod library;
mod logging;
mod media;
mod natural_sort;
mod report;
#[cfg(test)]
mod test_support;
mod viewer;

use std::path::PathBuf;

use gpui_kit::*;
use gpui_kit::component::Root;

use crate::config::Config;
use crate::viewer::{Startup, ViewerView};

/// Lower bound for the window size (spec §7).
const MIN_WINDOW_SIZE: (f32, f32) = (400., 300.);
/// Window size used when `config.toml` has no saved geometry.
const DEFAULT_WINDOW_SIZE: (f32, f32) = (1024., 768.);

fn main() {
    // First, so that anything failing below is reported instead of vanishing.
    report::install_panic_hook();

    let requested_path = std::env::args_os().nth(1).map(PathBuf::from);

    // Logging settings come from the config file, so it is read before
    // anything that might want to log.
    let config = Config::load();
    logging::init(config.logging.enabled, config.verbose_logging());
    log::debug!("starting with argument {requested_path:?}");

    match ipc::acquire() {
        Ok(ipc::Instance::Secondary(secondary)) => {
            // A viewer is already running: hand it the path and get out of
            // the way, leaving its window exactly where it is (spec §3).
            match secondary.send(requested_path.as_deref()) {
                Ok(()) => log::debug!("handed the path to the running viewer"),
                Err(error) => report::error(&format!(
                    "起動中のビュワーに画像を渡せませんでした。\n\n{error:#}"
                )),
            }
        }
        Ok(ipc::Instance::Primary(primary)) => {
            run(Some(primary), requested_path, config);
        }
        Err(error) => {
            // Without single-instance control the viewer still works; it just
            // cannot be handed paths by later launches.
            log::error!("single-instance control unavailable: {error:#}");
            run(None, requested_path, config);
        }
    }
}

fn run(primary: Option<ipc::Primary>, requested_path: Option<PathBuf>, config: Config) {
    let startup = decide_startup(requested_path, config);

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            viewer::bind_keys(cx);

            let options = window_options(cx);
            let mut viewer: Option<Entity<ViewerView>> = None;
            let opened = cx.open_window(options, |window, cx| {
                let view = cx.new(|cx| ViewerView::new(startup, window, cx));
                viewer = Some(view.clone());
                cx.new(|cx| Root::new(view, window, cx))
            });

            let (window, viewer) = match (opened, viewer) {
                (Ok(window), Some(viewer)) => (window, viewer),
                (Err(error), _) => {
                    report::error(&format!("ウィンドウを開けませんでした。\n\n{error:#}"));
                    cx.quit();
                    return;
                }
                (Ok(_), None) => {
                    report::error("ウィンドウの初期化に失敗しました。");
                    cx.quit();
                    return;
                }
            };

            let _ = window.update(cx, |_root, window, cx| {
                window.activate_window();
                viewer.update(cx, |view, cx| view.focus(window, cx));

                let on_close = viewer.clone();
                window.on_window_should_close(cx, move |window, cx| {
                    on_close.update(cx, |view, cx| view.save_state(window, cx));
                    true
                });
            });

            // GPUI keeps running after the last window closes unless told
            // otherwise.
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            if let Some(primary) = primary {
                serve_launch_requests(primary, window, viewer, cx);
            }
        });
}

/// Bridges the IPC listener thread to the UI: the thread pushes onto a
/// channel, a foreground task drains it (spec §3).
fn serve_launch_requests(
    primary: ipc::Primary,
    window: WindowHandle<Root>,
    viewer: Entity<ViewerView>,
    cx: &mut App,
) {
    let (sender, receiver) = async_channel::unbounded::<Option<PathBuf>>();
    primary.serve(move |path| {
        if sender.send_blocking(path).is_err() {
            log::error!("the viewer is no longer accepting launch requests");
        }
    });

    cx.spawn(async move |cx| {
        while let Ok(path) = receiver.recv().await {
            let updated = window.update(cx, |_root, window, cx| {
                viewer.update(cx, |view, cx| {
                    view.handle_launch_request(path, window, cx);
                });
                // Raise the existing window without touching its geometry.
                window.activate_window();
            });
            if let Err(error) = updated {
                log::error!("cannot apply a launch request: {error:#}");
                break;
            }
        }
    })
    .detach();
}

/// Resolves what to show at startup (spec §8). Startup is always windowed
/// (spec §7), so nothing here restores a fullscreen state.
fn decide_startup(requested_path: Option<PathBuf>, mut config: Config) -> Startup {
    if let Some(path) = requested_path {
        return Startup::Image(path);
    }

    let Some(last_opened) = config.last_opened.clone() else {
        log::debug!("no last opened folder recorded");
        return Startup::Message(
            "前回開いたフォルダの記録がありません。\n画像ファイルを指定して起動してください。"
                .to_string(),
        );
    };

    let folder = last_opened.folder.clone();
    if !folder.is_dir() || !library::has_images(&folder) {
        log::error!(
            "last opened folder is unusable: {}",
            folder.display()
        );
        // The record is stale, so drop it rather than fail the same way again.
        config.clear_last_opened();
        config.save();
        return Startup::Message(format!(
            "前回開いたフォルダを表示できません。\n{}\n\n設定から記録を削除しました。",
            folder.display()
        ));
    }

    Startup::Restored {
        folder,
        index: last_opened.index,
    }
}

/// Restores the saved geometry, or centres a default-sized window (spec §7).
fn window_options(cx: &App) -> WindowOptions {
    let min_size = size(px(MIN_WINDOW_SIZE.0), px(MIN_WINDOW_SIZE.1));
    let config = Config::load();

    let bounds = match config.window {
        Some(window) => Bounds {
            origin: point(px(window.x as f32), px(window.y as f32)),
            size: size(
                px((window.width as f32).max(MIN_WINDOW_SIZE.0)),
                px((window.height as f32).max(MIN_WINDOW_SIZE.1)),
            ),
        },
        None => Bounds::centered(
            None,
            size(px(DEFAULT_WINDOW_SIZE.0), px(DEFAULT_WINDOW_SIZE.1)),
            cx,
        ),
    };

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(min_size),
        app_id: Some("imageviewer".to_string()),
        titlebar: Some(TitlebarOptions {
            title: Some("imageviewer".into()),
            ..Default::default()
        }),
        ..Default::default()
    }
}
