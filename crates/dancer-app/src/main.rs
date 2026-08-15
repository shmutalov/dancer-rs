//! dancer-rs — M1: the beat clock drives the sprite.
//!
//! M0 looped a sheet at a fixed frame rate. This adds the parts that make the
//! animation mean something: a `Source` reporting where playback is, a `BeatClock`
//! steering a local estimate from those reports, and cell selection derived from
//! the beat grid rather than from a timer.
//!
//! Anticipation — starting a move early so its impact cell lands *on* the beat — is
//! M3 and is deliberately absent. What is here locks phase to the grid; what M3
//! adds is choosing which move and when to begin it.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context;
use crossbeam_channel::{Receiver, Sender};
use dancer_render::{
    apply_window_styles, capture_owner, hwnd_of, present, primary_button_down, reassert_topmost,
    surface_size, Surface,
};
use dancer_score::Score;
use dancer_source::{FileSource, Source};
use dancer_sprite::Sheet;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

mod cli;
mod config;
mod events;
mod library;
mod playback;

/// The score cache, beside the executable (spec §5.1, §13).
const SCORE_DB: &str = "scores.db";

use config::{data_dir, Config};
use events::AppEvent;
use playback::Playback;

/// Grid-driven states re-evaluate at this rate and redraw only when the cell
/// actually changes. Cheaper than computing exact cell boundaries and immune to
/// the off-by-one errors that boundary maths invites.
const GRID_TICK: Duration = Duration::from_millis(8);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                // The binary target is `dancer-rs`, so its crate name is
                // `dancer_rs` — not `dancer_app`, which matches nothing.
                .unwrap_or_else(|_| {
                    "dancer_rs=info,dancer_render=info,dancer_sprite=info,dancer_score=info".into()
                }),
        )
        .with_target(false)
        .init();

    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(if msg == cli::USAGE { 0 } else { 2 });
        }
    };

    let dir = data_dir();
    tracing::info!(dir = %dir.display(), "data directory");

    let cfg = Config::load(&dir);

    let sheet_path = args.sheet.clone().unwrap_or_else(|| cfg.sheet_path(&dir));
    let sheet = Sheet::load(&sheet_path)
        .with_context(|| format!("loading sheet {}", sheet_path.display()))?;

    let (tx, rx) = crossbeam_channel::unbounded();

    // Three ways to get a grid, in order of directness. Without any of them the
    // app is exactly M0: a sheet looping at a fixed rate. That is the honest
    // `Unscored` behaviour (spec §10), not a degraded mode.
    match (args.score.clone(), args.audio.clone()) {
        // A hand-written grid, paired with whatever file was named. M1's path,
        // and still how the clock is tested against a known-correct grid.
        (Some(score_path), _) => {
            let score = Score::load(&score_path)
                .with_context(|| format!("loading score {}", score_path.display()))?;
            let audio = args.audio.clone().unwrap_or_else(|| score_path.clone());
            let duration = score.duration_secs();
            let source = build_source(&args, &audio, Some(duration))?;
            let meta = source.meta().clone();
            let _ = tx.send(AppEvent::TrackChanged {
                id: meta.id.clone(),
                meta: meta.clone(),
            });
            let _ = tx.send(AppEvent::ScoreReady {
                id: meta.id,
                score: Arc::new(score),
            });
            spawn_poll(source, cfg.playback.poll_secs, tx.clone())?;
        }
        // The real path: analyse the track, or take it from the cache.
        (None, Some(audio)) => {
            let source = build_source(&args, &audio, None)?;
            let meta = source.meta().clone();
            let _ = tx.send(AppEvent::TrackChanged {
                id: meta.id.clone(),
                meta: meta.clone(),
            });
            library::spawn(
                (!args.no_cache).then(|| dir.join(SCORE_DB)),
                args.models.clone().unwrap_or_else(|| dir.join("models")),
                meta.id.clone(),
                meta,
                audio,
                tx.clone(),
            );
            spawn_poll(source, cfg.playback.poll_secs, tx.clone())?;
        }
        (None, None) => {
            tracing::info!("no --audio or --score given; running Unscored at a fixed frame rate");
        }
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App {
        row: sheet.default_row,
        sheet,
        playback: Playback::new(Instant::now(), cfg.playback.offset_secs),
        cfg,
        dir,
        rx,
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

/// Build the simulated transport (spec §6.5).
fn build_source(
    args: &cli::Args,
    audio: &Path,
    duration: Option<f64>,
) -> anyhow::Result<FileSource> {
    let now = Instant::now();
    let mut source = match duration {
        Some(d) => FileSource::new(audio, d, now),
        // Length arrives with the analysis, off-thread and after playback has
        // started. Guessing it would poison the library index (spec §5.1).
        None => FileSource::with_unknown_duration(audio, now),
    };
    if let Some(rate) = args.rate {
        source = source.with_rate(rate);
    }
    if let Some(stale) = args.staleness {
        source = source.with_staleness(stale);
    }
    if !source.available() {
        anyhow::bail!("{} does not exist", audio.display());
    }

    tracing::info!(
        audio = %audio.display(),
        track = %source.meta().id,
        duration = ?duration,
        rate = args.rate.unwrap_or(1.0),
        stale_secs = args.staleness.unwrap_or_default().as_secs_f64(),
        "file source"
    );
    Ok(source)
}

/// Start the polling thread (spec §3.2's source-poll thread).
fn spawn_poll(
    mut source: FileSource,
    poll_secs: f64,
    tx: Sender<AppEvent>,
) -> anyhow::Result<()> {
    let interval = Duration::from_secs_f64(poll_secs.clamp(0.1, 30.0));

    std::thread::Builder::new()
        .name("source-poll".into())
        .spawn(move || loop {
            match source.poll() {
                Ok(Some(obs)) => {
                    let msg = AppEvent::PositionReport {
                        pos_secs: obs.position_secs(),
                        playing: obs.playing,
                        // Travels with the observation, so the render thread never
                        // needs to timestamp on receipt (spec §6.1).
                        at: obs.observed_at,
                    };
                    if tx.send(msg).is_err() {
                        return; // render thread gone
                    }
                }
                Ok(None) => {
                    if tx.send(AppEvent::PlaybackStopped).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    if tx.send(AppEvent::SourceLost(e.to_string())).is_err() {
                        return;
                    }
                }
            }
            std::thread::sleep(interval);
        })?;
    Ok(())
}

struct App {
    cfg: Config,
    dir: PathBuf,
    sheet: Sheet,
    playback: Playback,
    rx: Receiver<AppEvent>,
    window: Option<Window>,
    hwnd: Option<HWND>,
    surface: Option<Surface>,
    row: usize,
    cell: usize,
    /// Top-left in physical screen pixels.
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

    /// Beats one pass through the current row occupies (spec §4.2).
    fn beats_per_loop(&self) -> u32 {
        self.sheet
            .rows
            .get(self.row)
            .map_or(1, |r| r.beats_per_loop.max(1))
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

    /// Move the window through winit, so winit's cached geometry stays correct.
    ///
    /// `UpdateLayeredWindow` could do this in the same call that presents pixels,
    /// but moving the window behind winit's back desyncs it and mouse input
    /// eventually stops being delivered.
    fn move_to(&mut self, pos: (i32, i32)) {
        self.pos = pos;
        if let Some(window) = self.window.as_ref() {
            window.set_outer_position(PhysicalPosition::new(pos.0, pos.1));
        }
        self.draw();
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

        if let Err(e) = present(hwnd, surface, self.cfg.sprite.opacity) {
            tracing::error!(error = %e, "present failed");
        }
    }

    /// Drain the source thread's messages into the state machine.
    ///
    /// Draining here rather than waking the loop per message costs at most one
    /// frame of latency and no accuracy: every message carries the instant it
    /// describes.
    fn drain_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.playback.apply(ev);
        }
    }

    /// Advance the animation. Returns when the next tick is due.
    ///
    /// Two regimes, and the difference is the whole milestone: `Locked` reads the
    /// cell out of the beat grid, so phase cannot drift; everything else counts
    /// frames off a timer, which is M0's behaviour and is honest about knowing no
    /// tempo (spec §10's `Unscored`).
    fn tick(&mut self, now: Instant) -> Instant {
        let cells = self.sheet.cells_per_row().max(1);

        // Dragging owns the sprite: the Held row is a single pose, and letting the
        // grid drive it would fight the interaction.
        if self.drag.is_none() {
            if let Some(cell) = self.playback.grid_cell(now, self.beats_per_loop(), cells) {
                if cell != self.cell {
                    self.cell = cell;
                    self.draw();
                }
                return now + GRID_TICK;
            }
        }

        if now >= self.next_frame {
            self.cell = (self.cell + 1) % cells;
            self.draw();
            // Advance from the scheduled time, not from now, so the loop does not
            // drift slower than the configured rate.
            self.next_frame += self.frame_interval();
            if self.next_frame < now {
                self.next_frame = now + self.frame_interval();
            }
        }
        self.next_frame
    }

    fn begin_drag(&mut self) {
        if self.drag.is_some() {
            return;
        }
        let Some(cursor) = cursor_pos() else { return };
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

    fn end_drag(&mut self) {
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
        window.set_outer_position(PhysicalPosition::new(self.pos.0, self.pos.1));
        self.window = Some(window);

        self.draw();

        tracing::info!(
            w, h,
            pos = ?self.pos,
            fps = self.cfg.sprite.fps,
            offset_secs = self.cfg.playback.offset_secs,
            beats_per_loop = self.beats_per_loop(),
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
            WindowEvent::MouseInput { state, button, .. } => {
                tracing::debug!(
                    ?button,
                    ?state,
                    dragging = self.drag.is_some(),
                    capture = ?self.hwnd.map(capture_owner),
                    "mouse"
                );
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => self.begin_drag(),
                    (MouseButton::Left, ElementState::Released) => self.end_drag(),
                    // M1 still has no tray, and WS_EX_NOACTIVATE means no keyboard
                    // — so right-click is the only way out. Replaced in M5.
                    (MouseButton::Right, ElementState::Pressed) => {
                        self.end_drag();
                        self.store_position(el);
                        let _ = self.cfg.save(&self.dir);
                        el.exit();
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.drain_events();

        // While dragging, track the cursor in screen coordinates. Window-relative
        // events are useless here because the window moves with the pointer.
        if self.drag.is_some() && !primary_button_down() {
            // Belt and braces: capture should make a lost release impossible, but
            // a wedged drag leaves the window glued to the cursor, which is bad
            // enough to detect directly rather than trusting the event stream.
            tracing::debug!("button released without a Released event; ending drag");
            self.end_drag();
        }
        if let (Some(off), Some(cursor)) = (self.drag, cursor_pos()) {
            let want = (cursor.0 - off.0, cursor.1 - off.1);
            if want != self.pos {
                self.move_to(want);
            }
        }

        let now = Instant::now();
        let next = self.tick(now);
        el.set_control_flow(ControlFlow::WaitUntil(next.max(now)));
    }
}

fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}
