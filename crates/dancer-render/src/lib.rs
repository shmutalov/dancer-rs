//! Windowing and presentation (spec §12).
//!
//! winit owns window creation, events, DPI and monitors. Presentation is
//! `UpdateLayeredWindow` via the `windows` crate — **not** `softbuffer`, whose
//! pixel format is documented as `00000000RRRRRRRRGGGGGGGGBBBBBBBB` and therefore
//! cannot express per-pixel alpha at all (Phase 0.2, spec §5.2 reasoning).
//!
//! Consequence: the whole window surface is replaced by the present call, so there
//! is no `WM_PAINT` and winit's redraw path goes unused. The loop is "compose a
//! premultiplied BGRA DIB, call `UpdateLayeredWindow`".

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use windows::Win32::Foundation::{COLORREF, HWND, POINT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetCapture, VK_LBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::*;

pub mod surface;
pub use surface::{client_size, surface_size, Surface};

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("window is not a Win32 window")]
    NotWin32,
    #[error("creating the layered surface failed")]
    Surface,
    #[error("presenting failed: {0}")]
    Present(#[from] windows::core::Error),
}

/// Extract the `HWND` winit created for us.
pub fn hwnd_of(window: &Window) -> Result<HWND, RenderError> {
    match window
        .window_handle()
        .map_err(|_| RenderError::NotWin32)?
        .as_raw()
    {
        RawWindowHandle::Win32(h) => Ok(HWND(h.hwnd.get() as *mut _)),
        _ => Err(RenderError::NotWin32),
    }
}

/// Extended window styles the dancer needs (spec §12).
///
/// `LAYERED` is required by `UpdateLayeredWindow`. `TOOLWINDOW` keeps the window
/// out of Alt-Tab and the taskbar. `NOACTIVATE` stops it ever stealing focus —
/// which also means we never receive keyboard input, so interaction is mouse and
/// tray only.
pub fn apply_window_styles(hwnd: HWND, click_through: bool) {
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut ex = cur | WS_EX_LAYERED.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;
        // Click-through is a toggle, so it must be cleared as well as set —
        // dragging needs mouse messages to reach us.
        if click_through {
            ex |= WS_EX_TRANSPARENT.0;
        } else {
            ex &= !WS_EX_TRANSPARENT.0;
        }
        if ex != cur {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex as isize);
        }
    }
}

/// Is the window currently click-through?
pub fn is_click_through(hwnd: HWND) -> bool {
    unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TRANSPARENT.0 != 0 }
}

/// Which window currently holds the mouse capture, if any.
///
/// Diagnostic. If some window holds capture, every mouse message goes there
/// regardless of what is under the cursor — which is exactly the shape of "I can
/// see the dancer but clicking it does nothing, until I activate another window".
///
/// Note we deliberately do **not** call `SetCapture` ourselves: winit's Win32
/// backend already maintains a `capture_count`, captures on button-down and
/// releases at zero, and zeroes that count on `WM_CAPTURECHANGED` — which our own
/// `SetCapture` would trigger, corrupting its bookkeeping.
pub fn capture_owner(hwnd: HWND) -> CaptureOwner {
    let owner = unsafe { GetCapture() };
    if owner.0.is_null() {
        CaptureOwner::None
    } else if owner == hwnd {
        CaptureOwner::Us
    } else {
        CaptureOwner::Other(owner.0 as usize)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureOwner {
    None,
    Us,
    Other(usize),
}

/// Is the primary mouse button physically down right now?
///
/// Used as a self-healing check: capture should make a lost release impossible,
/// but a wedged drag is bad enough — the window follows the cursor forever — that
/// it is worth detecting directly rather than trusting the event stream.
///
/// Honours the left/right swap, since `VK_LBUTTON` is the physical button rather
/// than the primary one.
pub fn primary_button_down() -> bool {
    unsafe {
        let vk = if GetSystemMetrics(SM_SWAPBUTTON) != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        GetAsyncKeyState(vk.0 as i32) as u16 & 0x8000 != 0
    }
}

/// Re-assert always-on-top without activating.
///
/// `WS_EX_NOACTIVATE` means clicking never raises us, so z-order can be lost to
/// another topmost window and never regained.
pub fn reassert_topmost(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

/// Present a premultiplied BGRA buffer as the window surface.
///
/// **Pixels only — this does not move the window.** `UpdateLayeredWindow` can
/// reposition via its `pptDst` argument, and doing so is tempting because it
/// makes a drag one call instead of two. It is also a trap: winit owns window
/// geometry and caches it, so moving the window underneath leaves winit's view of
/// the world wrong, and mouse input routing stops working after enough fast
/// drags. Move with `Window::set_outer_position` and let this call carry pixels.
///
/// `opacity` is applied by the compositor via `SourceConstantAlpha`, so a global
/// fade costs no pixel work. This is where spec §13's `[sprite] opacity` lands;
/// FAOSDance pays for the same effect with a full `AlphaComposite` pass.
pub fn present(hwnd: HWND, surface: &Surface, opacity: f32) -> Result<(), RenderError> {
    unsafe {
        let screen_dc = GetDC(None);
        let mut src = POINT { x: 0, y: 0 };
        let mut size = SIZE {
            cx: surface.width() as i32,
            cy: surface.height() as i32,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: (opacity.clamp(0.0, 1.0) * 255.0).round() as u8,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let r = UpdateLayeredWindow(
            hwnd,
            Some(screen_dc),
            // NULL pptDst = leave the window where winit put it.
            None,
            Some(&mut size),
            Some(surface.dc()),
            Some(&mut src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        ReleaseDC(None, screen_dc);
        r?;
    }
    Ok(())
}
