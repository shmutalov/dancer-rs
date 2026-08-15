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
#[cfg(test)]
use dancer_score::ScoreSource;

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
        // Index under the file's **own tags**, not under whatever the caller
        // happened to name the track. SMTC reports those tags (spec §5.1, §6.2),
        // so anything else guarantees a miss when the same file is later played
        // through the user's own player — which is the entire point of the index.
        let tags = dancer_analyze::tags::read(path);
        let indexed = TrackMeta {
            id: meta.id.clone(),
            title: tags.title_or_stem(path),
            artist: tags.artist_or_empty(),
            duration_secs: Some(score.duration_secs()),
        };
        tracing::info!(
            title = %indexed.title,
            artist = %indexed.artist,
            "indexed under the file's tags"
        );
        if let Err(e) = store.record_analysis(id, &indexed, path, &score) {
            // A cache we cannot write is a slow app, not a broken one.
            tracing::warn!(error = %e, "caching the score failed");
        }
    }
    Some((score, Origin::Analyzed))
}

/// Cache-only lookup for a track we have no file for (spec §6.2, §8.3).
///
/// SMTC reports `(title, artist)` and never a path, so there is nothing to analyse
/// — either the track was analysed earlier and the `library` table remembers where
/// it lives, or the dancer stays `Unscored`. This is the join that makes owned
/// music work through the user's own player, and it is the point of M4.
pub fn spawn_lookup(
    db: Option<PathBuf>,
    meta: TrackMeta,
    fallback: Option<StreamFallback>,
    tx: Sender<AppEvent>,
) {
    let spawned = std::thread::Builder::new()
        .name("library-lookup".into())
        .spawn(move || {
            let Some(db) = db else { return };
            let store = match Store::open(&db) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(path = %db.display(), error = %e, "opening the score cache failed");
                    return;
                }
            };

            // The streamed-track key, checked before reaching for the network:
            // a track fetched once is never fetched again.
            if let Some(fb) = fallback.as_ref() {
                if let Ok(Some(score)) = store.get_score(&fb.cache_key(&meta)) {
                    tracing::info!(title = %meta.title, "streamed track already analysed");
                    let _ = tx.send(AppEvent::ScoreReady {
                        id: meta.id,
                        score: Arc::new(score),
                    });
                    return;
                }
            }

            match store.lookup(&meta) {
                Ok(Some(score)) => {
                    tracing::info!(
                        title = %meta.title,
                        artist = %meta.artist,
                        key = %score.track_id,
                        confidence = score.confidence,
                        "library hit"
                    );
                    let _ = tx.send(AppEvent::ScoreReady {
                        id: meta.id,
                        score: Arc::new(score),
                    });
                }
                Ok(None) => {
                    // Expected and cheap. Spec §5.1 chose the miss deliberately:
                    // hashing raw strings can fail to match, and that costs one
                    // re-analysis, whereas canonicalising could match the *wrong*
                    // master and apply a grid that is confidently off.
                    tracing::info!(
                        title = %meta.title,
                        artist = %meta.artist,
                        "no score in the library"
                    );
                    // Nothing owned matches. The streamed path is the last resort
                    // and only runs if the user turned it on.
                    match fallback {
                        Some(fb) => fb.run(&store, &meta, &tx),
                        None => tracing::info!("staying Unscored"),
                    }
                }
                Err(e) => tracing::warn!(error = %e, "library lookup failed"),
            }
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "could not start the lookup thread");
    }
}

/// Building a grid for a track that exists only as a stream (spec §6.4).
///
/// Split behind a struct so the SMTC path reads the same whether or not the
/// feature is compiled in, and so the "only when explicitly enabled" condition
/// lives in exactly one place.
#[derive(Clone)]
pub struct StreamFallback {
    #[cfg(feature = "yandex")]
    pub token: String,
    // Unread without the feature, and kept anyway so the type is identical in both
    // builds — the SMTC path should not be shaped by whether this is compiled in.
    #[cfg_attr(not(feature = "yandex"), allow(dead_code))]
    pub models: PathBuf,
    #[cfg_attr(not(feature = "yandex"), allow(dead_code))]
    pub scratch: PathBuf,
}

