//! One dancer: a window, a sheet, and its own choreography.
//!
//! Extracted from `App` when the troupe arrived (`[dancers] count`). The split is
//! along what multiplies: everything here exists once *per dancer* — window,
//! surface, current row and cell, drag state, and a `Playback` of its own —
//! while `App` keeps what exists once per process: the source poll, the tray,
//! the config, the watcher.
//!
//! # Why each dancer carries a whole `Playback`
//!
//! The clock inside it is a few floats and the state machine a couple of enums;
//! duplicating them per dancer costs nothing. What *must* differ per dancer is
//! the scheduler seed — same grid, same downbeats, different move choices — and
//! the cheapest way to get that is to give each dancer the same `Playback` a
//! single one always had, seeded differently, and broadcast every source event
//! to all of them. The alternative, one shared clock with N schedulers hanging
//! off it, splits a type that was designed whole, to save memory nobody misses.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use dancer_render::{
    apply_window_styles, capture_owner, hwnd_of, present, reassert_topmost, surface_size,
    LatencyMonitor, Surface,
};
use dancer_sprite::Sheet;
use windows::Win32::Foundation::HWND;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowId, WindowLevel};

use crate::config::Config;
use crate::playback::Playback;

/// Grid-driven states re-evaluate at this rate and redraw only when the cell
/// actually changes. Cheaper than computing exact cell boundaries and immune to
/// the off-by-one errors that boundary maths invites.
pub const GRID_TICK: Duration = Duration::from_millis(8);

pub struct Dancer {
    pub sheet: Sheet,
    /// Path the sheet was loaded from, so a reload knows what to re-read.
    pub sheet_path: PathBuf,
    pub playback: Playback,
    /// This dancer's effective scale — `[sprite] scale` after per-dancer jitter,
    /// or whatever the context menu set since.
    pub scale: f32,
    pub window: Option<Window>,
    pub hwnd: Option<HWND>,
    surface: Option<Surface>,
    latency: LatencyMonitor,
    pub row: usize,
    pub cell: usize,
    /// Top-left in physical screen pixels.
    pub pos: (i32, i32),
    /// Cursor-to-window offset captured at mouse-down.
    pub drag: Option<(i32, i32)>,
    row_before_drag: Option<usize>,
    pub next_frame: Instant,
}

impl Dancer {
    pub fn new(sheet: Sheet, sheet_path: PathBuf, playback: Playback, scale: f32) -> Self {
        Self {
            row: sheet.default_row,
            sheet,
            sheet_path,
            playback,
            scale,
            window: None,
            hwnd: None,
            surface: None,
            latency: LatencyMonitor::new(GRID_TICK),
            cell: 0,
            pos: (0, 0),
            drag: None,
            row_before_drag: None,
            next_frame: Instant::now(),
        }
    }

    pub fn window_id(&self) -> Option<WindowId> {
        self.window.as_ref().map(|w| w.id())
    }

    pub fn surface_size(&self) -> (u32, u32) {
        surface_size(self.sheet.cell_width, self.sheet.cell_height, self.scale)
    }

    /// Beats one pass through the current row occupies (spec §4.2).
    pub fn beats_per_loop(&self) -> u32 {
        self.sheet
            .rows
            .get(self.row)
            .map_or(1, |r| r.beats_per_loop.max(1))
    }

    fn frame_interval(&self, cfg: &Config) -> Duration {
        // Derived from the *row*, not from a frame rate. `beats_per_loop` says what
        // a pass through this row is worth musically, so the same idle tempo gives a
        // four-beat resting pose twice the dwell of a two-beat step (spec §4.2).
        cfg.playback
            .idle_frame_interval(self.beats_per_loop(), self.sheet.cells_per_row())
    }

