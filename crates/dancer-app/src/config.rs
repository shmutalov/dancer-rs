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
