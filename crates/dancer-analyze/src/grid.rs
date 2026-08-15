//! Turning raw tracker output into a score (spec §5, §8.1).
//!
//! Everything here is pure arithmetic over beat times, so it is testable without
//! the ONNX models — which matters, because these are the judgement calls that
//! decide whether the dancer looks right, and they should not need a 10 MB
//! download to exercise.

/// Plausible bar lengths. Outside this the inference is wrong, not exotic.
const MIN_METER: usize = 2;
const MAX_METER: usize = 12;

/// What the bar-grid fit concluded.
#[derive(Debug, Clone, PartialEq)]
pub struct BarGrid {
    /// Modal bar length in beats. **Never assumed to be 4** (spec §5).
    pub meter: u8,
    /// Beat index the bar grid is phased from.
    pub phase: usize,
    /// Fraction of downbeat candidates the fitted grid agrees with, `0.0..1.0`.
    pub agreement: f32,
    /// Fraction of observed bars that were the modal length.
    pub consistency: f32,
}

/// Infer meter from the tracker's per-beat bar numbering.
///
/// Returns the modal bar length and how consistent the bars were. Phase 0.1 found
/// 3/4 correctly detected on a waltz at 97 % consistency, and 4/4 at only 51 % on a
/// track whose beat grid was rock steady — so consistency is a signal about the
/// *downbeats*, not about the beats, and the two must not be conflated.
pub fn infer_meter(counts: &[u8]) -> (u8, f32) {
    let mut bars = Vec::new();
    let mut cur = 0usize;
    for &c in counts {
        if c == 1 && cur > 0 {
            bars.push(cur);
            cur = 0;
        }
        cur += 1;
    }

    if bars.is_empty() {
        // No bar structure at all. Four is the honest fallback — it is the most
        // common meter — but consistency 0 tells the confidence heuristic that
        // nothing supports it.
        return (4, 0.0);
    }

    let mut hist = std::collections::BTreeMap::new();
    for &b in &bars {
        *hist.entry(b).or_insert(0usize) += 1;
    }
    let (modal, n) = hist
        .iter()
        .filter(|(len, _)| (MIN_METER..=MAX_METER).contains(len))
        .max_by_key(|(_, n)| **n)
        .map(|(len, n)| (*len, *n))
        .unwrap_or((4, 0));

    (modal as u8, n as f32 / bars.len() as f32)
}

/// Fit a regular bar phase to downbeat *candidates*, rejecting outliers.
///
/// This is the fix for the Phase 0.1 finding: a track with a rock-steady beat grid
/// whose downbeats split 29 two-beat bars against 30 four-beat ones. Trusting each
/// candidate would have halved half its bars. Instead, every phase `0..meter` is
/// scored by how many candidates it explains, and the best one wins — a spurious
/// downbeat then simply fails to vote for the winner rather than corrupting it.
///
/// `downbeat_indices` are beat indices, so callers must snap downbeat *times* to
/// beats first.
pub fn fit_bar_phase(beat_count: usize, meter: u8, downbeat_indices: &[usize]) -> BarGrid {
    let m = (meter as usize).max(1);
    if downbeat_indices.is_empty() || beat_count == 0 {
        return BarGrid {
            meter: meter.max(1),
            phase: 0,
            agreement: 0.0,
            consistency: 0.0,
        };
    }

    let mut best = (0usize, 0usize); // (phase, votes)
    for p in 0..m {
        let votes = downbeat_indices
            .iter()
            .filter(|&&i| i >= p && (i - p) % m == 0)
            .count();
        if votes > best.1 {
            best = (p, votes);
        }
    }

    BarGrid {
        meter: meter.max(1),
        phase: best.0,
        agreement: best.1 as f32 / downbeat_indices.len() as f32,
        consistency: 0.0,
    }
}

