//! Configuration and the portable data directory (spec §13).
//!
//! Everything the app owns lives in one folder beside the executable, falling
//! back to `%LOCALAPPDATA%` only when that directory is not writable. The fallback
//! is decided by *attempting a write*, not by inspecting the path — Program Files
//! detection by string matching is unreliable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub sprite: Sprite,
    pub window: WindowCfg,
    pub playback: Playback,
    pub source: SourceCfg,
    pub library: LibraryCfg,
}

/// Where the user's own music lives (spec §8.3, §13).
///
/// Analysis needs a file it can read, and SMTC never reports a path — so the
/// library index is the whole bridge between "something is playing" and "we have a
/// grid for it". Until M5 these folders could only be given as `--scan` arguments,
/// which meant the primary path of the product was reachable only from a terminal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryCfg {
    /// Folders scanned by `--scan` when no folder is named on the command line.
    ///
    /// Empty by default. Guessing at `%USERPROFILE%\Music` would mean a first run
    /// that silently spends minutes analysing whatever happens to be there.
    pub folders: Vec<String>,
}

impl LibraryCfg {
    pub fn paths(&self) -> Vec<PathBuf> {
        self.folders
            .iter()
            .map(|f| f.trim())
            .filter(|f| !f.is_empty())
            .map(PathBuf::from)
            .collect()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct SourceCfg {
    /// Media sessions allowed to drive the dancer (spec §6.2).
    ///
    /// Empty means "whatever is playing", which is the right default: a shipped
    /// table of English executable names would silently exclude anyone not running
    /// an English install. Phase 0.5 found the Yandex Music desktop app
    /// identifying as `Яндекс Музыка.exe`. The app logs every session it sees, so
    /// this can be filled in from observation.
    pub allowlist: Vec<String>,
    pub yandex: YandexCfg,
}

/// Yandex Music, for streamed tracks only (spec §6.4).
///
/// Off unless a token is present, and that is deliberate. This is the one part of
/// the app that reaches out and fetches audio, so it does not start doing so
/// because a default said it could — the user has to supply a credential, which is
/// an unambiguous act of asking for it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct YandexCfg {
    /// OAuth token. Empty disables the whole path.
    pub token: String,
    /// Whether to fetch a streamed track in order to analyse it.
    ///
    /// Requires `token`, and applies only to the track currently playing. The
    /// audio is deleted as soon as the grid is built — nothing is kept and nothing
    /// is redistributed. There is deliberately no batch or playlist mode.
    pub fetch_for_analysis: bool,
}

impl YandexCfg {
    pub fn enabled(&self) -> bool {
        self.fetch_for_analysis && !self.token.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    /// Output latency in seconds (spec §9.2), the nudge slider's stored value.
    ///
    /// Everything between "the player says 42.0 s" and "the sound reaches the
    /// speakers". At 128 BPM a beat is 469 ms, so leaving this at zero can put the
    /// dancer more than half a beat out — it is not a rounding concern.
    pub offset_secs: f64,
    /// How often to ask the source where it is.
    pub poll_secs: f64,
    /// Poll cadence for SMTC, which bounds how late a track change is noticed.
    ///
    /// Faster than `poll_secs` because a stale reading costs nothing here — every
    /// SMTC reading carries its own anchor — while a late track change means
    /// dancing to the previous song's grid.
    pub smtc_poll_secs: f64,
}

impl Playback {
    /// Spec §9.2's local-playback default, and what `Reset offset` returns to.
    pub fn default_offset() -> f64 {
        0.180
    }

    pub fn smtc_poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f64(self.smtc_poll_secs.clamp(0.05, 10.0))
    }
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            // Browsers want ~250 ms; per-source values are still future work. The
            // tray nudges this live (spec §9.2).
            offset_secs: Self::default_offset(),
            poll_secs: 2.0,
            smtc_poll_secs: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Sprite {
    /// Sheet basename or path, resolved against `artwork_dir` when relative.
    pub sheet: String,
    pub artwork_dir: String,
    pub scale: f32,
    pub mirror: bool,
    pub opacity: f32,
    /// Fixed playback rate until the clock lands in M1.
    pub fps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowCfg {
    pub always_on_top: bool,
    pub click_through: bool,
    /// Monitor index, paired with normalised coordinates so the position survives
    /// resolution and DPI changes (spec §12).
    pub monitor: usize,
    pub x: f32,
    pub y: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sprite: Sprite::default(),
            window: WindowCfg::default(),
            playback: Playback::default(),
            source: SourceCfg::default(),
            library: LibraryCfg::default(),
        }
    }
}

impl Default for Sprite {
    fn default() -> Self {
        Self {
            sheet: "default.png".into(),
            artwork_dir: "assets".into(),
            scale: 1.0,
            mirror: false,
            opacity: 1.0,
            fps: 12,
        }
    }
}

impl Default for WindowCfg {
    fn default() -> Self {
        Self {
            always_on_top: true,
            click_through: false,
            monitor: 0,
            x: 0.82,
            y: 0.65,
        }
    }
}

impl Config {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => {
                    tracing::info!(path = %path.display(), "loaded config");
                    c
                }
                Err(e) => {
                    // A broken config should not stop the dancer appearing.
                    tracing::warn!(path = %path.display(), error = %e, "config invalid, using defaults");
                    Self::default()
                }
            },
            Err(_) => {
                tracing::info!(path = %path.display(), "no config, using defaults");
                Self::default()
            }
        }
    }

    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        let path = dir.join("config.toml");
        std::fs::write(&path, toml::to_string_pretty(self)?)?;
        tracing::debug!(path = %path.display(), "saved config");
        Ok(())
    }

    /// Absolute path to the configured sheet.
    pub fn sheet_path(&self, dir: &Path) -> PathBuf {
        let sheet = Path::new(&self.sprite.sheet);
        if sheet.is_absolute() {
            return sheet.to_owned();
        }
        let art = Path::new(&self.sprite.artwork_dir);
        let base = if art.is_absolute() {
            art.to_owned()
        } else {
            dir.join(art)
        };
        base.join(sheet)
    }
}

/// Resolve the portable data directory, or fall back if it is read-only.
pub fn data_dir() -> PathBuf {
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        if is_writable(&exe_dir) {
            return exe_dir;
        }
        tracing::info!(dir = %exe_dir.display(), "exe directory not writable, falling back");
    }
    let fallback = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("dancer-rs");
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// Probe writability by writing, since path inspection is not reliable.
fn is_writable(dir: &Path) -> bool {
    let probe = dir.join(".dancer-write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}
