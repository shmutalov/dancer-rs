//! Getting a score for a track: cache first, analyser second (spec §8.3).
//!
//! The order matters and is the whole point of the cache. Analysis costs seconds
//! and produces the same answer every time, so a track played twice should be
//! instant the second time — and a track played through *another application*
//! should be recognised at all, which is what the `library` table is for.
//!
//! This runs on its own thread. Analysis is 41–74× realtime (Phase 0.1), so a
//! 3½-minute track takes about five seconds — nowhere near enough to block a
//! render loop for.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crossbeam_channel::Sender;
use dancer_analyze::Analyzer;
use dancer_score::{Score, Store, TrackId, TrackMeta};

use crate::events::AppEvent;

/// Where a score came from, for logging and for the M2 exit criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Cache,
    Analyzed,
}

/// Resolve a score for `meta`, analysing `path` if the cache has nothing.
///
/// Returns `None` when there is no score and none can be made — a missing model,
/// an undecodable file, or audio with no beats in it. All three are `Unscored`,
/// not errors: the dancer keeps dancing, it just stops claiming to know the tempo.
pub fn resolve(
    store: Option<&Store>,
    analyzer: Option<&mut Analyzer>,
    id: &TrackId,
    meta: &TrackMeta,
    path: &Path,
) -> Option<(Score, Origin)> {
    if let Some(store) = store {
        match store.get_score(&id.key()) {
            Ok(Some(score)) => {
                tracing::info!(track = %id, "score from cache");
                return Some((score, Origin::Cache));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "cache read failed; analysing instead"),
        }

        // Not analysed under this id, but perhaps under another — the same file
        // reached through a different path, or reported by a player rather than
        // opened directly (spec §6.2).
        match store.lookup(meta) {
            Ok(Some(score)) => {
                tracing::info!(track = %id, key = %score.track_id, "score via library index");
                return Some((score, Origin::Cache));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "library lookup failed"),
        }
    }

    let analyzer = analyzer?;
    let score = match analyzer.analyze_file(path, id) {
        Ok(s) => s,
        Err(e) => {
            // Never fatal. A track we cannot analyse is a track we dance to
            // without a grid, which is exactly what `Unscored` is for.
            tracing::warn!(path = %path.display(), error = %e, "analysis failed");
            return None;
        }
    };

    if score.beats.is_empty() {
        // Phase 0.1: non-musical input returns an empty grid rather than a
        // hallucinated one. Caching that would be caching a non-answer.
        tracing::info!(path = %path.display(), "no beats found; staying Unscored");
        return None;
    }

    if let Some(store) = store {
        if let Err(e) = store.record_analysis(id, meta, path, &score) {
            // A cache we cannot write is a slow app, not a broken one.
            tracing::warn!(error = %e, "caching the score failed");
        }
    }
    Some((score, Origin::Analyzed))
}

/// Run [`resolve`] off the render thread and deliver the result as an `AppEvent`.
pub fn spawn(
    db: Option<PathBuf>,
    models: PathBuf,
    id: TrackId,
    meta: TrackMeta,
    path: PathBuf,
    tx: Sender<AppEvent>,
) {
    let name = "analyze".to_string();
    let spawned = std::thread::Builder::new().name(name).spawn(move || {
        // Opened on this thread: a rusqlite Connection is not Sync, and the
        // render thread has no business holding one anyway.
        let store = db.and_then(|p| match Store::open(&p) {
            Ok(s) => Some(s),
            Err(e) => {
                tracing::warn!(path = %p.display(), error = %e, "opening the score cache failed");
                None
            }
        });

        // Only pay for model loading if the cache misses.
        let cached = store.as_ref().and_then(|s| {
            s.get_score(&id.key())
                .ok()
                .flatten()
                .or_else(|| s.lookup(&meta).ok().flatten())
        });

        let resolved = match cached {
            Some(score) => {
                tracing::info!(track = %id, "score from cache");
                Some((score, Origin::Cache))
            }
            None => {
                let mut analyzer = match Analyzer::new(&models) {
                    Ok(a) => Some(a),
                    Err(e) => {
                        tracing::warn!(error = %e, "no analyzer");
                        None
                    }
                };
                resolve(store.as_ref(), analyzer.as_mut(), &id, &meta, &path)
            }
        };

        if let Some((score, origin)) = resolved {
            tracing::info!(
                track = %id,
                origin = ?origin,
                bpm = score.bpm,
                meter = score.meter,
                confidence = score.confidence,
                "score ready"
            );
            let _ = tx.send(AppEvent::ScoreReady {
                id,
                score: Arc::new(score),
            });
        }
    });

    if let Err(e) = spawned {
        tracing::error!(error = %e, "could not start the analysis thread");
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use dancer_score::{ScoreSource, SCHEMA};

    use super::*;

    fn score(key: &str) -> Score {
        Score {
            schema: SCHEMA,
            track_id: key.into(),
            duration_ms: 10_000,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::BeatThis,
            confidence: 0.9,
            analyzed_at: "2026-08-15T00:00:00Z".into(),
            beats: (0..20).map(|i| i as f64 * 0.5).collect(),
            beat_positions: (0..20).map(|i| (i % 4 + 1) as u8).collect(),
            downbeats: vec![0.0],
            segments: vec![],
            cues: vec![],
            beat_energy: vec![],
        }
    }

    fn meta(title: &str) -> TrackMeta {
        TrackMeta {
            id: TrackId::new("file", "a.wav"),
            title: title.into(),
            artist: String::new(),
            duration_secs: Some(10.0),
        }
    }

    #[test]
    fn cache_hit_avoids_the_analyzer_entirely() {
        let store = Store::open_in_memory().unwrap();
        let id = TrackId::new("file", "a.wav");
        store.put_score(&score(&id.key())).unwrap();

        // No analyzer at all: if this resolves, nothing tried to analyse.
        let (got, origin) = resolve(Some(&store), None, &id, &meta("a"), Path::new("a.wav")).unwrap();
        assert_eq!(origin, Origin::Cache);
        assert_eq!(got.bpm, 120.0);
    }

    #[test]
    fn library_index_finds_a_score_stored_under_another_id() {
        // The §8.3 mechanism: a player reports (title, artist), not a path.
        let store = Store::open_in_memory().unwrap();
        let analysed = TrackId::new("file", "D:/music/a.wav");
        store
            .record_analysis(
                &analysed,
                &meta("Song 2"),
                Path::new("D:/music/a.wav"),
                &score(&analysed.key()),
            )
            .unwrap();

        // Same track, reached under a different id.
        let played = TrackId::new("smtc", "whatever");
        let (got, origin) =
            resolve(Some(&store), None, &played, &meta("song 2"), Path::new("x")).unwrap();
        assert_eq!(origin, Origin::Cache);
        assert_eq!(got.track_id, analysed.key());
    }

    #[test]
    fn no_cache_and_no_analyzer_is_unscored_not_an_error() {
        let id = TrackId::new("file", "a.wav");
        assert!(resolve(None, None, &id, &meta("a"), Path::new("a.wav")).is_none());
    }
}
