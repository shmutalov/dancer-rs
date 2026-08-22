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

use anyhow::Context as _;
use tray_icon::menu::MenuEvent;
use crossbeam_channel::{Receiver, Sender};
use dancer_render::{apply_window_styles, primary_button_down};
use dancer_score::Score;
use dancer_source::{FileSource, Source};
use dancer_sprite::Sheet;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{WindowId, WindowLevel};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

mod account;
mod cli;
mod config;
mod console;
mod context;
mod dancer;
mod dialog;
mod events;
mod library;
mod logging;
mod playback;
mod sheets;
mod tray;
mod watch;

/// The score cache, beside the executable (spec §5.1, §13).
const SCORE_DB: &str = "scores.db";

use config::{data_dir, Config};
use events::AppEvent;
use playback::Playback;


fn main() {
    // Before anything writes a byte. In release this is a GUI-subsystem binary
    // with no console, so a command-line invocation has nowhere to print until
    // one is attached — see `console`.
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let cli = is_command_line(&argv);
    if cli {
        console::attach();
    }

    if let Err(e) = run(argv) {
        // `{e:#}` chains the contexts: "loading sheet X: file not found" rather
        // than just the outermost line.
        tracing::error!(error = %format!("{e:#}"), "fatal");
        if cli {
            eprintln!("Error: {e:#}");
        } else {
            // A release build has no console, so before this dialog existed a
            // startup failure was an exit code and nothing else — the app died
            // with the reason written to a stderr nobody can see.
            dialog::error(
                "dancer-rs could not start",
                &format!("{e:#}

Details are in dancer-rs.log, next to the executable."),
            );
        }
        std::process::exit(1);
    }
}

fn run(argv: Vec<String>) -> anyhow::Result<()> {
    // Resolved before the subscriber exists, because the log file lives in it.
    // `data_dir` logs one line when the exe directory turns out to be read-only,
    // and that line is lost here — the resolved directory is logged below, which
    // says the same thing in the case that matters.
    let dir = data_dir();
    let log = logging::init(&dir);

    let args = match cli::parse(argv.into_iter()) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(if msg == cli::USAGE { 0 } else { 2 });
        }
    };

    tracing::info!(dir = %dir.display(), "data directory");
    match &log {
        Some(p) => tracing::info!(file = %p.display(), "logging here too"),
        // Not a warning: a read-only folder is a supported way to run this.
        None => tracing::info!("no log file; this directory is not writable"),
    }

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
    if args.scan_requested() {
        return run_scan(&args, &cfg, &dir);
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

    // Seeded from the wall clock so successive runs choreograph differently, but
    // reported so a run can be reproduced from its log (spec §11.3's weighted
    // random is otherwise impossible to debug by eye).
    let dancers = build_troupe(&cfg, &dir, &args, sheet, sheet_path, seed());

    // Built before the struct takes ownership of `dir`.
    let sheet_paths: Vec<PathBuf> = dancers.iter().map(|d| d.sheet_path.clone()).collect();
    let watch = watch::Watch::new(&dir.join("config.toml"), &sheet_paths)
        .inspect_err(|e| tracing::warn!(error = %e, "no hot reload"))
        .ok();

    let mut app = App {
        dancers,
        cfg,
        dir,
        rx,
        tray: None,
        tray_shown: None,
        watch,
        sheets: Vec::new(),
        account: account::Status::Off,
        account_ch: account::Channel::new(),
        context: None,
    };
    event_loop.run_app(&mut app)?;
    Ok(())
}

