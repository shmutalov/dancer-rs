//! dancer-rs — M0: window and sprite playback.
//!
//! FAOSDance parity: loads an existing sheet plus its `.txt`, loops at a fixed
//! frame rate, transparent, click-through, draggable. No clock, no analysis, no
//! sources — those land in M1 onward.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use dancer_render::{apply_window_styles, hwnd_of, present, surface_size, Surface};
use dancer_sprite::Sheet;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

mod config;
use config::{data_dir, Config};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dancer=info,dancer_app=info,dancer_sprite=info".into()),
        )
        .with_target(false)
        .init();

    let dir = data_dir();
    tracing::info!(dir = %dir.display(), "data directory");

    let cfg = Config::load(&dir);

    // A path on the command line overrides the configured sheet, which is how
    // compatibility checks against real sheets are driven without editing files.
    // It is resolved against the working directory, not the data directory —
    // anything else would surprise whoever typed it.
    let sheet_path = match std::env::args().nth(1) {
        Some(arg) => PathBuf::from(arg),
        None => cfg.sheet_path(&dir),
    };
    let sheet = Sheet::load(&sheet_path)
        .with_context(|| format!("loading sheet {}", sheet_path.display()))?;

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App {
        row: sheet.default_row,
        sheet,
        cfg,
        dir,
        window: None,
        hwnd: None,
        surface: None,
        cell: 0,
        pos: (0, 0),
        drag: None,
        row_before_drag: None,
        next_frame: Instant::now(),
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct App {
    cfg: Config,
    dir: PathBuf,
    sheet: Sheet,
    window: Option<Window>,
    hwnd: Option<HWND>,
    surface: Option<Surface>,
    row: usize,
    cell: usize,
    /// Top-left in physical screen pixels. Authoritative — `UpdateLayeredWindow`
    /// moves the window as part of presenting, so this is the position.
    pos: (i32, i32),
    /// Cursor-to-window offset captured at mouse-down.
    drag: Option<(i32, i32)>,
    row_before_drag: Option<usize>,
    next_frame: Instant,
}

impl App {
    fn frame_interval(&self) -> Duration {
        Duration::from_secs_f64(1.0 / self.cfg.sprite.fps.max(1) as f64)
    }

    fn surface_size(&self) -> (u32, u32) {
        surface_size(
            self.sheet.cell_width,
            self.sheet.cell_height,
            self.cfg.sprite.scale,
        )
    }

    /// Place the window from normalised config coordinates on the chosen monitor.
    fn initial_position(&self, el: &ActiveEventLoop, size: (u32, u32)) -> (i32, i32) {
        let mon = el
            .available_monitors()
            .nth(self.cfg.window.monitor)
            .or_else(|| el.primary_monitor());
        let (mp, ms) = mon
            .map(|m| (m.position(), m.size()))
            .unwrap_or((PhysicalPosition::new(0, 0), PhysicalSize::new(1920, 1080)));

        let span_x = (ms.width as i32 - size.0 as i32).max(0) as f32;
        let span_y = (ms.height as i32 - size.1 as i32).max(0) as f32;
        (
            mp.x + (self.cfg.window.x.clamp(0.0, 1.0) * span_x).round() as i32,
            mp.y + (self.cfg.window.y.clamp(0.0, 1.0) * span_y).round() as i32,
        )
    }

    /// Convert the current absolute position back to (monitor, normalised x/y).
    ///
    /// Stored normalised so the position survives a resolution or DPI change
    /// rather than putting the dancer off-screen (spec §12).
    fn store_position(&mut self, el: &ActiveEventLoop) {
        let size = self.surface_size();
        let mut best: Option<(usize, f32, f32)> = None;
        for (i, m) in el.available_monitors().enumerate() {
            let (mp, ms) = (m.position(), m.size());
            let inside = self.pos.0 >= mp.x
                && self.pos.1 >= mp.y
                && self.pos.0 < mp.x + ms.width as i32
                && self.pos.1 < mp.y + ms.height as i32;
            if inside {
                let span_x = (ms.width as i32 - size.0 as i32).max(1) as f32;
                let span_y = (ms.height as i32 - size.1 as i32).max(1) as f32;
                best = Some((
                    i,
                    ((self.pos.0 - mp.x) as f32 / span_x).clamp(0.0, 1.0),
                    ((self.pos.1 - mp.y) as f32 / span_y).clamp(0.0, 1.0),
                ));
                break;
            }
        }
        if let Some((i, x, y)) = best {
            self.cfg.window.monitor = i;
            self.cfg.window.x = x;
            self.cfg.window.y = y;
        }
    }

    fn draw(&mut self) {
        let (Some(hwnd), Some(surface)) = (self.hwnd, self.surface.as_mut()) else {
            return;
        };
        let row = &self.sheet.rows[self.row.min(self.sheet.rows.len() - 1)];
        let Some(cell) = row.cells.get(self.cell % row.cells.len().max(1)) else {
            return;
        };
        let (w, h) = surface_size(
            self.sheet.cell_width,
            self.sheet.cell_height,
            self.cfg.sprite.scale,
        );

        surface.clear();
        surface.blit_scaled(
            cell,
            self.sheet.cell_width,
            self.sheet.cell_height,
            w,
            h,
            self.cfg.sprite.mirror,
        );

        if let Err(e) = present(hwnd, surface, self.pos, self.cfg.sprite.opacity) {
            tracing::error!(error = %e, "present failed");
        }
    }

    fn begin_drag(&mut self) {
        let Some(cursor) = cursor_pos() else { return };
        self.drag = Some((cursor.0 - self.pos.0, cursor.1 - self.pos.1));
        // Switch to the Held row, bypassing anything else. This is the one
        // interaction that must feel immediate (spec §12).
        if let Some(held) = self.sheet.held_row {
            self.row_before_drag = Some(self.row);
            self.row = held;
            self.cell = 0;
        }
    }

    fn end_drag(&mut self) {
        self.drag = None;
        if let Some(prev) = self.row_before_drag.take() {
            self.row = prev;
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (w, h) = self.surface_size();

        let attrs = Window::default_attributes()
            .with_title("dancer-rs")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            // Must be created visible: `UpdateLayeredWindow` returns E_INVALIDARG
            // against a window winit has not shown yet. No flash results — the
            // window is transparent until the first present.
            .with_window_level(if self.cfg.window.always_on_top {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            })
            .with_inner_size(PhysicalSize::new(w, h));

        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "creating window failed");
                el.exit();
                return;
            }
        };
        let hwnd = match hwnd_of(&window) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "no Win32 handle");
                el.exit();
                return;
            }
        };

        apply_window_styles(hwnd, self.cfg.window.click_through);
        self.pos = self.initial_position(el, (w, h));
        self.surface = Surface::new(w, h).ok();
        self.hwnd = Some(hwnd);

        self.draw();
        self.window = Some(window);

        tracing::info!(
            w, h,
            pos = ?self.pos,
            fps = self.cfg.sprite.fps,
            click_through = self.cfg.window.click_through,
            "window up"
        );
        if self.cfg.window.click_through {
            tracing::warn!("click_through is on: the window ignores the mouse, so it cannot be dragged or closed by clicking");
        }

        self.next_frame = Instant::now() + self.frame_interval();
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.store_position(el);
                let _ = self.cfg.save(&self.dir);
                el.exit();
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.begin_drag(),
                (MouseButton::Left, ElementState::Released) => self.end_drag(),
                // M0 has no tray yet, and WS_EX_NOACTIVATE means no keyboard —
                // so right-click is the only way out. Replaced by the tray in M5.
                (MouseButton::Right, ElementState::Pressed) => {
                    self.store_position(el);
                    let _ = self.cfg.save(&self.dir);
                    el.exit();
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        // While dragging, track the cursor in screen coordinates. Window-relative
        // events are useless here because the window moves with the pointer.
        if let (Some(off), Some(cursor)) = (self.drag, cursor_pos()) {
            let want = (cursor.0 - off.0, cursor.1 - off.1);
            if want != self.pos {
                self.pos = want;
                self.draw();
            }
        }

        let now = Instant::now();
        if now >= self.next_frame {
            let cells = self.sheet.cells_per_row().max(1);
            self.cell = (self.cell + 1) % cells;
            self.draw();
            // Advance from the scheduled time, not from now, so the loop does not
            // drift slower than the configured rate.
            self.next_frame += self.frame_interval();
            if self.next_frame < now {
                self.next_frame = now + self.frame_interval();
            }
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}
