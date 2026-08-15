//! Deciding whether a search result is really the track that is playing.
//!
//! SMTC gives strings, so this is a fuzzy match and it can be wrong. Being wrong
//! is expensive twice over: the dancer gets a confidently incorrect grid, which
//! spec §8.3 rates as worse than no grid at all, *and* a stranger's song was
//! fetched to produce it.
//!
//! So the bar is deliberately high, and the failure is deliberately a miss. A
//! rejected match costs `Unscored` — which is where a streamed track sits anyway,
//! so the downside of refusing is precisely nothing.

use dancer_score::TrackMeta;
use yamuse::models::track::Track;

/// Below this, treat it as no match at all.
pub const MIN_SCORE: f32 = 0.75;

/// Duration agreement, in seconds, for a full mark. Same tolerance the library
/// index uses (spec §5.1).
pub const DURATION_TOLERANCE: f64 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub duration_secs: Option<f64>,
}

impl Candidate {
    pub fn from_track(t: &Track) -> Option<Self> {
        Some(Self {
            id: t.id.as_ref()?.to_string(),
            title: t.title.clone().unwrap_or_default(),
            artist: t
                .artists
                .first()
                .and_then(|a| a.name.clone())
                .unwrap_or_default(),
            duration_secs: t.duration_ms.map(|ms| ms as f64 / 1000.0),
        })
    }
}

/// The best candidate, or `None` if none is convincing.
pub fn best(tracks: &[Track], meta: &TrackMeta) -> Option<Track> {
    let mut scored: Vec<(f32, usize)> = tracks
        .iter()
        .enumerate()
        .filter_map(|(i, t)| {
            let c = Candidate::from_track(t)?;
            Some((score_candidate(&c, meta), i))
        })
        .collect();

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let (score, idx) = *scored.first()?;

    if score < MIN_SCORE {
        tracing::info!(
            title = %meta.title,
            artist = %meta.artist,
            best = score,
            "no convincing match; staying Unscored rather than guessing"
        );
        return None;
    }
    tracing::debug!(score, "match accepted");
    Some(tracks[idx].clone())
}

/// How well a candidate matches, `0.0..1.0`.
///
/// Duration is weighted heavily and deliberately. Titles and artists are the part
/// that varies between sources — the very reason spec §5.1 refuses to canonicalise
/// them — whereas a duration is a fact about the recording. A candidate whose
/// length disagrees is a different master even when the strings match perfectly,
/// and applying its grid is exactly the confident-and-wrong failure to avoid.
pub fn score_candidate(c: &Candidate, meta: &TrackMeta) -> f32 {
    let title = similarity(&c.title, &meta.title);
    let artist = if meta.artist.is_empty() {
        // Nothing to compare against; do not reward or punish for it.
        title
    } else {
        similarity(&c.artist, &meta.artist)
    };

    let duration = match (c.duration_secs, meta.duration_secs) {
        (Some(a), Some(b)) => {
            let diff = (a - b).abs();
            if diff <= DURATION_TOLERANCE {
                1.0
            } else {
                // Fades to nothing over ten seconds. A radio edit against an album
                // version is usually much further apart than that.
                (1.0 - ((diff - DURATION_TOLERANCE) / 10.0) as f32).max(0.0)
            }
        }
        // No duration to compare: **required evidence, not a neutral term.** A
        // perfect title and artist scores 0.65 without it, which is below the bar,
        // so an unconfirmable match cannot trigger a fetch. Album version against
        // radio edit is exactly the case where the strings agree and the recording
        // does not, and here being wrong means having downloaded a stranger's
        // track to build a grid that would have been wrong anyway.
        _ => 0.0,
    };

    0.4 * title + 0.25 * artist + 0.35 * duration
}