/// Build the dancers `[dancers]` asks for (one, unless configured otherwise).
///
/// Dancer 0 always gets the sheet `run` already loaded and validated — the CLI
/// override, or `[sprite] sheet` — so a troupe of one is byte-for-byte the app as
/// it was before troupes existed. Later dancers draw from `[dancers] sheets`,
/// cycling, or at random from the artwork folder when that list is empty; a name
/// that fails to load falls back to dancer 0's sheet rather than to no dancer.
///
/// Every per-dancer random draw — sheet, size jitter, scheduler seed — derives
/// from the one logged `seed`, so a troupe that looked wrong can be reproduced
/// exactly from its log line.
fn build_troupe(
    cfg: &Config,
    dir: &Path,
    args: &cli::Args,
    sheet: Sheet,
    sheet_path: PathBuf,
    seed: u64,
) -> Vec<dancer::Dancer> {
    let count = cfg.dancers.count();
    let jitter = cfg.dancers.jitter();
    let mut rng = XorShift::new(seed);

    // Candidates for dancers beyond the first: the named list, or the folder.
    // Names resolve against the same scan the tray shows, matched by that menu's
    // own labels — `PolishCow` finds `assets/polish-cow/PolishCow.png`, and
    // `polish-cow/PolishCow` names it exactly.
    let artwork = artwork_dir_of(cfg, dir);
    let scanned = sheets::list(&artwork);
    let pool: Vec<PathBuf> = if cfg.dancers.sheets.is_empty() {
        scanned
    } else {
        cfg.dancers
            .sheets
            .iter()
            .filter_map(|name| {
                let hit = scanned.iter().find(|p| {
                    sheets::label_in(&artwork, p).eq_ignore_ascii_case(name)
                        || sheets::label(p).eq_ignore_ascii_case(name)
                });
                if hit.is_none() {
                    tracing::warn!(name, "no such sheet in the artwork folder; skipping");
                }
                hit.cloned()
            })
            .collect()
    };

    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let (sheet_i, path_i) = if i == 0 {
            (sheet.clone(), sheet_path.clone())
        } else {
            let want = if cfg.dancers.sheets.is_empty() {
                // Random from the folder. The folder can be empty; fall through.
                pool.get((rng.next() as usize) % pool.len().max(1)).cloned()
            } else {
                pool.get(i % pool.len()).cloned()
            };
            match want.map(|p| (Sheet::load(&p), p)) {
                Some((Ok(s), p)) => (s, p),
                Some((Err(e), p)) => {
                    tracing::warn!(sheet = %p.display(), error = %e, "dancer sheet failed; using dancer 0's");
                    (sheet.clone(), sheet_path.clone())
                }
                None => (sheet.clone(), sheet_path.clone()),
            }
        };

        // 1 ± jitter, uniform. Multiplicative so "about half again as big" means
        // the same thing at every base scale.
        let spread = if jitter > 0.0 {
            1.0 + jitter * (rng.unit() * 2.0 - 1.0)
        } else {
            1.0
        };
        let scale = (cfg.sprite.scale * spread).clamp(0.1, 8.0);

        let mut playback = Playback::new(
            Instant::now(),
            cfg.playback.offset_secs,
            row_info(&sheet_i),
            sheet_i.default_row,
            // A different stream per dancer, so a troupe picks different moves on
            // the same downbeat — which is what makes it read as dancers together
            // rather than one dancer copy-pasted.
            seed.wrapping_add((i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        );
        if args.no_anticipate {
            playback.toggle_anticipation();
        }

        tracing::info!(dancer = i, sheet = %path_i.display(), scale, "dancer");
        out.push(dancer::Dancer::new(sheet_i, path_i, playback, scale));
    }
    out
}

/// Where sheets live (spec §13's `artwork_dir`), before `App` exists.
fn artwork_dir_of(cfg: &Config, dir: &Path) -> PathBuf {
    let art = Path::new(&cfg.sprite.artwork_dir);
    if art.is_absolute() {
        art.to_path_buf()
    } else {
        dir.join(art)
    }
}

/// A tiny deterministic generator for per-dancer draws.
///
/// Not `rand`: the scheduler already made the project's one randomness decision
/// — seeded, logged, reproducible — and pulling a crate in for two draws per
/// dancer per launch would be a second decision for no second reason.
struct XorShift(u64);

impl XorShift {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1).
    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }
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
pub(crate) fn hostname() -> Option<String> {
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
///
/// Folders come from the command line when given, and otherwise from
/// `[library] folders`. Bare `--scan` is the form a stranger runs after setting the
/// folders once — the alternative was retyping paths on every rescan.
fn run_scan(args: &cli::Args, cfg: &Config, dir: &Path) -> anyhow::Result<()> {
    let db = (!args.no_cache).then(|| dir.join(SCORE_DB));
    let models = args.models.clone().unwrap_or_else(|| dir.join("models"));

    let folders = if args.scan.is_empty() {
        cfg.library.paths()
    } else {
        args.scan.clone()
    };
    if folders.is_empty() {
        println!("No folders to scan.
");
        println!("Either name one:");
        println!("  dancer-rs --scan D:/music
");
        println!("or set them once in {}:", dir.join("config.toml").display());
        println!("  [library]");
        println!("  folders = [\"D:/music\"]");
        return Ok(());
    }

    println!("Scanning {} folder(s)…", folders.len());
    let started = Instant::now();
    let mut last = 0usize;

    let report = library::scan(&folders, db.as_deref(), &models, &mut |r, path| {
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

/// Send the user to a sheet source, after saying whose artwork it is.
///
/// The warning is a confirmation rather than a footnote. The app is about to point
/// someone at artwork it has no rights to, from a menu it drew, and that reads like
/// an endorsement unless it says otherwise once, in front of the link.
fn open_sheet_source(index: usize) {
    let Some(source) = sheets::SOURCES.get(index) else {
        return;
    };
    std::thread::spawn(move || {
        if dialog::confirm("Whose artwork is this?", &sheets::ownership_warning(source)) {
            dialog::open_url(source.url);
        }
    });
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
    /// The troupe. Always at least one; index 0 is the "primary" dancer — the
    /// one whose position is remembered, whose sheet cuts the tray icon and whose
    /// state the tray reports.
    dancers: Vec<dancer::Dancer>,
    rx: Receiver<AppEvent>,
    /// `None` until `resumed`, and `None` for good if the shell refused it. A
    /// missing tray is a degraded UI, never a reason not to dance.
    tray: Option<tray::Tray>,
    /// `None` if the platform watcher could not start. Hot reload is a convenience;
    /// losing it must not stop the dancer.
    watch: Option<watch::Watch>,
    /// Sheets found in the artwork folder, as the tray lists them.
    sheets: Vec<PathBuf>,
    /// Yandex sign-in state, and the channel the worker reports through.
    account: account::Status,
    account_ch: account::Channel,
    /// What the tray menu was last told, so it is only rewritten on change.
    tray_shown: Option<tray::State>,
    /// The last right-click menu shown and which dancer it was shown on, kept so
    /// its ids stay matchable: the selection arrives on the shared menu channel
    /// after the popup closes.
    context: Option<(usize, context::Context)>,
}

impl App {
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
    /// Build the tray, using the sheet's own artwork as the icon.
    ///
    /// A failure here is logged and dropped. The shell can refuse a tray icon —
    /// explorer restarting, a locked-down session — and none of that is a reason to
    /// stop dancing. Right-click on the sprite remains the fallback exit.
    fn build_tray(&mut self) {
        if self.tray.is_some() {
            return;
        }
        // Dancer 0's default row, first cell: the pose the sheet author chose as
        // its resting state, which is the one that reads as "this sheet".
        let sheet = &self.dancers[0].sheet;
        let cell: Option<Vec<u32>> = sheet
            .rows
            .get(sheet.default_row)
            .and_then(|r| r.cells.first())
            .map(|c| c.to_vec());
        let Some(cell) = cell else {
            tracing::warn!("sheet has no cells; skipping the tray icon");
            return;
        };
        let (cell_w, cell_h) = (sheet.cell_width, sheet.cell_height);

        // Rescanned on every build, so a sheet dropped into the folder appears
        // after any action that rebuilds the tray rather than only after a restart.
        self.sheets = sheets::list(&self.artwork_dir());
        let art = self.artwork_dir();
        let names: Vec<String> = self.sheets.iter().map(|p| sheets::label_in(&art, p)).collect();
        let current = self
            .sheets
            .iter()
            .position(|p| *p == self.dancers[0].sheet_path);

        match tray::Tray::new(
            &cell,
            cell_w,
            cell_h,
            &self.tray_state(),
            &names,
            current,
            &sheets::SOURCES.iter().map(|s| s.name).collect::<Vec<_>>(),
        ) {
            Ok(t) => {
                tracing::info!("tray ready");
                self.tray = Some(t);
            }
            Err(e) => tracing::warn!(error = %e, "no tray icon; the sprite's right-click menu still quits"),
        }
    }

    /// Apply whatever the tray menu was clicked for, then refresh what it shows.
    /// Drain the process-wide menu channel and dispatch each click to whichever
    /// menu owns its id.
    ///
    /// One drain point on purpose: `MenuEvent::receiver()` is global, so a second
    /// poller would steal clicks at random -- whichever ran first would eat the
    /// other menu's events. The tray is asked first only because it existed first;
    /// ids are unique, so the order cannot change the answer.
    fn drain_menus(&mut self, el: &ActiveEventLoop) {
        while let Ok(ev) = MenuEvent::receiver().try_recv() {
            if let Some(action) = self.tray.as_ref().and_then(|t| t.action_for(ev.id())) {
                tracing::info!(?action, "tray");
                self.on_tray(action, el);
            } else if let Some((i, action)) = self
                .context
                .as_ref()
                .and_then(|(i, c)| c.action_for(ev.id()).map(|a| (*i, a)))
            {
                tracing::info!(dancer = i, ?action, "context menu");
                self.on_context(i, action, el);
            }
            if el.exiting() {
                return;
            }
        }
        self.refresh_tray();
    }

    fn on_tray(&mut self, action: tray::Action, el: &ActiveEventLoop) {
        match action {
            tray::Action::ToggleClickThrough => {
                self.cfg.window.click_through = !self.cfg.window.click_through;
                for d in &self.dancers {
                    if let Some(hwnd) = d.hwnd {
                        apply_window_styles(hwnd, self.cfg.window.click_through);
                    }
                }
                // With the mouse passing through, the sprite cannot be clicked
                // at all — the tray is now the only way back, which is exactly
                // why this option was unusable before there was one.
                if self.cfg.window.click_through {
                    tracing::info!("click-through on; use the tray to turn it off");
                }
            }
            tray::Action::ToggleAlwaysOnTop => {
                self.cfg.window.always_on_top = !self.cfg.window.always_on_top;
                for d in &self.dancers {
                    if let Some(w) = d.window.as_ref() {
                        w.set_window_level(if self.cfg.window.always_on_top {
                            WindowLevel::AlwaysOnTop
                        } else {
                            WindowLevel::Normal
                        });
                    }
                }
            }
            tray::Action::ToggleAnticipation => {
                self.toggle_anticipation_all();
            }
            tray::Action::NudgeOffset(by) => self.set_offset(self.cfg.playback.offset_secs + by),
            tray::Action::ResetOffset => self.set_offset(config::Playback::default_offset()),
            tray::Action::OpenDataDir => dialog::open_dir(&self.dir),
            tray::Action::OpenArtworkDir => dialog::open_dir(&self.artwork_dir()),
            tray::Action::SheetHelp => self.show_sheet_help(),
            tray::Action::OpenSheetSource(i) => open_sheet_source(i),
            tray::Action::SelectSheet(i) => self.select_sheet(i),
            tray::Action::YandexSignIn => {
                self.account = account::Status::Checking;
                account::sign_in(self.account_ch.tx.clone());
                self.tray_shown = None;
            }
            tray::Action::Quit => self.quit(el),
        }
    }

    fn on_context(&mut self, dancer: usize, action: context::Action, el: &ActiveEventLoop) {
        match action {
            context::Action::SetScale(s) => {
                if let Some(d) = self.dancers.get_mut(dancer) {
                    d.set_scale(s, &self.cfg);
                    tracing::info!(dancer, scale = s, "scale");
                }
                // Persisted only when there is one dancer: with a troupe on
                // screen -- possibly jittered, possibly resized individually --
                // no single number is "the" scale, and writing one would make
                // the config lie. Chosen by eye, so written through immediately,
                // same as the offset.
                if self.dancers.len() == 1 {
                    self.cfg.sprite.scale = s;
                    if let Err(e) = self.cfg.save(&self.dir) {
                        tracing::warn!(error = %e, "could not save the scale");
                    }
                }
            }
            context::Action::ToggleMirror => {
                self.cfg.sprite.mirror = !self.cfg.sprite.mirror;
                for d in &mut self.dancers {
                    d.draw(&self.cfg);
                }
                if let Err(e) = self.cfg.save(&self.dir) {
                    tracing::warn!(error = %e, "could not save mirror");
                }
            }
            context::Action::Quit => self.quit(el),
        }
    }

    /// Build the right-click menu as of right now and pop it at the cursor.
    ///
    /// Blocks until the menu closes; the selection lands on the menu channel and
    /// `drain_menus` picks it up on the next pass, which winit runs immediately
    /// after this handler returns.
    fn show_context_menu(&mut self, dancer: usize) {
        let Some(d) = self.dancers.get(dancer) else { return };
        let Some(hwnd) = d.hwnd else { return };
        match context::Context::new(d.scale, self.cfg.sprite.mirror) {
            Ok(ctx) => {
                ctx.show(hwnd);
                self.context = Some((dancer, ctx));
            }
            Err(e) => tracing::warn!(error = %e, "could not build the context menu"),
        }
    }

    /// Flip the A/B lead for the whole troupe at once: half a troupe
    /// anticipating is neither arm of the experiment.
    fn toggle_anticipation_all(&mut self) -> bool {
        let mut on = false;
        for d in &mut self.dancers {
            on = d.playback.toggle_anticipation();
        }
        on
    }

    /// Move the output-latency offset (spec §9.2) and persist it.
    ///
    /// Written through immediately rather than on exit: this is dialled in by eye
    /// against playing music, and losing the value to a crash — or to the process
    /// being killed, which is how a tray app usually dies — would mean doing it
    /// again.
    fn set_offset(&mut self, secs: f64) {
        // A full second either way is far past any real output latency, and a
        // runaway value would put the dancer on a different beat entirely.
        let secs = (secs * 1000.0).round() / 1000.0;
        let secs = secs.clamp(-1.0, 1.0);
        self.cfg.playback.offset_secs = secs;
        for d in &mut self.dancers {
            d.playback.clock.set_offset(secs);
        }
        tracing::info!(offset_ms = secs * 1000.0, "offset");
        if let Err(e) = self.cfg.save(&self.dir) {
            tracing::warn!(error = %e, "could not save the offset");
        }
    }

    /// Re-read whatever changed on disk (ROADMAP M5).
    fn drain_watch(&mut self) {
        let Some(w) = self.watch.as_mut() else {
            return;
        };
        let changes = w.poll(Instant::now());
        for change in changes {
            match change {
                watch::Change::Config => self.reload_config(),
                watch::Change::Artwork => {
                    // The watcher does not say whose sheet changed, and reloading
                    // an unchanged one is a no-op that costs one PNG decode --
                    // simpler than plumbing the path through and being wrong about
                    // sidecar naming.
                    for d in &mut self.dancers {
                        d.reload_sheet(&self.cfg);
                    }
                }
            }
        }
    }

    /// Re-read `config.toml`, applying only what can be changed live.
    ///
    /// Sources, the Yandex token and the library folders are **not** re-applied:
    /// they own live threads, and half-swapping a running source is a much bigger
    /// change than it looks. A restart is honest for those and rare in practice.
    ///
    /// The window position is deliberately not re-applied either. It is written
    /// *by* the app whenever the sprite is dragged, so honouring it on reload would
    /// make the dancer jump back to wherever the file happened to say.
    fn reload_config(&mut self) {
        let fresh = Config::load(&self.dir);
        let (pos_x, pos_y, monitor) = (
            self.cfg.window.x,
            self.cfg.window.y,
            self.cfg.window.monitor,
        );

        let sheet_changed = fresh.sheet_path(&self.dir) != self.cfg.sheet_path(&self.dir);
        let resize = fresh.sprite.scale != self.cfg.sprite.scale;

        self.cfg.sprite = fresh.sprite;
        self.cfg.window = fresh.window;
        self.cfg.playback = fresh.playback;
        self.cfg.window.x = pos_x;
        self.cfg.window.y = pos_y;
        self.cfg.window.monitor = monitor;

        for d in &mut self.dancers {
            d.playback.clock.set_offset(self.cfg.playback.offset_secs);
            if let Some(hwnd) = d.hwnd {
                apply_window_styles(hwnd, self.cfg.window.click_through);
            }
            if let Some(win) = d.window.as_ref() {
                win.set_window_level(if self.cfg.window.always_on_top {
                    WindowLevel::AlwaysOnTop
                } else {
                    WindowLevel::Normal
                });
            }
        }

        tracing::info!(
            offset_ms = self.cfg.playback.offset_secs * 1000.0,
            scale = self.cfg.sprite.scale,
            idle_bpm = self.cfg.playback.idle_bpm,
            "config reloaded"
        );

        if sheet_changed {
            // `[sprite] sheet` names dancer 0's sheet; the rest keep whatever the
            // troupe assignment gave them -- their sheets come from `[dancers]`,
            // and re-rolling a random troupe on every config save would make
            // editing an unrelated key reshuffle the screen.
            let path = self.cfg.sheet_path(&self.dir);
            self.dancers[0].sheet_path = path;
            self.dancers[0].reload_sheet(&self.cfg);
            self.reset_watch_sheets();
        } else if resize {
            // A hand-edited `[sprite] scale` resets every dancer to it, jitter
            // included: the file was touched on purpose, and keeping stale jitter
            // on top of a new base would make the edit look ignored.
            let scale = self.cfg.sprite.scale;
            for d in &mut self.dancers {
                d.set_scale(scale, &self.cfg);
            }
        }
        self.tray_shown = None;
        self.refresh_tray();
    }

    /// Where sheets live (spec §13's `artwork_dir`).
    fn artwork_dir(&self) -> PathBuf {
        let art = Path::new(&self.cfg.sprite.artwork_dir);
        if art.is_absolute() {
            art.to_path_buf()
        } else {
            self.dir.join(art)
        }
    }

    /// Switch to another sheet from the tray.
    ///
    /// Written to the config immediately, because picking a dancer is a preference
    /// and not a session setting — the surprise would be it reverting on restart.
    /// Switch the whole troupe to the sheet at this index of the tray's list.
    ///
    /// All dancers, deliberately: the tray is process-wide, and "which of the
    /// five identical menu entries moves which dancer" is not a puzzle to hand
    /// anyone. Per-dancer choice is what `[dancers] sheets` is for.
    fn select_sheet(&mut self, index: usize) {
        let Some(path) = self.sheets.get(index).cloned() else {
            return;
        };

        // Relative when it sits in the artwork folder, so a config written here
        // stays portable if the whole folder moves.
        self.cfg.sprite.sheet = match path.strip_prefix(self.artwork_dir()) {
            Ok(rel) => rel.to_string_lossy().into_owned(),
            Err(_) => path.to_string_lossy().into_owned(),
        };
        for d in &mut self.dancers {
            if d.sheet_path != path {
                d.sheet_path = path.clone();
                d.reload_sheet(&self.cfg);
            }
        }
        self.reset_watch_sheets();
        if let Err(e) = self.cfg.save(&self.dir) {
            tracing::warn!(error = %e, "could not save the chosen sheet");
        }
        // The tick moves to the new sheet, and the icon is cut from it.
        self.rebuild_tray();
    }

    /// Re-point the artwork watch at the sheets the troupe is actually wearing.
    fn reset_watch_sheets(&mut self) {
        let paths: Vec<PathBuf> = self.dancers.iter().map(|d| d.sheet_path.clone()).collect();
        if let Some(w) = self.watch.as_mut() {
            w.set_sheets(&paths);
        }
    }

    /// Throw the tray away and build it again.
    ///
    /// `muda` menus are built once, so anything that changes their *shape* — the
    /// list of sheets, which one is ticked, the icon — is a rebuild rather than an
    /// update. Cheap, and it happens only on an explicit user action.
    fn rebuild_tray(&mut self) {
        self.tray = None;
        self.tray_shown = None;
        self.build_tray();
    }

    /// Explain where dancers come from, on a worker thread so the dancer keeps going.
    fn show_sheet_help(&self) {
        let text = sheets::help_text(&self.artwork_dir());
        std::thread::spawn(move || dialog::info("Adding dancers", &text));
    }

    /// Fold in whatever the Yandex worker learned.
    fn drain_account(&mut self) {
        while let Ok(ev) = self.account_ch.rx.try_recv() {
            match ev {
                account::AccountEvent::Valid { login } => {
                    // The login is deliberately not logged. The tray shows it, so
                    // it is not a secret — but the log file exists to be pasted
                    // into a bug report, and an account name is not something
                    // anyone needs in order to read one.
                    tracing::info!("yandex token ok");
                    self.account = account::Status::Ok(login);
                }
                account::AccountEvent::Unknown(why) => {
                    // Explicitly *not* a prompt. A network blip is not a reason to
                    // ask someone to sign in again, and treating it as one trains
                    // people to dismiss the dialog that matters.
                    tracing::warn!(why, "could not check the yandex token");
                    self.account = account::Status::Unavailable;
                }
                account::AccountEvent::Rejected => {
                    tracing::warn!("yandex token rejected");
                    self.account = account::Status::Rejected;
                    self.offer_sign_in();
                }
                account::AccountEvent::SignedIn { token, login } => {
                    self.cfg.source.yandex.token = token;
                    // Signing in *is* the request; there is nothing left to opt into.
                    self.cfg.source.yandex.fetch_for_analysis = true;
                    if let Err(e) = self.cfg.save(&self.dir) {
                        tracing::warn!(error = %e, "could not save the token");
                    }
                    tracing::info!(login, "signed in to yandex");
                    self.account = account::Status::Ok(login.clone());
                    let dir = self.dir.clone();
                    std::thread::spawn(move || {
                        dialog::info(
                            "Signed in to Yandex Music",
                            &format!(
                                "Signed in as {login}.\n\n\
                                 Streamed tracks will now be fetched, analysed and \
                                 deleted immediately — only the beat grid is kept.\n\n\
                                 The token is stored in plain text in\n     {}\n\n\
                                 Revoke it any time at {}",
                                dir.join("config.toml").display(),
                                account::REVOKE_URL
                            ),
                        );
                    });
                }
                account::AccountEvent::SignInFailed(why) => {
                    tracing::warn!(why, "yandex sign-in did not complete");
                    // Back to whatever it was: a failed *new* sign-in does not
                    // invalidate a token that was already working.
                    if !matches!(self.account, account::Status::Ok(_)) {
                        self.account = account::Status::Off;
                    }
                    std::thread::spawn(move || {
                        dialog::error(
                            "Sign-in did not complete",
                            &format!(
                                "{why}\n\nNothing has changed. You can try again from \
                                 the tray menu whenever you like — the dancer works \
                                 without it, it just cannot analyse streamed tracks."
                            ),
                        );
                    });
                }
            }
        }
    }

    /// Ask whether to sign in again, after a token turned out to be dead.
    ///
    /// A question rather than a prompt that starts the flow: the feature is opt-in,
    /// and someone who revoked their token on purpose should not be walked back into
    /// authorising a new one by dismissing a dialog.
    fn offer_sign_in(&mut self) {
        let tx = self.account_ch.tx.clone();
        std::thread::spawn(move || {
            let yes = dialog::confirm(
                "Yandex sign-in expired",
                "The saved Yandex Music sign-in is no longer valid — it has expired \
                 or been revoked.\n\n\
                 Until you sign in again, streamed tracks cannot be analysed and the \
                 dancer will loop at a fixed rate for them. Everything else keeps \
                 working.\n\n\
                 Sign in again now?",
            );
            if yes {
                account::sign_in(tx);
            }
        });
    }

    /// What the tray should be showing right now.
    fn tray_state(&self) -> tray::State {
        // Dancer 0 speaks for the troupe: every playback consumes the same event
        // stream, so their states only differ transiently.
        let playback = &self.dancers[0].playback;
        tray::State {
            state: playback.state.name().to_string(),
            track: playback.track.as_ref().map(|t| {
                if t.artist.is_empty() {
                    t.title.clone()
                } else {
                    format!("{} — {}", t.artist, t.title)
                }
            }),
            click_through: self.cfg.window.click_through,
            always_on_top: self.cfg.window.always_on_top,
            anticipate: playback.anticipating(),
            offset_secs: self.cfg.playback.offset_secs,
            yandex: account::status_line(&self.account),
        }
    }

    /// Push current state into the tray menu, if anything it shows has changed.
    ///
    /// Compared before writing because `set_text` crosses into the shell, and a menu
    /// rewritten every 8 ms is both wasteful and capable of flickering one that is
    /// open.
    fn refresh_tray(&mut self) {
        let Some(tray) = self.tray.as_ref() else {
            return;
        };
        let now = self.tray_state();
        if self.tray_shown.as_ref() == Some(&now) {
            return;
        }
        tray.refresh(&now);
        self.tray_shown = Some(now);
    }

    /// Save and exit. The one path out, however it was asked for.
    fn quit(&mut self, el: &ActiveEventLoop) {
        self.store_position(el);
        if let Err(e) = self.cfg.save(&self.dir) {
            tracing::warn!(error = %e, "could not save the config on exit");
        }
        el.exit();
    }

    /// Remember where dancer 0 sits. The rest are placed relative to it on the
    /// next launch (see `resumed`).
    fn store_position(&mut self, el: &ActiveEventLoop) {
        let size = self.dancers[0].surface_size();
        let pos = self.dancers[0].pos;
        let mut best: Option<(usize, f32, f32)> = None;
        for (i, m) in el.available_monitors().enumerate() {
            let (mp, ms) = (m.position(), m.size());
            let inside = pos.0 >= mp.x
                && pos.1 >= mp.y
                && pos.0 < mp.x + ms.width as i32
                && pos.1 < mp.y + ms.height as i32;
            if inside {
                let span_x = (ms.width as i32 - size.0 as i32).max(1) as f32;
                let span_y = (ms.height as i32 - size.1 as i32).max(1) as f32;
                best = Some((
                    i,
                    ((pos.0 - mp.x) as f32 / span_x).clamp(0.0, 1.0),
                    ((pos.1 - mp.y) as f32 / span_y).clamp(0.0, 1.0),
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

    /// Drain the source thread's messages into the state machine.
    ///
    /// Draining here rather than waking the loop per message costs at most one
    /// frame of latency and no accuracy: every message carries the instant it
    /// describes.
    fn drain_events(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            // Broadcast: every dancer folds in the same observation against its
            // own clock. The clones are cheap — scores travel as `Arc` — and the
            // alternative, one shared clock, would only save them.
            for d in &mut self.dancers {
                d.playback.apply(ev.clone());
            }
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.dancers[0].window.is_some() {
            return;
        }

        // Dancer 0 sits where the config remembers; the rest cascade to its right,
        // one window-width apart, and stay wherever the user drags them. Only
        // dancer 0's position is persisted -- the rest are placed relative to it
        // on every launch, which survives monitor changes better than N saved
        // positions that may refer to screens that no longer exist.
        let size0 = self.dancers[0].surface_size();
        let base = self.initial_position(el, size0);
        let mut x = base.0;
        for i in 0..self.dancers.len() {
            let pos = (x, base.1);
            let (w, h) = self.dancers[i].surface_size();
            let cfg = &self.cfg;
            if !self.dancers[i].create_window(el, cfg, pos) {
                el.exit();
                return;
            }
            x += w as i32 + 12;

            tracing::info!(
                dancer = i,
                w, h,
                pos = ?pos,
                sheet = %self.dancers[i].sheet_path.display(),
                beats_per_loop = self.dancers[i].beats_per_loop(),
                "window up"
            );
        }
        tracing::info!(
            dancers = self.dancers.len(),
            idle_bpm = self.cfg.playback.idle_bpm,
            offset_secs = self.cfg.playback.offset_secs,
            anticipate = self.dancers[0].playback.anticipating(),
            click_through = self.cfg.window.click_through,
            "troupe up"
        );
        if self.cfg.window.click_through {
            tracing::warn!("click_through is on: the windows ignore the mouse, so they cannot be dragged or closed by clicking");
        }

        // After the windows, because the icon is cut from the loaded sheet, and on
        // the event-loop thread, because the tray owns a hidden message window that
        // has to be pumped by this loop.
        self.build_tray();

        // Checked at startup rather than at first use. A dead token otherwise
        // surfaces mid-track as a fetch that quietly does not happen, behind a
        // dancer that carries on looking fine -- see `account`.
        if !self.cfg.source.yandex.token.trim().is_empty() {
            self.account = account::Status::Checking;
            account::verify(self.cfg.source.yandex.token.clone(), self.account_ch.tx.clone());
        }

        el.set_control_flow(ControlFlow::WaitUntil(Instant::now()));
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // Route by window: with a troupe up, every dancer raises its own events.
        let Some(i) = self.dancers.iter().position(|d| d.window_id() == Some(id)) else {
            return;
        };
        match event {
            WindowEvent::CloseRequested => self.quit(el),
            WindowEvent::MouseInput { state, button, .. } => {
                tracing::debug!(dancer = i, ?button, ?state, dragging = self.dancers[i].drag.is_some(), "mouse");
                match (button, state) {
                    (MouseButton::Left, ElementState::Pressed) => {
                        if let Some(cursor) = cursor_pos() {
                            self.dancers[i].begin_drag(cursor);
                        }
                    }
                    (MouseButton::Left, ElementState::Released) => self.dancers[i].end_drag(),
                    // The A/B switch (ROADMAP M3). Middle-click flips between
                    // anticipation and M1's plain loop on the same track, which is
                    // the only way to judge the difference honestly -- described,
                    // it sounds like a detail; seen back to back, it is the point.
                    // All dancers together: half a troupe anticipating is neither
                    // arm of the experiment.
                    (MouseButton::Middle, ElementState::Pressed) => {
                        let on = self.toggle_anticipation_all();
                        tracing::info!(anticipate = on, "A/B toggle");
                    }
                    // A menu, not quit. Right-click-quit was M0's only control
                    // and a misclick away from data loss; Quit now lives inside
                    // this menu, so the sprite alone still suffices when the shell
                    // refuses a tray icon -- one deliberate step instead of zero.
                    (MouseButton::Right, ElementState::Pressed) => {
                        self.dancers[i].end_drag();
                        self.show_context_menu(i);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        self.drain_events();
        self.drain_watch();
        self.drain_account();
        self.drain_menus(el);
        if el.exiting() {
            return;
        }

        // While dragging, track the cursor in screen coordinates. Window-relative
        // events are useless here because the window moves with the pointer.
        let button_down = primary_button_down();
        let cursor = cursor_pos();
        for d in &mut self.dancers {
            if d.drag.is_some() && !button_down {
                // Belt and braces: capture should make a lost release impossible,
                // but a wedged drag leaves the window glued to the cursor, which is
                // bad enough to detect directly rather than trusting the events.
                tracing::debug!("button released without a Released event; ending drag");
                d.end_drag();
            }
            if let (Some(off), Some(cursor)) = (d.drag, cursor) {
                let want = (cursor.0 - off.0, cursor.1 - off.1);
                if want != d.pos {
                    d.move_to(want, &self.cfg);
                }
            }
        }

        let now = Instant::now();
        let mut next = now + Duration::from_secs(3600);
        for d in &mut self.dancers {
            next = next.min(d.tick(now, &self.cfg));
        }
        el.set_control_flow(ControlFlow::WaitUntil(next.max(now)));
    }
}

fn cursor_pos() -> Option<(i32, i32)> {
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p).ok().map(|_| (p.x, p.y)) }
}
