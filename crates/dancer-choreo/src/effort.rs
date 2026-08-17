//! Laban's Time Effort — sudden against sustained (spec §11.3).
//!
//! # Why a second axis at all
//!
//! Selection used to run on one number, `energy`. That conflates two genuinely
//! different musics: a sustained loud pad and a stabbing loud drum both rank near
//! the top and pick from the same rows. Laban Movement Analysis separates them —
//! Weight (strong/light) is roughly what `energy` already measures, and **Time**
//! (sudden/sustained) is the one we were missing.
//!
//! Of LMA's four Efforts, Weight and Time are the two recoverable from what the
//! analyzer already stores. Space (direct/indirect) and Flow (bound/free) describe
//! intent rather than signal, and nothing in a beat grid implies them, so they are
//! left out rather than guessed at.
//!
//! # What is actually measured, and what it is not
//!
//! **This is a proxy, and worth being plain about.** True Time Effort would come
//! from onset sharpness — how fast energy arrives at each attack — which needs an
//! envelope at finer resolution than one value per beat. The score stores
//! `beat_energy`, one RMS per beat, and that is all there is.
//!
//! What that *can* measure is how much the level jumps from beat to beat across a
//! bar. Even, driving music holds a similar level on every beat; accented or sparse
//! music does not. So the measurement here is **articulation** — mean absolute
//! beat-to-beat change — read as: high means punctuated and suits sudden moves, low
//! means flowing and suits sustained ones.
//!
//! It is deliberately a *weighting* rather than a filter, unlike
//! [`crate::motif`], because the confidence in it is lower. It nudges selection
//! between rows that are already admissible; it never removes one.
//!
//! Computed here rather than stored in the score, for the same reason as
//! [`crate::energy`]: it is an interpretation of a measurement, so changing it must
//! not invalidate a single cached score.

use crate::energy::{distribution, rank_in};

/// The Time Effort a row's artwork expresses.
///
/// Bipolar, and only the ends are named — Laban names qualities, not degrees, and
/// a sheet author can tell a stab from a sway without being asked for a number.
/// `None` on a row means it suits either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    /// Quick, urgent, arriving all at once. A stamp, a stab, a snap.
    Sudden,
    /// Unhurried, continuous, spread through its own duration. A sway, a drift.
    Sustained,
}

impl Effort {
    pub const fn as_str(self) -> &'static str {
        match self {
            Effort::Sudden => "sudden",
            Effort::Sustained => "sustained",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase();
        Some(match s.as_str() {
            "sudden" | "quick" | "sharp" | "staccato" => Effort::Sudden,
            "sustained" | "slow" | "smooth" | "legato" => Effort::Sustained,
            _ => return None,
        })
    }
}

