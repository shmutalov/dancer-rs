//! Logging to a file beside the executable.
//!
//! # Why this exists
//!
//! A release build is `windows_subsystem = "windows"` and has no console, so every
//! `tracing` line the app emits went nowhere unless it was launched with a
//! command-line flag. That is fine right up until something misbehaves in normal
//! use — and then the one artefact that would explain it does not exist. A user
//! reporting "it loses sync after one song" had no way to show what happened, and
//! neither did the app.
//!
//! So: the same stream, written to a file, kept small, and living in the data
//! directory with everything else this app writes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing_subscriber::fmt::writer::MakeWriterExt;

/// Log file name, in the data directory (spec §13's portable layout).
pub const LOG_FILE: &str = "dancer-rs.log";

/// Rotate once past this. Two files, so a long session cannot fill a disk while
/// still leaving enough history to cover a reproduction.
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// What gets logged when `RUST_LOG` says nothing.
///
/// Names every crate whose events explain a misbehaving session — the source
/// adapter, the clock, the scheduler — because the default is what a user will be
/// running when something goes wrong. `dancer_rs` is the binary's crate name;
/// `dancer_app` matches nothing.
const DEFAULT_FILTER: &str = "dancer_rs=info,dancer_source=info,dancer_clock=info,\
dancer_choreo=info,dancer_analyze=info,dancer_yandex=info,dancer_render=info,\
dancer_sprite=info,dancer_score=info";

/// Install the subscriber. Returns the log file path, if there is one.
///
/// Never fails the app: a read-only directory or a file held open by something
/// else means no file logging, not no dancer.
pub fn init(dir: &Path) -> Option<PathBuf> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| DEFAULT_FILTER.into());

    let path = dir.join(LOG_FILE);
    rotate(&path);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok();

    match file {
        Some(f) => {
            // Tee: the console when there is one, the file always. ANSI off for
            // both, because escape codes in a file people are asked to paste are
            // noise at best.
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .with_ansi(false)
                .with_writer(std::io::stderr.and(Arc::new(f)))
                .init();
            Some(path)
        }
        None => {
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_target(false)
                .init();
            None
        }
    }
}

/// Move an oversized log aside, keeping exactly one generation.
///
/// Deliberately at startup rather than mid-run: rotating while running means
/// holding the handle and swapping it under the writer, and the value of that over
/// a per-launch check is nil for a desktop toy that gets restarted often.
fn rotate(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.len() < MAX_BYTES {
        return;
    }
    let old = path.with_extension("log.1");
    // Both failures are survivable and neither is worth a message the user cannot
    // act on: worst case the log keeps growing, and the size check will try again
    // next launch.
    let _ = std::fs::remove_file(&old);
    let _ = std::fs::rename(path, &old);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dancer-log-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_small_log_is_left_alone() {
        let d = tmp("small");
        let p = d.join(LOG_FILE);
        std::fs::write(&p, b"one line\n").unwrap();
        rotate(&p);
        assert_eq!(std::fs::read(&p).unwrap(), b"one line\n");
        assert!(!p.with_extension("log.1").exists());
    }

    #[test]
    fn an_oversized_log_is_moved_aside_and_only_one_generation_is_kept() {
        let d = tmp("big");
        let p = d.join(LOG_FILE);
        std::fs::write(&p, vec![b'x'; (MAX_BYTES + 1) as usize]).unwrap();
        rotate(&p);
        assert!(!p.exists(), "the live log should have been moved");
        assert!(p.with_extension("log.1").exists());

        // A second rotation must not accumulate .2, .3, ...
        std::fs::write(&p, vec![b'y'; (MAX_BYTES + 1) as usize]).unwrap();
        rotate(&p);
        let generations = std::fs::read_dir(&d).unwrap().count();
        assert_eq!(generations, 1, "one rotated file, and the live one is gone");
    }

    #[test]
    fn rotating_a_log_that_does_not_exist_is_not_an_error() {
        rotate(&tmp("missing").join(LOG_FILE));
    }
}
