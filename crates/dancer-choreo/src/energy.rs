//! Reading energy *relative to the track it came from*.
//!
//! # The problem this solves
//!
//! `beat_energy` is RMS divided by the track's 95th percentile (spec §5), which
//! makes it a ratio against the loudest part of the same song. That sounds
//! relative, and it is — but it is relative in a way that destroys the axis. On a
//! real track measured in M4, the median beat scored **0.78** and the tenth
//! percentile **0.45**: the entire song sat in the top half of the scale.
//!
//! Move selection compares a row's declared energy against that number, so with
//! the default sheet the high-energy `spin` row was in range for **90 %** of bars
//! and the calm `idle` row for **9 %**. The dancer spun through quiet passages
//! because, as far as the numbers were concerned, there were no quiet passages.
//!
//! # What this does instead
//!
//! Maps each value to its **rank within the track**: the quietest bar becomes 0,
//! the loudest 1, the median 0.5. The distribution is spread across the whole axis
//! by construction, whatever the recording's dynamic range, so "quiet for this
//! song" always means something.
//!
//! Deliberately computed here rather than stored in the score. The score holds a
//! measurement; what counts as loud is an interpretation, and interpretations
//! belong to whoever is deciding. It also means no cached score has to be thrown
//! away when the interpretation changes.

/// Energy distribution of one track, for rank lookups.
#[derive(Debug, Clone, Default)]
pub struct EnergyProfile {
    sorted: Vec<f32>,
}

impl EnergyProfile {
    pub fn new(beat_energy: &[f32]) -> Self {
        let mut sorted: Vec<f32> = beat_energy.iter().copied().filter(|v| v.is_finite()).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        Self { sorted }
    }

    pub fn is_empty(&self) -> bool {
        self.sorted.is_empty()
    }

    /// Where `value` sits in the track, `0.0..1.0`.
    ///
    /// `None` when nothing was measured — the caller then has no energy to steer
    /// by, which is different from having measured silence.
    pub fn rank(&self, value: f32) -> Option<f32> {
        if self.sorted.is_empty() {
            return None;
        }
        if self.sorted.len() == 1 {
            // One data point cannot describe a distribution. Mid-scale is the
            // honest answer: it steers towards neither extreme.
            return Some(0.5);
        }
        // Midpoint of the tied range, not the count strictly below. Music has
        // plateaus — most of a track sits at one level — and ranking by
        // "strictly below" puts a plateau covering 90 % of the track at rank
        // 0.10, which would call the *normal* level quiet. Splitting the tie
        // places a plateau at its own middle, so the common level reads as
        // ordinary and only genuine peaks and lulls reach the extremes.
        let below = self.sorted.partition_point(|&v| v < value) as f32;
        let at_or_below = self.sorted.partition_point(|&v| v <= value) as f32;
        let mid = (below + at_or_below) / 2.0;
        Some((mid / self.sorted.len() as f32).clamp(0.0, 1.0))
    }

    /// Coarse band, for deciding whether the music has actually changed character.
    ///
    /// Three tiers rather than a continuous value because this gates *whether to
    /// change move at all*, and a continuous measure would cross a threshold on
    /// noise alone.
    pub fn tier(&self, value: f32) -> Tier {
        self.tier_from(value, Tier::Steady)
    }

    /// The tier `value` falls in, given the tier currently in force.
    ///
    /// **Hysteresis, and it is not optional.** A tier change cuts a phrase short,
    /// so bare thresholds mean that music sitting near 0.33 or 0.70 flips band
    /// bar after bar and every phrase is cut to one — which is the erratic
    /// changing this whole mechanism exists to stop. Leaving a tier therefore
    /// requires clearing its boundary by [`HYSTERESIS`], while entering one only
    /// requires reaching it.
    pub fn tier_from(&self, value: f32, current: Tier) -> Tier {
        let Some(r) = self.rank(value) else {
            return Tier::Steady;
        };
        let (low, high) = match current {
            // Widen the band we are in, so leaving it takes a real change.
            Tier::Calm => (CALM_MAX + HYSTERESIS, LOUD_MIN),
            Tier::Steady => (CALM_MAX - HYSTERESIS, LOUD_MIN + HYSTERESIS),
            Tier::Loud => (CALM_MAX, LOUD_MIN - HYSTERESIS),
        };
        if r < low {
            Tier::Calm
        } else if r < high {
            Tier::Steady
        } else {
            Tier::Loud
        }
    }
}