/// Beat numbers `1..=meter` generated from a fitted grid.
pub fn beat_positions(beat_count: usize, grid: &BarGrid) -> Vec<u8> {
    let m = (grid.meter as usize).max(1);
    (0..beat_count)
        .map(|i| ((i as i64 - grid.phase as i64).rem_euclid(m as i64) + 1) as u8)
        .collect()
}

/// Snap downbeat times onto beat indices, dropping any that match nothing.
///
/// Phase 0.1 verified downbeats are a subset of beats, but "verified on four
/// tracks" is not "guaranteed", and a downbeat that matches no beat would silently
/// shift the phase fit.
pub fn snap_downbeats(beats: &[f64], downbeats: &[f64], tolerance: f64) -> Vec<usize> {
    let mut out = Vec::with_capacity(downbeats.len());
    for &d in downbeats {
        let i = beats.partition_point(|&b| b < d);
        // The nearest beat is either side of the insertion point.
        let cand = [i.saturating_sub(1), i.min(beats.len().saturating_sub(1))];
        let best = cand
            .iter()
            .filter(|&&j| j < beats.len())
            .min_by(|&&a, &&b| {
                (beats[a] - d)
                    .abs()
                    .partial_cmp(&(beats[b] - d).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied();
        match best {
            Some(j) if (beats[j] - d).abs() <= tolerance => out.push(j),
            _ => tracing::debug!(downbeat = d, "downbeat matches no beat; dropped"),
        }
    }
    out.dedup();
    out
}

/// Normalised RMS per beat, parallel to `beats` (spec §5, ROADMAP M2).
///
/// Normalised against the 95th percentile rather than the maximum: one clipped
/// transient would otherwise push every other beat toward zero and flatten the
/// energy tiers M3 selects moves by.
pub fn beat_energy(samples: &[f32], sample_rate: u32, beats: &[f64]) -> Vec<f32> {
    if beats.is_empty() || samples.is_empty() || sample_rate == 0 {
        return Vec::new();
    }
    let sr = sample_rate as f64;

    let mut raw = Vec::with_capacity(beats.len());
    for (i, &b) in beats.iter().enumerate() {
        // Window runs to the next beat, so it follows local tempo rather than a
        // fixed span.
        let end = beats.get(i + 1).copied().unwrap_or(b + 0.5);
        let s = ((b * sr) as usize).min(samples.len());
        let e = ((end * sr) as usize).min(samples.len());
        raw.push(rms(&samples[s..e.max(s)]));
    }

    let mut sorted = raw.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    // Never let the loudest beat be its own normaliser. On a full track the 95th
    // percentile already excludes the top 5 %, but on a short one it rounds up to
    // the maximum, and then a single clipped transient flattens everything else
    // into the floor — which is the exact failure the percentile was chosen to
    // avoid. Capping the index at n-2 makes the intent hold at every length.
    let idx = ((sorted.len() as f32 * 0.95) as usize).min(sorted.len().saturating_sub(2));
    let p95 = sorted[idx];
    if p95 <= f32::EPSILON {
        return vec![0.0; beats.len()];
    }
    raw.iter().map(|v| (v / p95).clamp(0.0, 1.0)).collect()
}

fn rms(s: &[f32]) -> f32 {
    if s.is_empty() {
        return 0.0;
    }
    (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
}

/// Inputs to the confidence heuristic, kept separate so it can be reasoned about.
#[derive(Debug, Clone, Copy)]
pub struct ConfidenceInputs {
    pub beat_count: usize,
    /// Seconds from the first beat to the last.
    pub span: f64,
    /// Track duration in seconds.
    pub duration: f64,
    /// Mean inter-beat interval.
    pub ibi_mean: f64,
    /// Standard deviation of the inter-beat interval.
    pub ibi_sd: f64,
    /// Fraction of bars matching the modal length.
    pub meter_consistency: f32,
    /// Fraction of downbeat candidates the fitted phase explains.
    pub bar_agreement: f32,
}

/// How much to trust this grid, `0.0..1.0`. Gates entry to `Locked` at 0.6.
///
/// **This is a heuristic over proxies, not a measurement.** Nothing here knows
/// whether the beats are in the right places — that needs ground truth we do not
/// have at runtime. What it can see is whether the grid is *self-consistent* and
/// *covers the track*, and those catch the failure that matters: a partial or
/// incoherent detection presented as if it were solid.
///
/// Weighting reflects Phase 0.1. Coverage dominates, because a grid over half a
/// track is wrong for the other half. Inter-beat deviation is weighted *lightly*:
/// the waltz measured 12.5 % and its grid was good — that was expressive timing,
/// and since the clock reads local intervals rather than a global BPM, expressive
/// timing costs nothing. Weighting it heavily would reject exactly the material
/// that motivated carrying `meter` in the first place.
pub fn confidence(i: ConfidenceInputs) -> f32 {
    if i.beat_count < 8 || i.duration <= 0.0 {
        // Too little to schedule against. Better Unscored than a confident guess
        // (spec §8.3).
        return 0.0;
    }

    // A grid should cover most of the track. Full marks at 80 % coverage, since
    // intros and fade-outs legitimately carry no beats.
    let coverage = ((i.span / i.duration) as f32 / 0.8).clamp(0.0, 1.0);

    // Implausible tempo means something went wrong upstream.
    let bpm = if i.ibi_mean > 0.0 { 60.0 / i.ibi_mean } else { 0.0 };
    if !(40.0..=220.0).contains(&bpm) {
        return 0.0;
    }

    let stability = if i.ibi_mean > 0.0 {
        (1.0 - (i.ibi_sd / i.ibi_mean) as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // Base 0.5 for "there is a grid at all", the rest earned by bar structure.
    let structure = 0.5
        + 0.25 * i.meter_consistency.clamp(0.0, 1.0)
        + 0.15 * i.bar_agreement.clamp(0.0, 1.0)
        + 0.10 * stability;

    (coverage * structure).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_is_inferred_not_assumed() {
        // 4/4
        let counts: Vec<u8> = (0..40).map(|i| (i % 4 + 1) as u8).collect();
        assert_eq!(infer_meter(&counts), (4, 1.0));

        // 3/4 — Phase 0.1 found a real one, so this is not hypothetical.
        let counts: Vec<u8> = (0..39).map(|i| (i % 3 + 1) as u8).collect();
        let (m, c) = infer_meter(&counts);
        assert_eq!(m, 3);
        assert!(c > 0.9);
    }

    #[test]
    fn no_bar_structure_falls_back_without_claiming_confidence() {
        let (m, c) = infer_meter(&[2, 3, 4, 2, 3, 4]);
        assert_eq!(m, 4, "four is the fallback");
        assert_eq!(c, 0.0, "but nothing supports it");
    }

    #[test]
    fn spurious_downbeats_do_not_corrupt_the_bar_phase() {
        // Phase 0.1's exact failure: a clean 4/4 grid with extra downbeats
        // halving bars. Candidates at every 2 beats; the true grid is every 4.
        let downbeats: Vec<usize> = (0..40).step_by(2).collect();
        let grid = fit_bar_phase(40, 4, &downbeats);
        assert_eq!(grid.phase, 0);
        // Half the candidates are spurious and simply fail to vote.
        assert!((grid.agreement - 0.5).abs() < 0.01);

        let pos = beat_positions(8, &grid);
        assert_eq!(pos, vec![1, 2, 3, 4, 1, 2, 3, 4]);
    }

    #[test]
    fn bar_phase_is_found_when_the_track_does_not_start_on_one() {
        // Pickup bar: the first downbeat is beat 2.
        let downbeats: Vec<usize> = (2..40).step_by(4).collect();
        let grid = fit_bar_phase(40, 4, &downbeats);
        assert_eq!(grid.phase, 2);
        assert_eq!(grid.agreement, 1.0);
        assert_eq!(beat_positions(6, &grid), vec![3, 4, 1, 2, 3, 4]);
    }

    #[test]
    fn one_bad_downbeat_loses_the_vote_rather_than_winning_it() {
        let mut downbeats: Vec<usize> = (0..40).step_by(4).collect();
        downbeats.push(7); // off-grid
        downbeats.sort();
        let grid = fit_bar_phase(40, 4, &downbeats);
        assert_eq!(grid.phase, 0);
        assert!(grid.agreement > 0.9);
    }

    #[test]
    fn downbeats_snap_to_beats_and_strays_are_dropped() {
        let beats: Vec<f64> = (0..10).map(|i| i as f64 * 0.5).collect();
        // Slightly off, exact, and nowhere near anything.
        let got = snap_downbeats(&beats, &[0.002, 2.0, 3.77], 0.05);
        assert_eq!(got, vec![0, 4]);
    }

    #[test]
    fn energy_tracks_loudness_and_resists_one_loud_transient() {
        let sr = 1000;
        let beats: Vec<f64> = (0..10).map(|i| i as f64).collect();
        let mut samples = vec![0.1_f32; sr as usize * 10];
        // Beat 5 is loud; beat 9 is a single absurd clip.
        for s in samples[5000..6000].iter_mut() {
            *s = 0.5;
        }
        for s in samples[9000..10000].iter_mut() {
            *s = 40.0;
        }

        let e = beat_energy(&samples, sr, &beats);
        assert_eq!(e.len(), 10);
        assert!(e[5] > e[0] * 4.0, "louder beat should read higher");
        // The clip does not flatten everything else into the floor.
        assert!(e[5] > 0.5, "p95 normalisation should survive one outlier, got {}", e[5]);
        assert!(e.iter().all(|v| (0.0..=1.0).contains(v)));
    }

    fn inputs() -> ConfidenceInputs {
        ConfidenceInputs {
            beat_count: 300,
            span: 175.0,
            duration: 180.0,
            ibi_mean: 0.5,
            ibi_sd: 0.007,
            meter_consistency: 0.98,
            bar_agreement: 0.95,
            }
    }

    #[test]
    fn a_clean_grid_earns_locked() {
        assert!(confidence(inputs()) > 0.9);
    }

    #[test]
    fn the_waltz_is_not_punished_for_expressive_timing() {
        // Phase 0.1: 12.5 % inter-beat deviation, 97 % meter consistency, and a
        // grid judged good. The clock reads local intervals, so the deviation
        // costs nothing and must not push it below the 0.6 gate.
        let c = confidence(ConfidenceInputs {
            ibi_mean: 0.34,
            ibi_sd: 0.0425,
            meter_consistency: 0.97,
            ..inputs()
        });
        assert!(c > 0.6, "waltz scored {c}, would be rejected");
    }

    #[test]
    fn shaky_bars_over_a_steady_beat_grid_still_lock() {
        // Phase 0.1's `pachimu`: rock-steady beats, 51 % meter consistency. The
        // beats are what the animation follows, so this should still lock.
        let c = confidence(ConfidenceInputs {
            meter_consistency: 0.51,
            bar_agreement: 0.5,
            ..inputs()
        });
        assert!(c > 0.6, "scored {c}");
    }

    #[test]
    fn partial_coverage_is_rejected() {
        // Beats over only the first third: right there, wrong everywhere else.
        let c = confidence(ConfidenceInputs { span: 60.0, ..inputs() });
        assert!(c < 0.6, "scored {c}");
    }

    #[test]
    fn nonsense_is_rejected_outright() {
        // Phase 0.1: non-musical input returns an empty grid. That must score 0.
        assert_eq!(confidence(ConfidenceInputs { beat_count: 0, span: 0.0, ..inputs() }), 0.0);
        // Implausible tempo.
        assert_eq!(confidence(ConfidenceInputs { ibi_mean: 0.05, ..inputs() }), 0.0);
        assert_eq!(confidence(ConfidenceInputs { ibi_mean: 3.0, ..inputs() }), 0.0);
    }
}