    /// Create this dancer's window at `pos`. Returns false if the platform said no.
    pub fn create_window(&mut self, el: &ActiveEventLoop, cfg: &Config, pos: (i32, i32)) -> bool {
        let (w, h) = self.surface_size();

        let attrs = Window::default_attributes()
            .with_title("dancer-rs")
            .with_decorations(false)
            .with_transparent(true)
            .with_resizable(false)
            // Must be created visible: `UpdateLayeredWindow` returns E_INVALIDARG
            // against a window winit has not shown yet. No flash results — the
            // window is transparent until the first present.
            .with_window_level(if cfg.window.always_on_top {
                WindowLevel::AlwaysOnTop
            } else {
                WindowLevel::Normal
            })
            .with_inner_size(PhysicalSize::new(w, h));

        let window = match el.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!(error = %e, "creating window failed");
                return false;
            }
        };
        let hwnd = match hwnd_of(&window) {
            Ok(h) => h,
            Err(e) => {
                tracing::error!(error = %e, "no Win32 handle");
                return false;
            }
        };

        apply_window_styles(hwnd, cfg.window.click_through);
        self.pos = pos;
        self.surface = Surface::new(w, h).ok();
        self.hwnd = Some(hwnd);
        window.set_outer_position(PhysicalPosition::new(pos.0, pos.1));
        self.window = Some(window);
        self.draw(cfg);
        true
    }

    pub fn draw(&mut self, cfg: &Config) {
        let (Some(hwnd), Some(surface)) = (self.hwnd, self.surface.as_mut()) else {
            return;
        };
        let row = &self.sheet.rows[self.row.min(self.sheet.rows.len() - 1)];
        let Some(cell) = row.cells.get(self.cell % row.cells.len().max(1)) else {
            return;
        };
        let (w, h) = surface_size(self.sheet.cell_width, self.sheet.cell_height, self.scale);

        surface.clear();
        surface.blit_scaled(
            cell,
            self.sheet.cell_width,
            self.sheet.cell_height,
            w,
            h,
            cfg.sprite.mirror,
        );

        // Spec §11.2 says to measure display latency rather than assume 16 ms.
        // This is the measurable half; see `dancer_render::latency` for what is
        // not, and why that is acceptable.
        let t0 = Instant::now();
        if let Err(e) = present(hwnd, surface, cfg.sprite.opacity) {
            tracing::error!(error = %e, "present failed");
        }
        self.latency.record(t0.elapsed());
        self.playback
            .set_render_latency(self.latency.render_latency().as_secs_f64());
    }

    /// Advance the animation. Returns when this dancer's next tick is due.
    ///
    /// Two regimes, and the difference is the whole milestone: `Locked` reads the
    /// cell out of the beat grid, so phase cannot drift; everything else counts
    /// frames off a timer, which is M0's behaviour and is honest about knowing no
    /// tempo (spec §10's `Unscored`).
    pub fn tick(&mut self, now: Instant, cfg: &Config) -> Instant {
        let cells = self.sheet.cells_per_row().max(1);

        // Dragging owns the sprite: the Held row is a single pose, and letting the
        // grid drive it would fight the interaction.
        if self.drag.is_none() {
            // Anticipation first: a scheduled move names both the row and the cell.
            if let Some(f) = self.playback.frame(now) {
                if f.row != self.row {
                    // Log the lead: how far ahead of its target beat this move
                    // began. That number *is* the milestone, so it should be
                    // visible rather than inferred.
                    if let Some(m) = self.playback.current_move() {
                        tracing::debug!(
                            row = %self.sheet.rows.get(f.row).map(|r| r.name.as_str()).unwrap_or("?"),
                            lead_ms = ((m.target_beat - m.start_at) * 1000.0).round(),
                            target_beat = m.target_beat,
                            "move"
                        );
                    }
                }
                if f.row != self.row || f.cell != self.cell {
                    self.row = f.row.min(self.sheet.rows.len().saturating_sub(1));
                    self.cell = f.cell;
                    self.draw(cfg);
                }
                return now + GRID_TICK;
            }
            // Nothing scheduled — a gap between moves, or anticipation off. Fall
            // back to M1: loop the default row against the grid.
            if let Some(cell) = self.playback.grid_cell(now, self.beats_per_loop(), cells) {
                if self.row != self.sheet.default_row {
                    self.row = self.sheet.default_row;
                }
                if cell != self.cell {
                    self.cell = cell;
                    self.draw(cfg);
                }
                return now + GRID_TICK;
            }
        }

        if now >= self.next_frame {
            // No grid to follow: loop the idle row at a fixed rate. This is spec
            // §10's `Unscored` — FAOSDance behaviour, honest about knowing no tempo
            // — and it is also what runs when nothing is playing at all.
            //
            // The row matters. Falling back to `default_row` looked like a bug on FL
            // Chan, whose `Waiting` pose is seven identical cells and one that
            // differs by three pixels: the loop was running the whole time and
            // rendering what appeared to be a single frame.
            if self.drag.is_none() && self.row != self.sheet.idle_row {
                self.row = self.sheet.idle_row;
                self.cell = 0;
            }
            self.cell = (self.cell + 1) % cells;
            self.draw(cfg);
            // Advance from the scheduled time, not from now, so the loop does not
            // drift slower than the configured rate.
            self.next_frame += self.frame_interval(cfg);
            if self.next_frame < now {
                self.next_frame = now + self.frame_interval(cfg);
            }
        }
        self.next_frame
    }

    pub fn begin_drag(&mut self, cursor: (i32, i32)) {
        if self.drag.is_some() {
            return;
        }
        self.drag = Some((cursor.0 - self.pos.0, cursor.1 - self.pos.1));
        tracing::debug!(
            cursor = ?cursor,
            pos = ?self.pos,
            capture = ?self.hwnd.map(capture_owner),
            "drag begin"
        );

        // Switch to the Held row, bypassing anything else. This is the one
        // interaction that must feel immediate (spec §12).
        if let Some(held) = self.sheet.held_row {
            self.row_before_drag = Some(self.row);
            self.row = held;
            self.cell = 0;
        }
    }

    pub fn end_drag(&mut self) {
        if self.drag.take().is_none() {
            return;
        }
        if let Some(prev) = self.row_before_drag.take() {
            self.row = prev;
            self.cell = 0;
        }
        // Resume on a timer boundary rather than mid-interval; the grid path
        // re-derives its own cell on the next tick regardless.
        self.next_frame = Instant::now();
        if let Some(hwnd) = self.hwnd {
            // Clicking never raises a WS_EX_NOACTIVATE window, so z-order lost
            // during the drag would otherwise never come back.
            reassert_topmost(hwnd);
        }
        tracing::debug!(
            pos = ?self.pos,
            state = self.playback.state.name(),
            "drag end"
        );
    }

    /// Move the window through winit, so winit's cached geometry stays correct.
    ///
    /// `UpdateLayeredWindow` could do this in the same call that presents pixels,
    /// but moving the window behind winit's back desyncs it and mouse input
    /// eventually stops being delivered.
    pub fn move_to(&mut self, pos: (i32, i32), cfg: &Config) {
        self.pos = pos;
        if let Some(window) = self.window.as_ref() {
            window.set_outer_position(PhysicalPosition::new(pos.0, pos.1));
        }
        self.draw(cfg);
    }

    /// Rebuild the surface after the cell size or scale changed.
    pub fn resize_surface(&mut self, cfg: &Config) {
        let (w, h) = self.surface_size();
        self.surface = Surface::new(w, h).ok();
        if let Some(win) = self.window.as_ref() {
            let _ = win.request_inner_size(PhysicalSize::new(w, h));
        }
        self.draw(cfg);
    }

    /// Set this dancer's scale. Whether it also lands in the config is the
    /// caller's decision — with a troupe on screen there is no single value it
    /// could honestly mean.
    pub fn set_scale(&mut self, scale: f32, cfg: &Config) {
        let scale = scale.clamp(0.1, 8.0);
        if (scale - self.scale).abs() < 1e-3 {
            return;
        }
        self.scale = scale;
        self.resize_surface(cfg);
    }

    /// Swap in a freshly loaded sheet.
    ///
    /// Row indices belong to the old sheet and mean nothing in the new one, and
    /// the scheduler holds a description of rows that no longer exist — both are
    /// reset here, in one place, so no caller can forget half of it.
    pub fn set_sheet(&mut self, sheet: Sheet, path: PathBuf, cfg: &Config) {
        let resize = sheet.cell_width != self.sheet.cell_width
            || sheet.cell_height != self.sheet.cell_height;

        self.row = sheet.default_row;
        self.cell = 0;
        self.row_before_drag = None;
        self.sheet = sheet;
        self.sheet_path = path;

        self.playback
            .set_rows(crate::row_info(&self.sheet), self.sheet.default_row);

        if resize {
            self.resize_surface(cfg);
        } else {
            self.draw(cfg);
        }
    }

    /// Re-read this dancer's sheet from disk.
    ///
    /// **A failed load keeps the sheet already in memory.** Saving a PNG is not
    /// atomic in every editor, and a sheet swapped for an error would leave
    /// nothing to draw — so a bad read is a warning and the dancer carries on
    /// with what it had.
    pub fn reload_sheet(&mut self, cfg: &Config) {
        match Sheet::load(&self.sheet_path) {
            Ok(sheet) => {
                let path = self.sheet_path.clone();
                tracing::info!(sheet = %path.display(), rows = sheet.rows.len(), "artwork reloaded");
                self.set_sheet(sheet, path, cfg);
            }
            Err(e) => {
                tracing::warn!(
                    sheet = %self.sheet_path.display(),
                    error = %e,
                    "reload failed; keeping the sheet already loaded"
                );
            }
        }
    }
}
