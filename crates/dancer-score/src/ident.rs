//! Track identity (spec §5.1, §6.2).
//!
//! Two keys, deliberately different:
//!
//! - [`TrackId`] is namespaced per source — `spotify:4uLU…`, `file:…`. The same
//!   song from two sources gets two entries **on purpose**: masters differ, and a
//!   grid off by 40 ms looks broken.
//! - [`TrackMeta::library_key`] hashes `(title, artist)` because SMTC reports those
//!   and never a path. It is what connects "the user pressed play in foobar2000" to
//!   "we analysed that file last week".

use std::fmt;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// Source-namespaced track identifier. Serialises as `"source:id"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TrackId {
    pub source: String,
    pub id: String,
}

impl TrackId {
    pub fn new(source: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            id: id.into(),
        }
    }

    /// The `scores` table key (spec §5.1).
    pub fn key(&self) -> String {
        format!("{}:{}", self.source, self.id)
    }
}

impl fmt::Display for TrackId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.source, self.id)
    }
}

impl Serialize for TrackId {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.key())
    }
}

impl<'de> Deserialize<'de> for TrackId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        // Split once: Spotify URIs and file paths both contain further colons.
        match s.split_once(':') {
            Some((source, id)) => Ok(TrackId::new(source, id)),
            None => Ok(TrackId::new("unknown", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackMeta {
    pub id: TrackId,
    pub title: String,
    pub artist: String,
    /// As reported by the source. `None` when it publishes no timeline.
    pub duration_secs: Option<f64>,
}

impl TrackMeta {
    /// The `library` table key: a hash of the **raw** strings (spec §5.1).
    ///
    /// Only encoding-level normalisation is applied — trim and ASCII casefold.
    /// Content-level cleanup is forbidden: stripping `(Radio Edit)` or
    /// `- Remastered` merges *different masters with different grids*, which is a
    /// false positive that looks like a bug. Hashing raw at worst misses, and a
    /// miss costs one re-analysis.
    ///
    /// Proper Unicode casefold and NFC belong here once M2 adds the dependency;
    /// ASCII-only for now, which under-normalises rather than over-normalises, and
    /// so fails in the safe direction.
    pub fn library_key(&self) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        normalise(&self.title).hash(&mut h);
        normalise(&self.artist).hash(&mut h);
        h.finish()
    }
}

fn normalise(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Do a stored duration and a reported one describe the same recording?
///
/// Spec §5.1: a hash hit with a disagreeing duration is treated as a miss. Catches
/// a player reporting an album title while playing a radio edit.
pub const DURATION_TOLERANCE_SECS: f64 = 2.0;

pub fn duration_agrees(stored: f64, reported: Option<f64>) -> bool {
    match reported {
        Some(r) => (stored - r).abs() <= DURATION_TOLERANCE_SECS,
        // Nothing to disagree with. A source with no timeline is already handled
        // by dropping to Unscored (spec §6.2); do not also reject the match.
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(title: &str, artist: &str) -> TrackMeta {
        TrackMeta {
            id: TrackId::new("file", "x"),
            title: title.into(),
            artist: artist.into(),
            duration_secs: Some(120.0),
        }
    }

    #[test]
    fn track_id_round_trips_through_colons() {
        let id = TrackId::new("spotify", "4uLU6hMCjMI75M1A2tKUQC");
        assert_eq!(id.key(), "spotify:4uLU6hMCjMI75M1A2tKUQC");
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(serde_json::from_str::<TrackId>(&json).unwrap(), id);

        // A Windows path keeps its drive colon in the id half.
        let id = TrackId::new("file", r"D:\music\a.wav");
        let back: TrackId = serde_json::from_str(&serde_json::to_string(&id).unwrap()).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn library_key_ignores_case_and_padding_only() {
        assert_eq!(
            meta("Song 2", "Blur").library_key(),
            meta("  song 2 ", "blur").library_key()
        );
    }

    #[test]
    fn library_key_keeps_different_masters_apart() {
        // Spec §5.1: these are different recordings with different grids, and
        // merging them applies the wrong timeline. The miss is the correct failure.
        let album = meta("Song 2", "Blur").library_key();
        for variant in [
            "Song 2 (Radio Edit)",
            "Song 2 (Official Music Video)",
            "Song 2 - Remastered",
            "Blur - Song 2",
        ] {
            assert_ne!(album, meta(variant, "Blur").library_key(), "{variant}");
        }
    }

    #[test]
    fn duration_check_gates_hash_hits() {
        assert!(duration_agrees(214.0, Some(215.0)));
        assert!(!duration_agrees(214.0, Some(190.0)));
        // No reported duration is not a disagreement.
        assert!(duration_agrees(214.0, None));
    }
}