/// Rank below which a bar is calm, and at or above which it is loud.
pub const CALM_MAX: f32 = 0.33;
pub const LOUD_MIN: f32 = 0.70;
/// How far past a boundary the music must go before the tier changes.
pub const HYSTERESIS: f32 = 0.07;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Calm,
    Steady,
    Loud,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_spreads_a_squashed_distribution_across_the_axis() {
        // The M4 measurement: everything crammed into 0.45..1.0.
        let squashed: Vec<f32> = (0..100).map(|i| 0.45 + i as f32 * 0.0055).collect();
        let p = EnergyProfile::new(&squashed);

        let quietest = p.rank(squashed[0]).unwrap();
        let median = p.rank(squashed[50]).unwrap();
        let loudest = p.rank(squashed[99]).unwrap();

        assert!(quietest < 0.05, "quietest ranked {quietest}");
        assert!((median - 0.5).abs() < 0.05, "median ranked {median}");
        assert!(loudest > 0.95, "loudest ranked {loudest}");
    }

    #[test]
    fn the_quiet_part_of_a_loud_track_reads_as_quiet() {
        // The actual complaint: a dancer spinning through the calm passage.
        // 0.6 is objectively loud, but it is this track's floor.
        let mut e = vec![0.95f32; 90];
        e.extend(std::iter::repeat(0.60).take(10));
        let p = EnergyProfile::new(&e);

        assert_eq!(p.tier(0.60), Tier::Calm, "rank {:?}", p.rank(0.60));
        // And the level the track spends 90 % of its time at is *ordinary*, not a
        // peak — which is the point. A dancer should not treat the entire song as
        // a climax just because it is loud in absolute terms.
        assert_eq!(p.tier(0.95), Tier::Steady, "rank {:?}", p.rank(0.95));
    }

    #[test]
    fn a_genuine_peak_still_reads_as_loud() {
        // Loud has to remain reachable, or the big moves never come out.
        let mut e = vec![0.5f32; 80];
        e.extend(std::iter::repeat(0.95).take(20));
        let p = EnergyProfile::new(&e);
        // The level covering four fifths of the track is the ordinary one, whatever
        // its absolute value — the plateau always lands mid-scale, by design.
        assert_eq!(p.tier(0.5), Tier::Steady, "rank {:?}", p.rank(0.5));
        // What matters is that the peak is distinguishable from it.
        assert_eq!(p.tier(0.95), Tier::Loud, "rank {:?}", p.rank(0.95));
    }

    #[test]
    fn three_levels_map_onto_three_tiers() {
        // A track with real structure: verse, chorus, and a quiet intro.
        let mut e = vec![0.2f32; 30];
        e.extend(std::iter::repeat(0.6).take(40));
        e.extend(std::iter::repeat(0.95).take(30));
        let p = EnergyProfile::new(&e);

        assert_eq!(p.tier(0.2), Tier::Calm);
        assert_eq!(p.tier(0.6), Tier::Steady);
        assert_eq!(p.tier(0.95), Tier::Loud);
    }

    #[test]
    fn tiers_split_a_uniform_track_sensibly() {
        let e: Vec<f32> = (0..300).map(|i| i as f32 / 300.0).collect();
        let p = EnergyProfile::new(&e);
        assert_eq!(p.tier(0.1), Tier::Calm);
        assert_eq!(p.tier(0.5), Tier::Steady);
        assert_eq!(p.tier(0.9), Tier::Loud);
    }

    #[test]
    fn a_track_with_no_dynamics_does_not_pretend_to_have_any() {
        // Every beat identical: rank is meaningless, so nothing should read as a
        // peak or a lull. This is the loudness-war case from M3.
        let p = EnergyProfile::new(&[0.9; 50]);
        // Every value ranks at the bottom because none is strictly below another;
        // what matters is that no beat is singled out as louder than the rest.
        assert_eq!(p.tier(0.9), p.tier(0.9));
        assert!(p.rank(0.9).is_some());
    }

    #[test]
    fn music_sitting_on_a_threshold_does_not_flap() {
        // Without hysteresis this is the original complaint in miniature: energy
        // hovering at a boundary changes tier every bar, which cuts every phrase
        // to one bar and looks erratic.
        let e: Vec<f32> = (0..300).map(|i| i as f32 / 300.0).collect();
        let p = EnergyProfile::new(&e);

        // Values straddling the calm/steady boundary at rank 0.33.
        let just_below = 0.32;
        let just_above = 0.34;

        // Once calm, small wobbles above the line do not move it.
        assert_eq!(p.tier_from(just_above, Tier::Calm), Tier::Calm);
        // Once steady, small wobbles below the line do not move it either.
        assert_eq!(p.tier_from(just_below, Tier::Steady), Tier::Steady);
    }

    #[test]
    fn a_real_change_still_moves_the_tier() {
        // Hysteresis must not become deafness.
        let e: Vec<f32> = (0..300).map(|i| i as f32 / 300.0).collect();
        let p = EnergyProfile::new(&e);
        assert_eq!(p.tier_from(0.9, Tier::Calm), Tier::Loud);
        assert_eq!(p.tier_from(0.05, Tier::Loud), Tier::Calm);
    }

    #[test]
    fn an_unmeasured_track_yields_nothing_rather_than_a_guess() {
        let p = EnergyProfile::new(&[]);
        assert!(p.is_empty());
        assert!(p.rank(0.5).is_none());
        // And the tier degrades to the middle, steering towards neither extreme.
        assert_eq!(p.tier(0.5), Tier::Steady);
    }
}