impl StreamFallback {
    /// Cache key for a streamed track, namespaced away from local files.
    ///
    /// Keyed on the reported strings rather than a Yandex id, because the id is
    /// not known until after a search — and the whole point is to avoid searching
    /// twice for the same song.
    pub fn cache_key(&self, meta: &TrackMeta) -> String {
        format!("stream:{:016x}", meta.library_key())
    }

    #[cfg(feature = "yandex")]
    fn run(&self, store: &Store, meta: &TrackMeta, tx: &Sender<AppEvent>) {
        use dancer_yandex::Yandex;

        tracing::info!(
            title = %meta.title,
            artist = %meta.artist,
            "fetching this track to analyse it; the audio is deleted afterwards"
        );

        let yandex = match Yandex::new(&self.token, self.scratch.clone()) {
            Ok(y) => y,
            Err(e) => {
                tracing::warn!(error = %e, "yandex unavailable; staying Unscored");
                return;
            }
        };
        let mut analyzer = match Analyzer::new(&self.models) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(error = %e, "no analyzer; staying Unscored");
                return;
            }
        };

        // A single-threaded runtime, built here and dropped here: this is the only
        // async code in the app and it should not impose a runtime on anything else.
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "no runtime; staying Unscored");
                return;
            }
        };

        let mut score = match runtime.block_on(yandex.score_for(meta, &mut analyzer)) {
            Ok(s) => s,
            Err(e) => {
                // Every failure here is a degradation, never a crash: no match, no
                // network, a changed API. The dancer keeps dancing.
                tracing::info!(error = %e, "could not build a grid; staying Unscored");
                return;
            }
        };

        // Re-key onto the reported strings so the next play is a cache hit without
        // another search.
        score.track_id = self.cache_key(meta);
        if let Err(e) = store.put_score(&score) {
            tracing::warn!(error = %e, "caching the streamed grid failed");
        }

        let _ = tx.send(AppEvent::ScoreReady {
            id: meta.id.clone(),
            score: Arc::new(score),
        });
    }

    #[cfg(not(feature = "yandex"))]
    fn run(&self, _store: &Store, _meta: &TrackMeta, _tx: &Sender<AppEvent>) {
        tracing::info!("built without the yandex feature; staying Unscored");
    }
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

/// Extensions worth opening. Everything symphonia handles that anyone keeps music
/// in; unknown extensions are skipped rather than probed, so a folder of PDFs
/// costs nothing.
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "wav", "m4a", "aac", "ogg", "oga", "opus", "wv", "aiff", "aif", "alac", "mka",
];

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| AUDIO_EXTENSIONS.contains(&e.as_str()))
}

/// Every audio file under `root`, depth-first.
///
/// Hand-rolled rather than a `walkdir` dependency: the whole requirement is
/// "recurse and skip what you cannot read". Unreadable directories are logged and
/// stepped over — a permission error deep in a music folder should not abandon the
/// scan of everything else.
pub fn find_audio(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(dir = %root.display(), error = %e, "skipping unreadable directory");
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            // Symlinks are not followed: a loop would hang the scan, and music
            // folders are exactly where people put junctions to other drives.
            Ok(t) if t.is_dir() => find_audio(&path, out),
            Ok(t) if t.is_file() && is_audio(&path) => out.push(path),
            _ => {}
        }
    }
}

/// Result of scanning one folder tree.
#[derive(Debug, Default, PartialEq)]
pub struct ScanReport {
    pub found: usize,
    pub analysed: usize,
    pub cached: usize,
    pub failed: usize,
}