/// Token-overlap similarity, `0.0..1.0`.
///
/// Not an edit distance: the differences that matter here are whole words —
/// `(Radio Edit)`, `(Official Music Video)`, a swapped `Artist - Title` — and
/// overlap handles those the way a person would, where character distance does not.
///
/// Note this *is* content-level normalisation, which spec §5.1 forbids for cache
/// keys. The rule is not violated: keys are still hashed raw. This is a search
/// ranking, where being approximately right is the job, and the duration check is
/// what stops an approximate title becoming a wrong grid.
fn similarity(a: &str, b: &str) -> f32 {
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let shared = ta.iter().filter(|t| tb.contains(*t)).count();
    // Against the shorter side, so a long "(Official Music Video)" suffix on one
    // side does not penalise an otherwise exact match.
    shared as f32 / ta.len().min(tb.len()) as f32
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dancer_score::TrackId;

    fn meta(title: &str, artist: &str, dur: Option<f64>) -> TrackMeta {
        TrackMeta {
            id: TrackId::new("smtc", "x"),
            title: title.into(),
            artist: artist.into(),
            duration_secs: dur,
        }
    }

    fn cand(title: &str, artist: &str, dur: Option<f64>) -> Candidate {
        Candidate {
            id: "1".into(),
            title: title.into(),
            artist: artist.into(),
            duration_secs: dur,
        }
    }

    #[test]
    fn an_exact_match_scores_full_marks() {
        let s = score_candidate(
            &cand("Rhythm Is A Dancer", "SNAP!", Some(220.0)),
            &meta("Rhythm Is A Dancer", "SNAP!", Some(220.0)),
        );
        assert!(s > 0.99, "{s}");
    }

    #[test]
    fn browser_style_suffixes_still_match() {
        // Phase 0.5 measured Edge reporting exactly this shape.
        let s = score_candidate(
            &cand("Song 2", "Blur", Some(122.0)),
            &meta("Blur - Song 2 (Official Music Video)", "Blur", Some(122.5)),
        );
        assert!(s >= MIN_SCORE, "scored {s}, would be rejected");
    }

    #[test]
    fn a_different_master_is_rejected_on_duration() {
        // The important one. Titles and artists agree perfectly; the recording is
        // a different length, so its grid would be confidently wrong.
        let s = score_candidate(
            &cand("Song 2", "Blur", Some(122.0)),
            &meta("Song 2", "Blur", Some(180.0)),
        );
        assert!(s < MIN_SCORE, "a 58 s difference must not match: {s}");
    }

    #[test]
    fn a_different_song_is_rejected() {
        let s = score_candidate(
            &cand("Parklife", "Blur", Some(220.0)),
            &meta("Rhythm Is A Dancer", "SNAP!", Some(220.0)),
        );
        assert!(s < MIN_SCORE, "{s}");
    }

    #[test]
    fn a_missing_duration_alone_is_not_enough() {
        // Without a duration to confirm on, a title match cannot reach the bar —
        // this is the case where guessing costs a stranger's song being fetched.
        let s = score_candidate(
            &cand("Song 2", "Blur", None),
            &meta("Song 2", "Blur", None),
        );
        assert!(s < MIN_SCORE, "unconfirmable match scored {s}");
    }

    #[test]
    fn a_missing_artist_does_not_punish_a_good_title() {
        // Some sessions publish no artist at all.
        let s = score_candidate(
            &cand("Rhythm Is A Dancer", "SNAP!", Some(220.0)),
            &meta("Rhythm Is A Dancer", "", Some(220.0)),
        );
        assert!(s >= MIN_SCORE, "{s}");
    }

    #[test]
    fn swapped_artist_and_title_still_match() {
        let s = score_candidate(
            &cand("Song 2", "Blur", Some(122.0)),
            &meta("Blur - Song 2", "Blur", Some(122.0)),
        );
        assert!(s >= MIN_SCORE, "{s}");
    }

    #[test]
    fn empty_strings_score_nothing_rather_than_everything() {
        assert_eq!(similarity("", "anything"), 0.0);
        assert_eq!(similarity("anything", ""), 0.0);
    }
}
