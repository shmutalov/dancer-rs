//! Hot reload for the config and the artwork (ROADMAP M5).
//!
//! Both are things people fiddle with while the app is running — the sprite scale,
//! the sheet, the window flags — and restarting to see the effect is the difference
//! between adjusting something and giving up on it.
//!
//! # What this deliberately does not reload
//!
//! Only presentation. `[sprite]`, `[window]` and `[playback]` are re-read; sources,
//! the Yandex token and the library folders are not, because those own live threads
//! and half-swapping a running source is a much larger change than it looks. A
//! restart is honest for those and rare in practice.
//!
//! # Why it debounces
//!
//! Editors do not write files once. Notepad, VS Code and every atomic-save editor
//! produce a burst of create/modify/rename events for one Ctrl-S, and a reload per
//! event means re-decoding a 1.6 MB PNG several times in a row. Worse, a write
//! caught mid-flight parses as a truncated file — so a change is only acted on
//! after the writes have stopped.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use notify::{Event, RecursiveMode, Watcher};

/// How long a path must be quiet before it is treated as written.
///
/// Long enough to cover an editor's save burst, short enough to feel immediate.
pub const DEBOUNCE: Duration = Duration::from_millis(400);

/// What changed on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Config,
    Artwork,
}

pub struct Watch {
    // Dropping the watcher stops the thread, so it is held even though the events
    // arrive through the channel rather than through this handle.
    _watcher: notify::RecommendedWatcher,
    rx: mpsc::Receiver<notify::Result<Event>>,
    config: PathBuf,
    sheets: Vec<PathBuf>,
    /// Pending changes and when their last event arrived.
    pending: Vec<(Change, Instant)>,
}

impl Watch {
    /// Watch the config file and the sheet's directory.
    ///
    /// Directories rather than files: an atomic save replaces the file, which
    /// destroys a watch registered on the inode. Watching the parent survives that,
    /// at the cost of having to filter events by path.
    pub fn new(config: &Path, sheets: &[PathBuf]) -> notify::Result<Self> {
        let (tx, rx) = mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx)?;

        // Each *distinct* parent once: a troupe usually draws every sheet from
        // one artwork folder, and watching it N times would deliver N copies of
        // every event.
        let mut dirs: Vec<&Path> = config.parent().into_iter().collect();
        for s in sheets {
            if let Some(d) = s.parent() {
                if !dirs.contains(&d) {
                    dirs.push(d);
                }
            }
        }
        for dir in dirs {
            // A missing artwork directory is not fatal — the sheet may have been
            // given by absolute path, or the folder may appear later.
            if let Err(e) = watcher.watch(dir, RecursiveMode::NonRecursive) {
                tracing::warn!(dir = %dir.display(), error = %e, "not watching");
            }
        }

        Ok(Self {
            _watcher: watcher,
            rx,
            config: config.to_path_buf(),
            sheets: sheets.to_vec(),
            pending: Vec::new(),
        })
    }

    /// Changes that have settled. Call once per event-loop pass.
    pub fn poll(&mut self, now: Instant) -> Vec<Change> {
        while let Ok(ev) = self.rx.try_recv() {
            let Ok(ev) = ev else { continue };
            for path in &ev.paths {
                let Some(what) = self.classify(path) else { continue };
                match self.pending.iter_mut().find(|(c, _)| *c == what) {
                    Some(slot) => slot.1 = now,
                    None => self.pending.push((what, now)),
                }
            }
        }

        let mut ready = Vec::new();
        self.pending.retain(|(what, last)| {
            if now.duration_since(*last) >= DEBOUNCE {
                ready.push(*what);
                false
            } else {
                true
            }
        });
        ready
    }

    /// Which of the watched things a path is, if any.
    ///
    /// The sheet's sidecars count as artwork: editing `<stem>.toml` to retune an
    /// `impact_cell` is the single most common reason to want a reload at all, and
    /// it never touches the PNG.
    fn classify(&self, path: &Path) -> Option<Change> {
        if same_file(path, &self.config) {
            return Some(Change::Config);
        }
        for sheet in &self.sheets {
            let Some(stem) = sheet.file_stem() else { continue };
            if path.parent() == sheet.parent() && path.file_stem() == Some(stem) {
                return Some(Change::Artwork);
            }
        }
        None
    }

    /// Point the artwork watch at a different set of sheets, after the config or
    /// the tray named them. Only classification changes — the watched directories
    /// were fixed at construction, which is fine while every sheet lives in the
    /// artwork folder the tray offers them from.
    pub fn set_sheets(&mut self, sheets: &[PathBuf]) {
        self.sheets = sheets.to_vec();
    }
}