/// Analyse a music folder into the cache (spec §13).
///
/// This is what makes the SMTC source useful: it reports `(title, artist)` and
/// never a path, so it can only ever find a track the library already knows. With
/// an empty cache every track misses and the dancer stays `Unscored` forever —
/// correct, and useless.
///
/// Resumable by construction: a file already in `scores` is skipped without
/// opening it, so an interrupted scan costs only the track it was on.
pub fn scan(
    roots: &[PathBuf],
    db: Option<&Path>,
    models: &Path,
    progress: &mut dyn FnMut(&ScanReport, &Path),
) -> ScanReport {
    let mut files = Vec::new();
    for root in roots {
        find_audio(root, &mut files);
    }
    files.sort();
    files.dedup();

    let mut report = ScanReport {
        found: files.len(),
        ..Default::default()
    };
    if files.is_empty() {
        return report;
    }

    let store = db.and_then(|p| match Store::open(p) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(path = %p.display(), error = %e, "cannot open the score cache");
            None
        }
    });

    // Loaded lazily: a scan that turns out to be entirely cached should not pay
    // for 10 MB of model weights.
    let mut analyzer: Option<Analyzer> = None;

    for path in &files {
        let id = TrackId::new("file", path.to_string_lossy());

        if let Some(s) = store.as_ref() {
            if matches!(s.get_score(&id.key()), Ok(Some(_))) {
                report.cached += 1;
                progress(&report, path);
                continue;
            }
        }

        if analyzer.is_none() {
            match Analyzer::new(models) {
                Ok(a) => analyzer = Some(a),
                Err(e) => {
                    // Without models nothing further can be analysed; stop rather
                    // than logging the same failure once per track.
                    tracing::error!(error = %e, "cannot analyse");
                    report.failed += files.len() - report.cached - report.analysed;
                    return report;
                }
            }
        }

        let meta = TrackMeta {
            id: id.clone(),
            title: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            artist: String::new(),
            duration_secs: None,
        };

        match resolve(store.as_ref(), analyzer.as_mut(), &id, &meta, path) {
            Some((_, Origin::Analyzed)) => report.analysed += 1,
            Some((_, Origin::Cache)) => report.cached += 1,
            None => report.failed += 1,
        }
        progress(&report, path);
    }
    report
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
    fn scanning_finds_audio_and_ignores_everything_else() {
        let dir = std::env::temp_dir().join(format!("dancer-scan-{}", std::process::id()));
        let nested = dir.join("album");
        std::fs::create_dir_all(&nested).unwrap();
        for name in ["a.mp3", "b.FLAC", "cover.jpg", "notes.txt"] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        std::fs::write(nested.join("c.wav"), b"x").unwrap();

        let mut found = Vec::new();
        find_audio(&dir, &mut found);
        found.sort();

        let names: Vec<String> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert!(names.contains(&"a.mp3".to_string()));
        // Extension matching is case-insensitive: real libraries are a mess.
        assert!(names.contains(&"b.FLAC".to_string()));
        // And it recurses, because albums live in folders.
        assert!(names.contains(&"c.wav".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_is_reported_not_fatal() {
        let mut found = Vec::new();
        find_audio(Path::new("definitely-not-here"), &mut found);
        assert!(found.is_empty());
    }

    #[test]
    fn scanning_skips_tracks_already_in_the_cache() {
        // Resumability: an interrupted scan of a large library must not start over.
        let dir = std::env::temp_dir().join(format!("dancer-scan2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("a.mp3");
        std::fs::write(&track, b"x").unwrap();

        let db = dir.join("scores.db");
        {
            let store = Store::open(&db).unwrap();
            let id = TrackId::new("file", track.to_string_lossy());
            let mut s = score(&id.key());
            s.source = ScoreSource::BeatThis;
            store.put_score(&s).unwrap();
        }

        // No models directory: if it tried to analyse, this would fail instead of
        // reporting a cache hit.
        let report = scan(
            &[dir.clone()],
            Some(&db),
            Path::new("no-models-here"),
            &mut |_, _| {},
        );
        assert_eq!(report, ScanReport { found: 1, analysed: 0, cached: 1, failed: 0 });

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_cache_and_no_analyzer_is_unscored_not_an_error() {
        let id = TrackId::new("file", "a.wav");
        assert!(resolve(None, None, &id, &meta("a"), Path::new("a.wav")).is_none());
    }
}
