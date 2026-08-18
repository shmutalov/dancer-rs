//! Native message boxes, for the few things that must interrupt (ROADMAP M5).
//!
//! Win32 `MessageBox` rather than a UI toolkit. The app has no widget layer and
//! does not want one — it is a sprite on a layered window — and everything that
//! needs to be *said* here is a sentence and a button.
//!
//! # Everything here blocks its calling thread
//!
//! A modal pumps its own message loop, so calling one from the winit thread stops
//! the dancer until it is dismissed. Every caller must be on a worker thread. That
//! is not a limitation to work around: sign-in takes as long as the user takes, and
//! the dancer should carry on dancing throughout.

use std::path::Path;

use windows::core::HSTRING;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDOK, MB_ICONERROR, MB_ICONINFORMATION, MB_OKCANCEL, MB_OK, MB_SETFOREGROUND,
    MB_SYSTEMMODAL, MESSAGEBOX_STYLE,
};

/// Show a message and wait for OK.
pub fn info(title: &str, text: &str) {
    show(title, text, MB_OK | MB_ICONINFORMATION);
}

/// Show an error and wait for OK.
pub fn error(title: &str, text: &str) {
    show(title, text, MB_OK | MB_ICONERROR);
}

/// Show a message with OK and Cancel. `true` means OK.
pub fn confirm(title: &str, text: &str) -> bool {
    show(title, text, MB_OKCANCEL | MB_ICONINFORMATION) == IDOK.0
}

fn show(title: &str, text: &str, style: MESSAGEBOX_STYLE) -> i32 {
    // `MB_SETFOREGROUND` because the app is a tool window that never takes focus —
    // without it a dialog raised from a background thread can open behind whatever
    // the user is looking at, which for a sign-in code is the same as not showing
    // it. `MB_SYSTEMMODAL` keeps it on top of the always-on-top sprite.
    let style = style | MB_SETFOREGROUND | MB_SYSTEMMODAL;
    // Null owner: these are raised from worker threads, and owning them to the
    // sprite window would let a modal outlive its owner during shutdown.
    unsafe { MessageBoxW(Some(HWND(std::ptr::null_mut())), &HSTRING::from(text), &HSTRING::from(title), style).0 }
}

/// Open a URL in the user's default browser.
///
/// Via `explorer` rather than `ShellExecuteW`: it is the same call underneath, it
/// needs no extra Win32 feature, and a failure to launch is not worth a crash.
pub fn open_url(url: &str) {
    // Only http(s). This takes a string that ultimately came from a network
    // response, and handing an arbitrary scheme to the shell is how a URL becomes
    // a command.
    if !url.starts_with("https://") && !url.starts_with("http://") {
        tracing::warn!(url, "refusing to open a non-http URL");
        return;
    }
    if let Err(e) = std::process::Command::new("explorer").arg(url).spawn() {
        tracing::warn!(error = %e, url, "could not open the browser");
    }
}

/// Open a folder in the file manager.
pub fn open_dir(dir: &Path) {
    // `explorer` returns a non-zero exit code even on success, so its status is
    // deliberately not checked — only a spawn failure is worth reporting.
    if let Err(e) = std::process::Command::new("explorer").arg(dir.as_os_str()).spawn() {
        tracing::warn!(error = %e, dir = %dir.display(), "could not open the folder");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_urls_are_handed_to_the_shell() {
        // These strings arrive from a network response. `explorer` will happily run
        // a great deal more than a web page.
        for bad in [
            "file:///C:/Windows/System32/cmd.exe",
            "C:/Windows/System32/cmd.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "",
        ] {
            // No assertion beyond "does not launch anything": the guard is a `return`
            // before the spawn, and a test that shelled out would be worse than none.
            open_url(bad);
        }
    }
}
