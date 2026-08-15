//! The score cache: one SQLite file, two tables (spec §5.1).
//!
//! **One file, `scores.db`, beside the executable.** Not a JSON file per track: a
//! user with a few hundred analysed tracks should not get a few hundred files
//! scattered under a directory they will never find.
//!
//! ```text
//! scores   {source}:{track_id}          -> the score, as JSON
//! library  hash(title, artist) -> path  -> which file that was, and its score
//! ```
//!
//! `library` exists because SMTC reports `(title, artist)` and never a path
//! (§6.2). It is what connects "the user pressed play in foobar2000" to "we
//! analysed that file last week" — the mechanism the whole owned-music path rests
//! on (§8.3).

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

use crate::{ident, Score, TrackId, TrackMeta};

/// Bumped only by a migration in [`Store::open`]. Set from the very first release
/// so that migrating is possible at all — retrofitting a version onto files
/// already in the wild is far harder than reading one that was always there.
pub const USER_VERSION: i32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("opening {path}: {source}")]
    Open {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("database at {path} is version {found}, newer than this build ({USER_VERSION})")]
    TooNew { path: PathBuf, found: i32 },
    #[error(transparent)]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A library entry: which file a `(title, artist)` pair resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct LibraryEntry {
    pub path: PathBuf,
    pub duration_secs: f64,
    pub score_key: String,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(|source| StoreError::Open {
            path: path.to_owned(),
            source,
        })?;
        Self::init(conn, path.to_owned())
    }

    /// In-memory store, for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, PathBuf::from(":memory:"))
    }

    fn init(conn: Connection, path: PathBuf) -> Result<Self, StoreError> {
        let found: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;

        // Refuse to touch a file a later build wrote. Silently reading it would
        // mean interpreting fields whose meaning has changed.
        if found > USER_VERSION {
            return Err(StoreError::TooNew { path, found });
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS scores (
                 key         TEXT PRIMARY KEY,
                 json        TEXT NOT NULL,
                 analyzed_at TEXT NOT NULL DEFAULT ''
             );
             CREATE TABLE IF NOT EXISTS library (
                 key           INTEGER PRIMARY KEY,
                 path          TEXT NOT NULL,
                 duration_secs REAL NOT NULL,
                 score_key     TEXT NOT NULL
             );",
        )?;

        if found < USER_VERSION {
            // No migration to run yet — version 1 is the first shape. When there
            // is one, it goes here, stepping `found` up one version at a time.
            conn.pragma_update(None, "user_version", USER_VERSION)?;
            tracing::info!(path = %path.display(), from = found, to = USER_VERSION, "cache initialised");
        }

        Ok(Self { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn put_score(&self, score: &Score) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO scores (key, json, analyzed_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET json = ?2, analyzed_at = ?3",
            params![&score.track_id, serde_json::to_string(score)?, &score.analyzed_at],
        )?;
        tracing::debug!(key = %score.track_id, "score cached");
        Ok(())
    }

    pub fn get_score(&self, key: &str) -> Result<Option<Score>, StoreError> {
        let json: Option<String> = self
            .conn
            .query_row("SELECT json FROM scores WHERE key = ?1", [key], |r| r.get(0))
            .optional()?;
        let Some(json) = json else { return Ok(None) };

        match serde_json::from_str::<Score>(&json).map_err(StoreError::from) {
            Ok(score) => match score.validate() {
                Ok(()) => Ok(Some(score)),
                Err(e) => {
                    // A cached score that no longer validates is a miss, not a
                    // crash: re-analysis is cheap and a wrong grid is not.
                    tracing::warn!(key, error = %e, "cached score is invalid; ignoring it");
                    Ok(None)
                }
            },
            Err(e) => {
                tracing::warn!(key, error = %e, "cached score will not parse; ignoring it");
                Ok(None)
            }
        }
    }

    /// Record that `meta` was found at `path` with the given score.
    pub fn put_library(
        &self,
        meta: &TrackMeta,
        path: &Path,
        duration_secs: f64,
        score_key: &str,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO library (key, path, duration_secs, score_key) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET path = ?2, duration_secs = ?3, score_key = ?4",
            params![
                meta.library_key() as i64,
                path.to_string_lossy(),
                duration_secs,
                score_key
            ],
        )?;
        Ok(())
    }

    /// Look a `(title, artist)` pair up in the library.
    ///
    /// **Verifies duration on match** (spec §5.1): a hash hit whose duration
    /// disagrees by more than [`ident::DURATION_TOLERANCE_SECS`] is treated as a
    /// miss. This costs nothing and catches a player reporting an album title
    /// while playing a radio edit.
    pub fn find_in_library(&self, meta: &TrackMeta) -> Result<Option<LibraryEntry>, StoreError> {
        let row: Option<(String, f64, String)> = self
            .conn
            .query_row(
                "SELECT path, duration_secs, score_key FROM library WHERE key = ?1",
                [meta.library_key() as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;

        let Some((path, duration_secs, score_key)) = row else {
            return Ok(None);
        };

        if !ident::duration_agrees(duration_secs, meta.duration_secs) {
            tracing::info!(
                title = %meta.title,
                stored = duration_secs,
                reported = ?meta.duration_secs,
                "library hit rejected on duration; treating as a miss"
            );
            return Ok(None);
        }

        Ok(Some(LibraryEntry {
            path: PathBuf::from(path),
            duration_secs,
            score_key,
        }))
    }

    /// Score for a track played through some other application, if we have one.
    pub fn lookup(&self, meta: &TrackMeta) -> Result<Option<Score>, StoreError> {
        let Some(entry) = self.find_in_library(meta)? else {
            return Ok(None);
        };
        self.get_score(&entry.score_key)
    }

    /// Store a freshly analysed score and index the file it came from.
    pub fn record_analysis(
        &self,
        id: &TrackId,
        meta: &TrackMeta,
        path: &Path,
        score: &Score,
    ) -> Result<(), StoreError> {
        self.put_score(score)?;
        self.put_library(meta, path, score.duration_secs(), &id.key())?;
        Ok(())
    }

    pub fn score_count(&self) -> Result<i64, StoreError> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM scores", [], |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ScoreSource, SCHEMA};

    fn score(id: &str, duration_ms: u64) -> Score {
        Score {
            schema: SCHEMA,
            track_id: id.into(),
            duration_ms,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::BeatThis,
            confidence: 0.8,
            analyzed_at: "2026-08-15T00:00:00Z".into(),
            beats: vec![0.0, 0.5, 1.0, 1.5],
            beat_positions: vec![1, 2, 3, 4],
            downbeats: vec![0.0],
            segments: vec![],
            cues: vec![],
            beat_energy: vec![0.2, 0.3, 0.4, 0.5],
        }
    }

    fn meta(title: &str, duration: Option<f64>) -> TrackMeta {
        TrackMeta {
            id: TrackId::new("file", "a.wav"),
            title: title.into(),
            artist: "Artist".into(),
            duration_secs: duration,
        }
    }

    #[test]
    fn round_trips_a_score() {
        let s = Store::open_in_memory().unwrap();
        let score = score("file:a.wav", 214_000);
        s.put_score(&score).unwrap();

        let back = s.get_score("file:a.wav").unwrap().unwrap();
        assert_eq!(back.beats, score.beats);
        assert_eq!(back.beat_energy, score.beat_energy);
        assert_eq!(back.source, ScoreSource::BeatThis);
        assert!(s.get_score("file:missing.wav").unwrap().is_none());
    }

    #[test]
    fn writing_the_same_key_twice_updates_rather_than_failing() {
        let s = Store::open_in_memory().unwrap();
        s.put_score(&score("file:a.wav", 1000)).unwrap();
        let mut second = score("file:a.wav", 2000);
        second.bpm = 90.0;
        s.put_score(&second).unwrap();

        assert_eq!(s.score_count().unwrap(), 1);
        assert_eq!(s.get_score("file:a.wav").unwrap().unwrap().bpm, 90.0);
    }

    #[test]
    fn library_connects_a_played_track_to_an_analysed_file() {
        // The §8.3 mechanism end to end: analyse a file, then find it again from
        // nothing but the (title, artist) a player reported.
        let s = Store::open_in_memory().unwrap();
        let id = TrackId::new("file", "a.wav");
        let score = score(&id.key(), 214_000);
        let m = meta("Song 2", Some(214.0));

        s.record_analysis(&id, &m, Path::new("D:/music/a.wav"), &score).unwrap();

        let found = s.lookup(&meta("song 2", Some(214.0))).unwrap().unwrap();
        assert_eq!(found.track_id, id.key());

        let entry = s.find_in_library(&m).unwrap().unwrap();
        assert_eq!(entry.path, PathBuf::from("D:/music/a.wav"));
    }

    #[test]
    fn duration_disagreement_is_a_miss() {
        // Spec §5.1: catches a player reporting an album title over a radio edit.
        let s = Store::open_in_memory().unwrap();
        let id = TrackId::new("file", "a.wav");
        s.record_analysis(
            &id,
            &meta("Song 2", Some(214.0)),
            Path::new("a.wav"),
            &score(&id.key(), 214_000),
        )
        .unwrap();

        assert!(s.lookup(&meta("Song 2", Some(190.0))).unwrap().is_none());
        assert!(s.lookup(&meta("Song 2", Some(215.0))).unwrap().is_some());
        // A source with no timeline has nothing to disagree with.
        assert!(s.lookup(&meta("Song 2", None)).unwrap().is_some());
    }

    #[test]
    fn different_masters_do_not_collide() {
        let s = Store::open_in_memory().unwrap();
        for (title, key) in [("Song 2", "file:album"), ("Song 2 (Radio Edit)", "file:radio")] {
            let id = TrackId::new("file", key.trim_start_matches("file:"));
            s.record_analysis(
                &id,
                &meta(title, Some(214.0)),
                Path::new(key),
                &score(&id.key(), 214_000),
            )
            .unwrap();
        }
        assert_eq!(s.score_count().unwrap(), 2);
        assert_eq!(
            s.lookup(&meta("Song 2", Some(214.0))).unwrap().unwrap().track_id,
            "file:album"
        );
    }

    #[test]
    fn corrupt_cached_score_is_a_miss_not_a_crash() {
        let s = Store::open_in_memory().unwrap();
        s.conn
            .execute(
                "INSERT INTO scores (key, json, analyzed_at) VALUES ('file:bad', '{oops', '')",
                [],
            )
            .unwrap();
        assert!(s.get_score("file:bad").unwrap().is_none());

        // Valid JSON but an invalid grid — same treatment.
        let mut bad = score("file:worse", 1000);
        bad.beats = vec![1.0, 0.5];
        s.conn
            .execute(
                "INSERT INTO scores (key, json, analyzed_at) VALUES ('file:worse', ?1, '')",
                [serde_json::to_string(&bad).unwrap()],
            )
            .unwrap();
        assert!(s.get_score("file:worse").unwrap().is_none());
    }

    #[test]
    fn user_version_is_set_and_a_newer_file_is_refused() {
        let s = Store::open_in_memory().unwrap();
        let v: i32 = s.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, USER_VERSION);

        // Simulate a file written by a later build.
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", USER_VERSION + 1).unwrap();
        assert!(matches!(
            Store::init(conn, PathBuf::from("x.db")),
            Err(StoreError::TooNew { .. })
        ));
    }

    #[test]
    fn reopening_an_existing_file_keeps_its_contents() {
        let dir = std::env::temp_dir().join(format!("dancer-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scores.db");
        let _ = std::fs::remove_file(&path);

        {
            let s = Store::open(&path).unwrap();
            s.put_score(&score("file:a.wav", 1000)).unwrap();
        }
        {
            let s = Store::open(&path).unwrap();
            assert_eq!(s.score_count().unwrap(), 1);
        }
        let _ = std::fs::remove_file(&path);
    }
}
