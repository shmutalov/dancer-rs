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
    surface_size, LatencyMonitor, Surface,
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
mod console;
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
    // Before anything writes a byte. In release this is a GUI-subsystem binary
    // with no console, so a command-line invocation has nowhere to print until
    // one is attached — see `console`.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if is_command_line(&argv) {
        console::attach();
    }

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

    let args = match cli::parse(argv.into_iter()) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(if msg == cli::USAGE { 0 } else { 2 });
        }
    };

    let dir = data_dir();
    tracing::info!(dir = %dir.display(), "data directory");

    let cfg = Config::load(&dir);

    // Round-trips the loaded config, so a file written by an older build gains
    // whatever keys have since appeared without losing what is already set.
    // `#[serde(default)]` means a missing section works fine at runtime, but it
    // also means the user cannot discover a setting by reading the file.
    if args.yandex_login {
        return run_yandex_login(cfg, &dir);
    }

    if args.write_config {
        cfg.save(&dir)?;
        let path = dir.join("config.toml");
        println!("Wrote {}", path.display());
        println!("\nFor Yandex, set:\n  [source.yandex]\n  token = \"...\"\n  fetch_for_analysis = true");
        return Ok(());
    }

    // A command, not a mode: analyse and exit. Deliberately separate from the
    // dancer, because a library scan is minutes of work and should not be
    // happening invisibly behind a sprite.
    if !args.scan.is_empty() {
        return run_scan(&args, &dir);
    }

    let sheet_path = args.sheet.clone().unwrap_or_else(|| cfg.sheet_path(&dir));
    let sheet = Sheet::load(&sheet_path)
        .with_context(|| format!("loading sheet {}", sheet_path.display()))?;

    if args.check_sheet {
        return run_check_sheet(&sheet);
    }

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
            spawn_poll(
                Box::new(source),
                Duration::from_secs_f64(cfg.playback.poll_secs.clamp(0.1, 30.0)),
                None,
                tx.clone(),
            )?;
        }
        // Analyse a named file, or take it from the cache.
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
            spawn_poll(
                Box::new(source),
                Duration::from_secs_f64(cfg.playback.poll_secs.clamp(0.1, 30.0)),
                None,
                tx.clone(),
            )?;
        }
        // Nothing named: follow whatever the user is actually playing. This is the
        // product's real behaviour, and the only configuration in which the M3 A/B
        // can be judged — the dancer moves to music you can hear.
        (None, None) if !args.no_smtc => {
            let db = (!args.no_cache).then(|| dir.join(SCORE_DB));
            let fallback = stream_fallback(&cfg, &args, &dir);
            match dancer_source::SmtcSource::new(cfg.source.allowlist.clone()) {
                Ok(source) => {
                    let tx2 = tx.clone();
                    spawn_poll(
                        Box::new(source),
                        cfg.playback.smtc_poll_interval(),
                        // SMTC never reports a path, only (title, artist). The
                        // library index from M2 is the whole bridge — analysis is
                        // impossible here because there is no file to analyse.
                        Some(Box::new(move |meta| {
                            library::spawn_lookup(
                                db.clone(),
                                meta.clone(),
                                fallback.clone(),
                                tx2.clone(),
                            );
                        })),
                        tx.clone(),
                    )?;
                }
                Err(e) => {
                    // Not fatal: the dancer still dances, it just knows nothing.
                    tracing::warn!(error = %e, "no SMTC; running Unscored");
                }
            }
        }
        (None, None) => {
            tracing::info!("no source; running Unscored at a fixed frame rate");
        }
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::Wait);

    let rows = row_info(&sheet);
    let mut playback = Playback::new(
        Instant::now(),
        cfg.playback.offset_secs,
        rows,
        sheet.default_row,
        // Seeded from the wall clock so successive runs choreograph differently,
        // but reported so a run can be reproduced from its log (spec §11.3's
        // weighted random is otherwise impossible to debug by eye).
        seed(),
    );
    if args.no_anticipate {
        playback.toggle_anticipation();
    }

    let mut app = App {
        row: sheet.default_row,
        sheet,
        playback,
        latency: LatencyMonitor::new(GRID_TICK),
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

/// Does this invocation talk to a terminal rather than open a window?
///
/// Deliberately a raw scan of the arguments rather than something derived from
/// the parsed `Args`: parsing can fail, and a usage error also needs somewhere to
/// print. Unknown flags count, for the same reason.
fn is_command_line(argv: &[String]) -> bool {
    argv.iter().any(|a| {
        matches!(
            a.as_str(),
            "-h" | "--help" | "--scan" | "--write-config" | "--yandex-login" | "--check-sheet"
        ) || (a.starts_with('-') && !KNOWN_GUI_FLAGS.contains(&a.as_str()))
    })
}

/// Flags that modify the dancer rather than replacing it with a command.
const KNOWN_GUI_FLAGS: &[&str] = &[
    "--audio",
    "--score",
    "--models",
    "--no-cache",
    "--no-anticipate",
    "--no-smtc",
    "--no-fetch",
    "--rate",
    "--stale",
];

/// `--yandex-login`: OAuth device flow, then store the token (spec §6.4.1).
///
/// Deliberately a separate command that exits. Signing in is a decision, and it
/// should not be something that happens because the dancer was launched.
#[cfg(feature = "yandex")]
fn run_yandex_login(mut cfg: Config, dir: &Path) -> anyhow::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let device = hostname().unwrap_or_else(|| "dancer-rs".into());
    println!("Signing in to Yandex Music as \"{device}\"…\n");

    let token = runtime.block_on(dancer_yandex::login(&device, |code| {
        // Printed from inside the callback because the call blocks afterwards,
        // waiting for confirmation — printing after it returns would show the
        // code once it was already too late to use.
        println!("  1. Open {}", code.verification_url);
        println!("  2. Enter code: {}", code.user_code);
        if let Some(t) = code.expires_in {
            println!("     (valid for {} minutes)", t.as_secs() / 60);
        }
        println!("\nWaiting for confirmation…");
    }));

    match token {
        Ok(token) => {
            cfg.source.yandex.token = token;
            // Signing in *is* the request, so there is nothing left to opt into.
            cfg.source.yandex.fetch_for_analysis = true;
            cfg.save(dir)?;
            println!("\nSigned in. Token saved to {}", dir.join("config.toml").display());
            println!(
                "Streamed tracks will now be fetched, analysed and deleted.\n\
                 Revoke any time at https://id.yandex.ru/security/app-passwords, \
                 or set fetch_for_analysis = false."
            );
            Ok(())
        }
        Err(e) => {
            // Not an anyhow bail: a declined or expired sign-in is a normal
            // outcome, not a crash, and the message is already the whole story.
            println!("\nSign-in did not complete: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(feature = "yandex"))]
fn run_yandex_login(_cfg: Config, _dir: &Path) -> anyhow::Result<()> {
    anyhow::bail!("this build has the `yandex` feature disabled")
}

/// A name the user will recognise in their Yandex device list.
#[cfg(feature = "yandex")]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|s| format!("dancer-rs on {s}"))
}

/// Whether to fetch a streamed track in order to analyse it (spec §6.4).
///
/// Three conditions, all required, none of them a default: the feature compiled
/// in, a token supplied, and `fetch_for_analysis` switched on. Reaching out to
/// fetch audio is not something the app should ever start doing on its own.
fn stream_fallback(
    cfg: &Config,
    args: &cli::Args,
    dir: &Path,
) -> Option<library::StreamFallback> {
    if args.no_fetch || !cfg.source.yandex.enabled() {
        return None;
    }
    tracing::info!(
        "streamed-track analysis is on: tracks with no local match will be fetched, \
         analysed and deleted"
    );
    Some(library::StreamFallback {
        #[cfg(feature = "yandex")]
        token: cfg.source.yandex.token.clone(),
        models: args.models.clone().unwrap_or_else(|| dir.join("models")),
        // Beside the cache, not in the user's temp: a file we are contractually
        // obliged to delete should be somewhere we can see if it survives.
        scratch: dir.join("scratch"),
    })
}

/// `--scan`: fill the cache so SMTC has something to recognise (spec §13).
fn run_scan(args: &cli::Args, dir: &Path) -> anyhow::Result<()> {
    let db = (!args.no_cache).then(|| dir.join(SCORE_DB));
    let models = args.models.clone().unwrap_or_else(|| dir.join("models"));

    println!("Scanning {} folder(s)…", args.scan.len());
    let started = Instant::now();
    let mut last = 0usize;

    let report = library::scan(&args.scan, db.as_deref(), &models, &mut |r, path| {
        let done = r.analysed + r.cached + r.failed;
        // Printed rather than logged: this is a foreground command whose whole
        // output is progress, and it can run for minutes on a real library.
        if done != last {
            last = done;
            println!(
                "  [{done}/{}] {}",
                r.found,
                path.file_name().unwrap_or_default().to_string_lossy()
            );
        }
    });

    println!(
        "\n{} found, {} analysed, {} already cached, {} failed  ({:.0}s)",
        report.found,
        report.analysed,
        report.cached,
        report.failed,
        started.elapsed().as_secs_f32()
    );
    if let Some(db) = db {
        println!("Cache: {}", db.display());
    }
    if report.found == 0 {
        println!("\nNothing to analyse. Supported: {}", library::AUDIO_EXTENSIONS.join(", "));
    }
    Ok(())
}

/// Print how every row of a sheet resolved, then exit.
///
/// Reports the *resolved* view — what the scheduler will actually see — rather than
/// echoing the manifest back. Unparsed `motif` and `effort_time` tags have already
/// been dropped with a warning by then (spec §4.2.1), so a typo shows up here as a
/// blank column rather than as a dancer behaving oddly three songs later.
fn run_check_sheet(sheet: &Sheet) -> anyhow::Result<()> {
    let rows = row_info(sheet);

    println!(
        "\n{:<3} {:<10} {:>6} {:>7} {:>7}  {:<22} {:<10} tiers",
        "#", "row", "impact", "beats", "energy", "motif", "effort"
    );
    for r in &rows {
        if r.held {
            println!(
                "{:<3} {:<10} {:>6} {:>7} {:>7}  {:<22} {:<10} (drag pose, never danced)",
                r.index, r.name, "-", "-", "-", "-", "-"
            );
            continue;
        }
        // The tiers a row can appear in — the one thing a manifest never states
        // directly, because it falls out of the Motif exertion (spec §11.3).
        let tiers: Vec<&str> = [
            (dancer_choreo::Tier::Calm, "calm"),
            (dancer_choreo::Tier::Steady, "steady"),
            (dancer_choreo::Tier::Loud, "loud"),
        ]
        .into_iter()
        .filter(|(t, _)| dancer_choreo::motif::admits(*t, &r.motifs))
        .map(|(_, n)| n)
        .collect();

        let motifs: Vec<&str> = r.motifs.iter().map(|m| m.as_str()).collect();
        println!(
            "{:<3} {:<10} {:>6} {:>7} {:>7}  {:<22} {:<10} {}",
            r.index,
            r.name,
            r.impact_cell,
            r.beats_per_loop,
            r.energy.map(|e| format!("{e:.2}")).unwrap_or_else(|| "-".into()),
            if motifs.is_empty() { "-".into() } else { motifs.join(", ") },
            r.effort_time.map(|e| e.as_str()).unwrap_or("-"),
            tiers.join(" ")
        );
    }

    let danceable = rows.iter().filter(|r| !r.held).count();
    let untagged = rows.iter().filter(|r| !r.held && r.motifs.is_empty()).count();
    println!("\n{danceable} danceable rows, default `{}`", sheet.rows[sheet.default_row].name);
    if untagged > 0 {
        println!(
            "{untagged} carry no motif and are admitted by every tier — energy alone \
             will steer them (spec §11.3)."
        );
    }
    for tier in ["calm", "steady", "loud"] {
        let n = rows
            .iter()
            .filter(|r| !r.held)
            .filter(|r| {
                let t = match tier {
                    "calm" => dancer_choreo::Tier::Calm,
                    "steady" => dancer_choreo::Tier::Steady,
                    _ => dancer_choreo::Tier::Loud,
                };
                dancer_choreo::motif::admits(t, &r.motifs)
            })
            .count();
        // One candidate in a tier means that tier plays one move for the whole
        // passage, which is the behaviour the project exists to beat.
        let note = if n < 2 { "  <-- too few to vary" } else { "" };
        println!("  {tier:<7} {n} rows{note}");
    }
    Ok(())
}

/// Describe the sheet's rows to the scheduler (spec §4.2, §11.3).
fn row_info(sheet: &Sheet) -> Vec<dancer_choreo::RowInfo> {
    sheet
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| dancer_choreo::RowInfo {
            index: i,
            name: r.name.clone(),
            cells: r.cells.len(),
            beats_per_loop: r.beats_per_loop,
            impact_cell: r.impact_cell,
            pools: r.pools.to_vec(),
            energy: r.energy,
            motifs: parse_tags(&r.motifs, &r.name),
            effort_time: r.effort_time.as_deref().and_then(|s| {
                dancer_choreo::Effort::parse(s).or_else(|| {
                    tracing::warn!(row = %r.name, effort_time = s, "unknown effort_time; ignoring");
                    None
                })
            }),
            loopable: r.loopable,
            held: Some(i) == sheet.held_row,
        })
        .collect()
}

/// Resolve a row's Motif tags, warning on anything unrecognised.
///
/// A bad tag must not stop the sheet loading: the artwork is fine, and a dancer
/// that refuses to start because of a typo in a metadata field is worse than one
/// that ignores the field. This is also where a manifest written for a future
/// vocabulary degrades gracefully rather than failing.
fn parse_tags(tags: &[String], row: &str) -> Vec<dancer_choreo::Motif> {
    tags.iter()
        .filter_map(|t| {
            dancer_choreo::Motif::parse(t).or_else(|| {
                tracing::warn!(row = %row, motif = %t, "unknown motif; ignoring");
                None
            })
        })
        .collect()
}

/// Seed for move selection, logged so a session can be reproduced.
fn seed() -> u64 {
    let s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x5EED);
    tracing::info!(seed = s, "choreography seed");
    s
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
///
/// Owns track-change detection for every source: an adapter reports what is
/// playing now, and noticing that "now" became a different track is this loop's
/// job rather than each adapter's.
fn spawn_poll(
    mut source: Box<dyn Source>,
    interval: Duration,
    on_track: Option<Box<dyn Fn(&dancer_score::TrackMeta) + Send>>,
    tx: Sender<AppEvent>,
) -> anyhow::Result<()> {
    let name = source.name();
    std::thread::Builder::new()
        .name("source-poll".into())
        .spawn(move || {
            let mut current: Option<dancer_score::TrackId> = None;
            loop {
                match source.poll() {
                    Ok(Some(obs)) => {
                        if current.as_ref() != Some(&obs.track.id) {
                            current = Some(obs.track.id.clone());
                            if let Some(f) = on_track.as_ref() {
                                f(&obs.track);
                            }
                            if tx
                                .send(AppEvent::TrackChanged {
                                    id: obs.track.id.clone(),
                                    meta: obs.track.clone(),
                                })
                                .is_err()
                            {
                                return;
                            }
                        }

                        // A session with no timeline publishes identity only
                        // (spec §6.2). Reporting a position of zero would be a
                        // lie the clock cannot detect, so say nothing and let the
                        // state machine sit in Unscored.
                        if obs.timeline {
                            let msg = AppEvent::PositionReport {
                                pos_secs: obs.position_secs(),
                                playing: obs.playing,
                                // Travels with the observation, so the render
                                // thread never timestamps on receipt (spec §6.1).
                                at: obs.observed_at,
                            };
                            if tx.send(msg).is_err() {
                                return;
                            }
                        }
                    }
                    Ok(None) => {
                        if current.is_some() {
                            current = None;
                            if tx.send(AppEvent::PlaybackStopped).is_err() {
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(source = name, error = %e, "poll failed");
                        if tx.send(AppEvent::SourceLost(e.to_string())).is_err() {
                            return;
                        }
                    }
                }
                std::thread::sleep(interval);
            }
        })?;
    Ok(())
}

struct App {
    cfg: Config,
    dir: PathBuf,
    sheet: Sheet,
    playback: Playback,
    latency: LatencyMonitor,
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

        // Spec §11.2 says to measure display latency rather than assume 16 ms.
        // This is the measurable half; see `dancer_render::latency` for what is
        // not, and why that is acceptable.
        let t0 = Instant::now();
        if let Err(e) = present(hwnd, surface, self.cfg.sprite.opacity) {
            tracing::error!(error = %e, "present failed");
        }
        self.latency.record(t0.elapsed());
        self.playback
            .set_render_latency(self.latency.render_latency().as_secs_f64());
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
                    self.draw();
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
            anticipate = self.playback.anticipating(),
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
                    // The A/B switch (ROADMAP M3). Middle-click flips between
                    // anticipation and M1's plain loop on the same track, which is
                    // the only way to judge the difference honestly — described,
                    // it sounds like a detail; seen back to back, it is the point.
                    (MouseButton::Middle, ElementState::Pressed) => {
                        let on = self.playback.toggle_anticipation();
                        tracing::info!(anticipate = on, "A/B toggle");
                    }
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
