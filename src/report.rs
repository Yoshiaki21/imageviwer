//! Telling the user about a problem when there is no console to print to.
//!
//! On Windows the viewer is a GUI-subsystem program. When Explorer starts it —
//! which is what a file association does — the process has no console at all,
//! `GetStdHandle` returns null, and `println!` / `eprintln!` **panic** with
//! "failed printing to stderr". A panic in a GUI-subsystem process produces no
//! visible output, so the program simply appears to do nothing.
//!
//! Started from `cmd.exe` the same binary inherits that console's handles and
//! the print succeeds, which is why the failure only shows up via the file
//! association. Nothing in this crate may write to stdio; every user-facing
//! problem goes through here instead.

use std::sync::OnceLock;

/// Why `viewer.log` is unavailable, if it is. Appended to dialogs so a report
/// is still complete when the log itself could not be written.
static LOG_FAILURE: OnceLock<String> = OnceLock::new();

/// Records that logging is unavailable, so error dialogs can say so.
pub fn set_log_unavailable(reason: String) {
    let _ = LOG_FAILURE.set(reason);
}

/// Reports a problem to the user and to `viewer.log`.
pub fn error(message: &str) {
    log::error!("{message}");

    let mut shown = message.to_string();
    if let Some(reason) = LOG_FAILURE.get() {
        shown.push_str(&format!("\n\n（viewer.log に記録できません: {reason}）"));
    }
    show(&shown);
}

/// Replaces the default panic hook with one that reaches the user.
///
/// Without this a panic in a GUI-subsystem process is completely silent.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_else(|| "不明な位置".to_string());

        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "詳細不明".to_string());

        log::error!("panic at {location}: {payload}");
        show(&format!(
            "imageviewer が予期しないエラーで停止しました。\n\n{payload}\n\n発生位置: {location}"
        ));
    }));
}

#[cfg(windows)]
fn show(message: &str) {
    use windows_sys::core::PCWSTR;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };

    let text = wide(message);
    let caption = wide("imageviewer");

    // SAFETY: both pointers are valid, NUL-terminated wide strings that outlive
    // the call; a null owner window makes the dialog application-modal.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr() as PCWSTR,
            caption.as_ptr() as PCWSTR,
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(not(windows))]
fn show(message: &str) {
    use std::io::Write as _;

    // Deliberately not `eprintln!`: that panics when stderr cannot be written,
    // and a viewer launched from a desktop entry may have no stderr either.
    let _ = writeln!(std::io::stderr(), "imageviewer: {message}");
}