/// Compare paths without touching the filesystem.
///
/// Case-insensitive because Windows is, and an editor that reports `Config.toml`
/// for a file opened as `config.toml` would otherwise be silently ignored.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || a.file_name().zip(b.file_name()).is_some_and(|(x, y)| {
            a.parent() == b.parent() && x.eq_ignore_ascii_case(y)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watch() -> Watch {
        // No watcher registrations are exercised here; only the classification and
        // debounce logic, which is where the behaviour worth testing lives.
        let (_tx, rx) = mpsc::channel();
        Watch {
            _watcher: notify::recommended_watcher(mpsc::channel().0).unwrap(),
            rx,
            config: PathBuf::from("C:/app/config.toml"),
            sheets: vec![PathBuf::from("C:/app/assets/dance.png")],
            pending: Vec::new(),
        }
    }

    #[test]
    fn the_config_and_the_sheet_are_told_apart() {
        let w = watch();
        assert_eq!(w.classify(Path::new("C:/app/config.toml")), Some(Change::Config));
        assert_eq!(
            w.classify(Path::new("C:/app/assets/dance.png")),
            Some(Change::Artwork)
        );
    }

    #[test]
    fn a_sidecar_counts_as_artwork() {
        // Retuning an impact_cell in `<stem>.toml` is the most common reason to
        // reload, and it never touches the PNG.
        let w = watch();
        assert_eq!(
            w.classify(Path::new("C:/app/assets/dance.toml")),
            Some(Change::Artwork)
        );
        assert_eq!(
            w.classify(Path::new("C:/app/assets/dance.txt")),
            Some(Change::Artwork)
        );
    }

    #[test]
    fn unrelated_files_in_the_same_folder_are_ignored() {
        // The watch is on directories, so everything beside the sheet arrives too.
        let w = watch();
        assert_eq!(w.classify(Path::new("C:/app/assets/other.png")), None);
        assert_eq!(w.classify(Path::new("C:/app/scores.db")), None);
    }

    #[test]
    fn every_dancers_sheet_counts_as_artwork() {
        // A troupe loads several sheets; an edit to any of them must reload, or
        // hot reload silently works for dancer 0 only.
        let mut w = watch();
        w.set_sheets(&[
            PathBuf::from("C:/app/assets/dance.png"),
            PathBuf::from("C:/app/assets/uma.png"),
        ]);
        assert_eq!(w.classify(Path::new("C:/app/assets/uma.toml")), Some(Change::Artwork));
        assert_eq!(w.classify(Path::new("C:/app/assets/dance.png")), Some(Change::Artwork));
        assert_eq!(w.classify(Path::new("C:/app/assets/other.png")), None);
    }

    #[test]
    fn config_matching_ignores_case() {
        // Windows is case-insensitive, and editors do not always echo back the
        // spelling a path was opened with.
        let w = watch();
        assert_eq!(w.classify(Path::new("C:/app/CONFIG.TOML")), Some(Change::Config));
    }

    #[test]
    fn a_change_is_held_until_the_writes_stop() {
        let mut w = watch();
        let t0 = Instant::now();

        w.pending.push((Change::Artwork, t0));
        assert!(w.poll(t0).is_empty(), "not settled yet");

        // A second write inside the window restarts the clock: an atomic save is a
        // burst, and reloading mid-burst reads a truncated file.
        let mid = t0 + DEBOUNCE / 2;
        w.pending[0].1 = mid;
        assert!(w.poll(mid + DEBOUNCE / 2).is_empty(), "burst restarted the wait");

        assert_eq!(w.poll(mid + DEBOUNCE), vec![Change::Artwork]);
        assert!(w.poll(mid + DEBOUNCE * 2).is_empty(), "delivered exactly once");
    }

    #[test]
    fn repeated_events_collapse_into_one_change() {
        // One Ctrl-S produces several events; it must produce one reload.
        let mut w = watch();
        let t0 = Instant::now();
        for _ in 0..8 {
            match w.pending.iter_mut().find(|(c, _)| *c == Change::Config) {
                Some(slot) => slot.1 = t0,
                None => w.pending.push((Change::Config, t0)),
            }
        }
        assert_eq!(w.pending.len(), 1);
        assert_eq!(w.poll(t0 + DEBOUNCE), vec![Change::Config]);
    }
}