impl std::str::FromStr for Effort {
    type Err = UnknownEffort;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Effort::parse(s).ok_or_else(|| UnknownEffort(s.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown effort_time `{0}` (expected `sudden` or `sustained`)")]
pub struct UnknownEffort(pub String);

/// How much a matching Time Effort may move a row's odds.
///
/// Weights land in `1 ± SWING`, so a perfect match is three times the odds of a
/// perfect mismatch and neither is ever zero. Kept modest on purpose: the
/// measurement behind it is a proxy (see the module docs), and a proxy should tilt
/// a choice rather than make it.
pub const SWING: f32 = 0.5;

/// Selection weight for a row, given how articulated the music is here.
///
/// `articulation` is a rank in `0.0..1.0` from [`Articulation::rank_at`]. Either
/// side being absent yields a neutral `1.0` — an untagged row and an unmeasured
/// track must both leave the other factors to decide.
pub fn weight(row: Option<Effort>, articulation: Option<f32>) -> f32 {
    match (row, articulation) {
        (Some(Effort::Sudden), Some(a)) => 1.0 - SWING + 2.0 * SWING * a,
        (Some(Effort::Sustained), Some(a)) => 1.0 + SWING - 2.0 * SWING * a,
        _ => 1.0,
    }
}

/// Rank below which the music reads as sustained, and at or above which as sudden.
///
/// Only used for reporting and tests; [`weight`] is continuous and needs no
/// threshold.
pub const SUSTAINED_MAX: f32 = 0.40;
pub const SUDDEN_MIN: f32 = 0.60;

/// How articulated each bar of one track is, and how that compares to the rest of it.
#[derive(Debug, Clone, Default)]
pub struct Articulation {
    /// Raw value for the bar starting at each beat index.
    values: Vec<f32>,
    /// The same values, ascending, for ranking.
    sorted: Vec<f32>,
}

impl Articulation {
    /// Measure every bar of a track.
    ///
    /// A bar is taken as `meter` beats starting at each beat index, rather than only
    /// at fitted downbeats: the scheduler asks about the bar starting at a specific
    /// beat, and computing every offset costs one pass and removes the need to agree
    /// with the bar phase.
    pub fn new(beat_energy: &[f32], meter: u8) -> Self {
        let m = meter.max(1) as usize;
        if beat_energy.len() < 2 {
            // Nothing can change beat to beat with fewer than two beats.
            return Self::default();
        }
        let values: Vec<f32> = (0..beat_energy.len())
            .map(|i| bar_articulation(beat_energy, i, m))
            .collect();
        let sorted = distribution(values.iter().copied());
        Self { values, sorted }
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    /// Where the bar starting at `beat` sits against the rest of the track,
    /// `0.0..1.0`. Low is flowing, high is punctuated.
    ///
    /// `None` when nothing was measured, which is different from measuring an even
    /// track — an even track ranks its bars against each other and still answers.
    pub fn rank_at(&self, beat: usize) -> Option<f32> {
        rank_in(&self.sorted, *self.values.get(beat)?)
    }
}

/// Mean absolute beat-to-beat energy change over the bar starting at `beat`.
fn bar_articulation(e: &[f32], beat: usize, meter: usize) -> f32 {
    let end = (beat + meter).min(e.len().saturating_sub(1));
    if end <= beat {
        // Past the last usable beat: no change can be observed, so report none
        // rather than extrapolating from the bar before.
        return 0.0;
    }
    let sum: f32 = (beat..end).map(|j| (e[j + 1] - e[j]).abs()).sum();
    sum / (end - beat) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_even_bar_reads_as_flowing_and_a_spiky_one_as_punctuated() {
        // Half the track holds a level; half alternates hard. The alternating half
        // is what a stabbing arrangement looks like through one-RMS-per-beat.
        let mut e = vec![0.5f32; 64];
        for (i, v) in e.iter_mut().enumerate().skip(32) {
            *v = if i % 2 == 0 { 0.2 } else { 0.9 };
        }
        let a = Articulation::new(&e, 4);

        let flowing = a.rank_at(8).unwrap();
        let punctuated = a.rank_at(40).unwrap();
        assert!(flowing < SUSTAINED_MAX, "even bar ranked {flowing}");
        assert!(punctuated > SUDDEN_MIN, "spiky bar ranked {punctuated}");
    }

    #[test]
    fn a_sudden_row_is_favoured_where_the_music_is_punctuated() {
        assert!(weight(Some(Effort::Sudden), Some(1.0)) > weight(Some(Effort::Sudden), Some(0.0)));
        assert!(
            weight(Some(Effort::Sustained), Some(0.0))
                > weight(Some(Effort::Sustained), Some(1.0))
        );
    }

    #[test]
    fn the_effort_nudge_never_silences_a_row() {
        // It weights, it does not filter — the measurement behind it is a proxy.
        for a in [0.0, 0.25, 0.5, 0.75, 1.0] {
            for row in [Effort::Sudden, Effort::Sustained] {
                let w = weight(Some(row), Some(a));
                assert!(w > 0.0, "{row:?} at {a} weighted {w}");
                assert!(w <= 1.0 + SWING + 1e-6, "{row:?} at {a} weighted {w}");
            }
        }
    }

    #[test]
    fn an_untagged_row_or_an_unmeasured_track_is_neutral() {
        assert_eq!(weight(None, Some(0.9)), 1.0);
        assert_eq!(weight(Some(Effort::Sudden), None), 1.0);
        assert_eq!(weight(None, None), 1.0);
    }

    #[test]
    fn a_track_too_short_to_change_measures_nothing() {
        assert!(Articulation::new(&[], 4).is_empty());
        assert!(Articulation::new(&[0.5], 4).is_empty());
        assert!(Articulation::new(&[], 4).rank_at(0).is_none());
    }

    #[test]
    fn a_perfectly_even_track_does_not_invent_articulation() {
        // Every bar identical: no bar should be singled out as more punctuated.
        let a = Articulation::new(&[0.7; 40], 4);
        let first = a.rank_at(0).unwrap();
        for beat in 0..30 {
            assert_eq!(a.rank_at(beat).unwrap(), first, "beat {beat}");
        }
    }

    #[test]
    fn laban_terms_and_plain_english_both_parse() {
        assert_eq!(Effort::parse("Sudden"), Some(Effort::Sudden));
        assert_eq!(Effort::parse("staccato"), Some(Effort::Sudden));
        assert_eq!(Effort::parse(" legato "), Some(Effort::Sustained));
        assert_eq!(Effort::parse("bound"), None, "Flow is not Time");
        for e in [Effort::Sudden, Effort::Sustained] {
            assert_eq!(Effort::parse(e.as_str()), Some(e));
        }
    }
}
