//! Phase 0.2 — per-pixel alpha probe.
//!
//! ROADMAP.md §0.2 asked whether winit's transparency path gives real per-pixel
//! alpha. Reading softbuffer 0.4.8 answered the softbuffer half by contract: its
//! documented pixel format is `00000000RRRRRRRRGGGGGGGGBBBBBBBB` — the top 8 bits
//! are specified as zero, there is no alpha channel — and the Win32 backend
//! presents with `BitBlt(SRCCOPY)`, an opaque copy. So softbuffer is out.
//!
//! This probe tests the replacement: winit for windowing, `UpdateLayeredWindow`
//! for presentation. It measures the result rather than eyeballing it — capture
//! the screen region, show a known alpha ramp over it, capture again, and check
//! the composited pixels against the blend the ramp should have produced.

use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use windows::Win32::Foundation::{COLORREF, HWND, POINT};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::WindowsAndMessaging::*;

const X: i32 = 300;
const Y: i32 = 300;
const W: i32 = 400;
const H: i32 = 200;

/// Alpha values across the five test bands.
const ALPHAS: [u8; 5] = [0, 64, 128, 192, 255];
/// Band colour, straight (non-premultiplied).
const C: (u8, u8, u8) = (255, 0, 0);

fn main() {
    let event_loop = EventLoop::new().expect("event loop");
    event_loop.set_control_flow(ControlFlow::Wait);
    let mut app = App { window: None, done: false, background: Vec::new() };
    event_loop.run_app(&mut app).expect("run");
}

struct App {
    window: Option<Window>,
    done: bool,
    background: Vec<u32>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        // Capture the backdrop BEFORE the window covers it.
        self.background = capture(X, Y, W, H);

        let attrs = Window::default_attributes()
            .with_title("alpha-probe")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_inner_size(PhysicalSize::new(W as u32, H as u32))
            .with_position(PhysicalPosition::new(X, Y));
        let window = el.create_window(attrs).expect("window");
        self.window = Some(window);
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            std::process::exit(0);
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if self.done {
            return;
        }
        self.done = true;

        let hwnd = hwnd_of(self.window.as_ref().unwrap());
        apply_styles(hwnd);
        paint_layered(hwnd);

        // Let DWM compose before reading the screen back.
        std::thread::sleep(Duration::from_millis(700));
        let after = capture(X, Y, W, H);

        report(&self.background, &after, hwnd);
        bench(hwnd);
        el.exit();
    }
}

fn hwnd_of(window: &Window) -> HWND {
    match window.window_handle().unwrap().as_raw() {
        RawWindowHandle::Win32(h) => HWND(h.hwnd.get() as *mut _),
        _ => panic!("not win32"),
    }
}

/// The extended styles §12 asks for, plus WS_EX_LAYERED which
/// `UpdateLayeredWindow` requires.
fn apply_styles(hwnd: HWND) {
    unsafe {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let want = cur
            | WS_EX_LAYERED.0
            | WS_EX_TOOLWINDOW.0
            | WS_EX_NOACTIVATE.0
            | WS_EX_TRANSPARENT.0; // click-through
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, want as isize);
    }
}

