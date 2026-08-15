//! Reading `(title, artist)` off an audio file (spec §5.1, §8.3).
//!
//! # Why this exists at all
//!
//! The library index is keyed on a hash of `(title, artist)`, because that is all
//! SMTC ever reports — never a path (§6.2). So a file analysed on disk and the same
//! file played through the user's own player must produce the *same* strings, or
//! the join never fires and every track is `Unscored`.
//!
//! M2 skipped this and used the filename stem as the title with an empty artist.
//! M4 found what that costs: SMTC reports the file's own tags, so `Rhythm Is A
//! Dancer` / `SNAP!` never matched `01 - rhythm is a dancer` / `""`, and the whole
//! owned-music path was dead on arrival. Spec §5.1 had it right — "SMTC reports
//! *that file's own tags*, so the strings match exactly because they came from the
//! same place" — but only if we read the same tags rather than inventing them.
//!
//! Uses symphonia, already present as beat-this's decoder, so this costs a direct
//! dependency on something already compiled rather than a new one.

use std::fs::File;
use std::path::Path;

use symphonia::core::formats::FormatOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTag};

/// What a file says it is.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
}

impl Tags {
    /// Title, falling back to the filename stem.
    ///
    /// The fallback matches what players do with untagged files — they show the
    /// filename — so the two sides still agree in that case.
    pub fn title_or_stem(&self, path: &Path) -> String {
        self.title.clone().unwrap_or_else(|| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }

    pub fn artist_or_empty(&self) -> String {
        self.artist.clone().unwrap_or_default()
    }
}

/// Read tags, or return empty ones.
///
/// Never an error: a file with no tags, or one symphonia cannot parse, is a
/// perfectly danceable file. It just falls back to the filename.
pub fn read(path: &Path) -> Tags {
    match try_read(path) {
        Ok(t) => {
            tracing::debug!(path = %path.display(), title = ?t.title, artist = ?t.artist, "tags");
            t
        }
        Err(e) => {
            tracing::debug!(path = %path.display(), error = %e, "no readable tags");
            Tags::default()
        }
    }
}

fn try_read(path: &Path) -> Result<Tags, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| e.to_string())?;

    let mut tags = Tags::default();

    // Two places to look. Formats that carry metadata ahead of the stream (ID3v2
    // on MP3) queue it during probing; formats that carry it inline (Vorbis
    // comments) expose it on the reader. Checking only one silently misses half
    // of a music library.
    let mut collect = |t: &symphonia::core::meta::Tag| match &t.std {
        Some(StandardTag::TrackTitle(v)) if tags.title.is_none() => {
            tags.title = non_empty(v);
        }
        Some(StandardTag::Artist(v)) if tags.artist.is_none() => {
            tags.artist = non_empty(v);
        }
        _ => {}
    };

    // Metadata read while probing (ID3v2 ahead of an MP3 stream) is queued and
    // attached to the reader, so one pass over the current revision covers both
    // that and inline metadata such as Vorbis comments.
    if let Some(rev) = format.metadata().current() {
        rev.media.tags.iter().for_each(&mut collect);
    }

    Ok(tags)
}

fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreadable_file_yields_empty_tags_rather_than_failing() {
        // Analysis must not die because a file has no metadata.
        let t = read(Path::new("definitely-not-here.mp3"));
        assert_eq!(t, Tags::default());
    }

    #[test]
    fn the_filename_stem_stands_in_for_a_missing_title() {
        // Matches what players show for untagged files, so both sides still agree.
        let t = Tags::default();
        assert_eq!(t.title_or_stem(Path::new("D:/music/Song 2.mp3")), "Song 2");
        assert_eq!(t.artist_or_empty(), "");
    }

    #[test]
    fn a_real_title_wins_over_the_filename() {
        let t = Tags {
            title: Some("Rhythm Is A Dancer".into()),
            artist: Some("SNAP!".into()),
        };
        assert_eq!(t.title_or_stem(Path::new("01 - track.mp3")), "Rhythm Is A Dancer");
        assert_eq!(t.artist_or_empty(), "SNAP!");
    }

    #[test]
    fn blank_tags_are_treated_as_absent() {
        // An empty ID3 frame must not hash as a distinct title.
        assert_eq!(non_empty("   "), None);
        assert_eq!(non_empty(" x "), Some("x".into()));
    }
}
