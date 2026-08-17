//! Giving the command-line paths a console to print to.
//!
//! # The bug this exists to fix
//!
//! The binary is built `windows_subsystem = "windows"` in release, because the
//! dancer is a desktop sprite and a console window flashing up behind it would be
//! wrong. That was decided in M0, when the only interface *was* the sprite.
//!
//! M2 through M4 then added `--scan`, `--write-config` and `--yandex-login`, all of
//! which talk to the user through `println!`. A GUI-subsystem process launched
//! from Explorer has no console at all, and one launched from a shell is not
//! attached to it in the way a console program is — so the output goes nowhere and
//! the shell does not wait. `--yandex-login` was the worst case: it printed a code
//! nobody could see, then blocked politely for a confirmation that could never
//! come, and Ctrl+C had no console control handler to arrive through.
//!
//! # What this does
//!
//! Attaches to the launching terminal when there is one, and allocates a console
//! when there is not, then rebinds the standard handles so `println!` and `tracing`
//! reach it. Called only for command-line invocations; running the dancer stays
//! silent and window-only.

#[cfg(windows)]
pub fn attach() {
    use windows::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{
        AllocConsole, AttachConsole, GetConsoleWindow, GetStdHandle, SetStdHandle,
        ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    use windows::core::w;

    unsafe {
        // Already have one — a debug build, or a shell that gave us its console.
        if !GetConsoleWindow().is_invalid() {
            return;
        }
        // Prefer the terminal the user typed into; only conjure a window if there
        // isn't one, so output lands where they are looking.
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() && AllocConsole().is_err() {
            return;
        }

        // The standard handles were fixed when the process started, and for a GUI
        // subsystem process they are typically null. Rebind them to the console
        // that now exists, or nothing printed will arrive.
        let open = |name: windows::core::PCWSTR, access: u32| -> Option<HANDLE> {
            CreateFileW(
                name,
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
            .ok()
            .filter(|h| !h.is_invalid())
        };

        // **Only replace a handle that is not already going somewhere.** Rebinding
        // unconditionally sends output to the console even when the caller
        // redirected it, so `dancer-rs --help > file` and `| grep` silently
        // produce nothing — which is how this was first written, and it broke
        // exactly that.
        let unset = |which| GetStdHandle(which).map(|h| h.is_invalid()).unwrap_or(true);

        if unset(STD_OUTPUT_HANDLE) || unset(STD_ERROR_HANDLE) {
            if let Some(out) = open(w!("CONOUT$"), GENERIC_READ.0 | GENERIC_WRITE.0) {
                if unset(STD_OUTPUT_HANDLE) {
                    let _ = SetStdHandle(STD_OUTPUT_HANDLE, out);
                }
                if unset(STD_ERROR_HANDLE) {
                    let _ = SetStdHandle(STD_ERROR_HANDLE, out);
                }
            }
        }
        if unset(STD_INPUT_HANDLE) {
            if let Some(inp) = open(w!("CONIN$"), GENERIC_READ.0 | GENERIC_WRITE.0) {
                let _ = SetStdHandle(STD_INPUT_HANDLE, inp);
            }
        }
    }
}

#[cfg(not(windows))]
pub fn attach() {}