/// Build a premultiplied BGRA bitmap of five alpha bands and hand it to
/// `UpdateLayeredWindow`.
fn paint_layered(hwnd: HWND) {
    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));

        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: W,
                biHeight: -H, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(Some(mem_dc), &header, DIB_RGB_COLORS, &mut bits, None, 0)
            .expect("CreateDIBSection");
        let old = SelectObject(mem_dc, dib.into());

        let px = std::slice::from_raw_parts_mut(bits as *mut u32, (W * H) as usize);
        let band_w = W / ALPHAS.len() as i32;
        for y in 0..H {
            for x in 0..W {
                let band = ((x / band_w) as usize).min(ALPHAS.len() - 1);
                let a = ALPHAS[band] as u32;
                // Premultiplied BGRA, as UpdateLayeredWindow requires.
                let r = C.0 as u32 * a / 255;
                let g = C.1 as u32 * a / 255;
                let b = C.2 as u32 * a / 255;
                px[(y * W + x) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }

        let mut pt_src = POINT { x: 0, y: 0 };
        let mut pt_dst = POINT { x: X, y: Y };
        let mut size = windows::Win32::Foundation::SIZE { cx: W, cy: H };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        let ok = UpdateLayeredWindow(
            hwnd,
            Some(screen_dc),
            Some(&mut pt_dst),
            Some(&mut size),
            Some(mem_dc),
            Some(&mut pt_src),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );
        println!("UpdateLayeredWindow: {:?}", ok);

        SelectObject(mem_dc, old);
        let _ = DeleteObject(dib.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
        let _ = &mut header;
    }
}

/// Grab a screen rect as 0RGB u32s.
fn capture(x: i32, y: i32, w: i32, h: i32) -> Vec<u32> {
    unsafe {
        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(Some(screen_dc));
        let bmp = CreateCompatibleBitmap(screen_dc, w, h);
        let old = SelectObject(mem_dc, bmp.into());
        let _ = BitBlt(mem_dc, 0, 0, w, h, Some(screen_dc), x, y, SRCCOPY);

        let mut header = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut buf = vec![0u32; (w * h) as usize];
        GetDIBits(
            mem_dc,
            bmp,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut _),
            &mut header,
            DIB_RGB_COLORS,
        );

        SelectObject(mem_dc, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
        buf
    }
}

fn report(before: &[u32], after: &[u32], hwnd: HWND) {
    let ex = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 };
    println!(
        "\nWS_EX: LAYERED={} TOOLWINDOW={} NOACTIVATE={} TRANSPARENT={}",
        ex & WS_EX_LAYERED.0 != 0,
        ex & WS_EX_TOOLWINDOW.0 != 0,
        ex & WS_EX_NOACTIVATE.0 != 0,
        ex & WS_EX_TRANSPARENT.0 != 0,
    );

    if before.len() != after.len() || before.is_empty() {
        println!("capture failed");
        return;
    }

    println!("\n alpha |    backdrop |    expected |    measured | delta");
    println!("-------|-------------|-------------|-------------|------");

    let band_w = W / ALPHAS.len() as i32;
    let mut worst = 0i32;
    for (i, a) in ALPHAS.iter().enumerate() {
        // Sample the middle of each band, middle row.
        let x = i as i32 * band_w + band_w / 2;
        let y = H / 2;
        let idx = (y * W + x) as usize;
        let (br, bg, bb) = rgb(before[idx]);
        let (mr, mg, mb) = rgb(after[idx]);

        let f = *a as f32 / 255.0;
        let er = (C.0 as f32 * f + br as f32 * (1.0 - f)) as i32;
        let eg = (C.1 as f32 * f + bg as f32 * (1.0 - f)) as i32;
        let eb = (C.2 as f32 * f + bb as f32 * (1.0 - f)) as i32;

        let d = (mr as i32 - er).abs().max((mg as i32 - eg).abs()).max((mb as i32 - eb).abs());
        worst = worst.max(d);
        println!(
            "  {:>3}  | {:>3},{:>3},{:>3} | {:>3},{:>3},{:>3} | {:>3},{:>3},{:>3} | {:>4}",
            a, br, bg, bb, er, eg, eb, mr, mg, mb, d
        );
    }

    println!("\nworst channel delta: {worst}");
    println!(
        "verdict: {}",
        if worst <= 8 {
            "PASS — per-pixel alpha composites correctly"
        } else if worst <= 40 {
            "MARGINAL — blending happens but is off; check colour management"
        } else {
            "FAIL — no true per-pixel alpha on this path"
        }
    );
}

/// Is `UpdateLayeredWindow` viable as the 60 Hz present path? Measured at a few
/// realistic sprite sizes: FL-Chan is 110x128, so 128 and 256 bracket normal use
/// and 512 covers a scaled-up dancer.
fn bench(hwnd: HWND) {
    println!("\npresent cost (UpdateLayeredWindow, 200 frames):");
    for dim in [128i32, 256, 512] {
        unsafe {
            let screen_dc = GetDC(None);
            let mem_dc = CreateCompatibleDC(Some(screen_dc));
            let header = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: dim,
                    biHeight: -dim,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut core::ffi::c_void = std::ptr::null_mut();
            let dib =
                CreateDIBSection(Some(mem_dc), &header, DIB_RGB_COLORS, &mut bits, None, 0).unwrap();
            let old = SelectObject(mem_dc, dib.into());
            let px = std::slice::from_raw_parts_mut(bits as *mut u32, (dim * dim) as usize);
            px.fill(0x80_40_00_00); // half-alpha, premultiplied

            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };

            const N: u32 = 200;
            let t0 = std::time::Instant::now();
            for i in 0..N {
                // Nudge position each frame so nothing can short-circuit a repaint.
                let mut pt_src = POINT { x: 0, y: 0 };
                let mut pt_dst = POINT { x: X + (i % 2) as i32, y: Y };
                let mut size = windows::Win32::Foundation::SIZE { cx: dim, cy: dim };
                let _ = UpdateLayeredWindow(
                    hwnd,
                    Some(screen_dc),
                    Some(&mut pt_dst),
                    Some(&mut size),
                    Some(mem_dc),
                    Some(&mut pt_src),
                    COLORREF(0),
                    Some(&blend),
                    ULW_ALPHA,
                );
            }
            let per = t0.elapsed().as_secs_f64() / N as f64;
            println!(
                "  {dim:>4}x{dim:<4} {:>7.3} ms/frame  ({:>6.0} fps headroom, {:.1}% of a 16.7 ms budget)",
                per * 1000.0,
                1.0 / per,
                per * 1000.0 / 16.667 * 100.0
            );

            SelectObject(mem_dc, old);
            let _ = DeleteObject(dib.into());
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
        }
    }
}

fn rgb(px: u32) -> (u8, u8, u8) {
    (((px >> 16) & 0xff) as u8, ((px >> 8) & 0xff) as u8, (px & 0xff) as u8)
}
