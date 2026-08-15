//! Score types, JSON, and beat-grid queries (spec §5).
//!
//! The score is the beat grid: where the beats are, which are downbeats, and what
//! the track is doing around them. Everything the scheduler decides is a query
//! against this.
//!
//! Two rules run through the whole module, both from spec §5:
//!
//! - **Never assume 4/4.** `meter` is data. Phase 0.1 detected 3/4 correctly on a
//!   waltz, and a hardcoded 4 would have silently mangled it.
//! - **Never trust individual downbeats.** They are detection candidates. Phase 0.1
//!   found a track with a rock-steady beat grid whose downbeats split 29 two-beat
//!   bars against 30 four-beat ones. [`Score::bar_origin`] therefore takes the
//!   *first* downbeat as a phase reference and counts bars arithmetically from
//!   there, rather than treating every downbeat as authoritative.
//!
//! The cache store lands in M2; this crate is types plus queries for now.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

pub mod ident;
pub use ident::{TrackId, TrackMeta};

/// Score schema version. Bump with a migration, never silently.
pub const SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ScoreError {
    #[error("reading {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        source: serde_json::Error,
    },
    #[error("score schema {found} is not supported (this build understands {SCHEMA})")]
    Schema { found: u32 },
    #[error("invalid score: {0}")]
    Invalid(String),
}

/// Which analyzer produced this grid (spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScoreSource {
    /// The default path (spec §8.1). No segment labels.
    BeatThis,
    /// Optional sidecar (spec §8.2). Carries segment labels.
    Allin1,
    /// Hand-written — fixtures and tests.
    Builtin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub energy: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub time: f64,
    pub kind: String,
    #[serde(default)]
    pub bars: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Score {
    #[serde(default = "default_schema")]
    pub schema: u32,
    pub track_id: String,
    pub duration_ms: u64,
    pub bpm: f64,
    /// Modal bar length in beats. **Do not assume 4** (spec §5).
    #[serde(default = "default_meter")]
    pub meter: u8,
    pub source: ScoreSource,
    pub confidence: f32,
    #[serde(default)]
    pub analyzed_at: String,
    pub beats: Vec<f64>,
    /// Beat number within the bar, 1-based. May be empty; derive from `meter` then.
    #[serde(default)]
    pub beat_positions: Vec<u8>,
    #[serde(default)]
    pub downbeats: Vec<f64>,
    /// **May be empty**, and is whenever the score came from §8.1 alone.
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub cues: Vec<Cue>,
}

fn default_schema() -> u32 {
    SCHEMA
}
fn default_meter() -> u8 {
    4
}

/// Where a media position falls on the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Phase {
    /// Index of the beat at or before the queried time.
    pub beat: usize,
    /// How far into that beat, `0.0..1.0`.
    pub frac: f64,
    /// Length of *this* beat in seconds — local, not derived from `bpm`.
    pub interval: f64,
    /// Beat number within the bar, 1-based. 1 means downbeat.
    pub bar_beat: u8,
}

impl Score {
    pub fn load(path: &Path) -> Result<Self, ScoreError> {
        let text = std::fs::read_to_string(path).map_err(|source| ScoreError::Io {
            path: path.to_owned(),
            source,
        })?;
        let score: Score = serde_json::from_str(&text).map_err(|source| ScoreError::Parse {
            path: path.to_owned(),
            source,
        })?;
        score.validate()?;
        tracing::info!(
            path = %path.display(),
            track = %score.track_id,
            bpm = score.bpm,
            meter = score.meter,
            beats = score.beats.len(),
            confidence = score.confidence,
            "loaded score"
        );
        Ok(score)
    }

    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Reject anything the queries below would silently misinterpret.
    ///
    /// This exists because M1's scores are hand-written. A typo that leaves the
    /// grid subtly out of order would show up as "the clock is broken", which is
    /// exactly the confusion the hand-written-first plan is meant to avoid
    /// (ROADMAP M1).
    pub fn validate(&self) -> Result<(), ScoreError> {
        if self.schema != SCHEMA {
            return Err(ScoreError::Schema { found: self.schema });
        }
        if self.meter == 0 {
            return Err(ScoreError::Invalid("meter is 0".into()));
        }
        if self.beats.is_empty() {
            return Err(ScoreError::Invalid("no beats".into()));
        }
        if !(0.0..=1.0).contains(&self.confidence) {
            return Err(ScoreError::Invalid(format!(
                "confidence {} outside 0..=1",
                self.confidence
            )));
        }
        for (i, w) in self.beats.windows(2).enumerate() {
            if !w[0].is_finite() {
                return Err(ScoreError::Invalid(format!("beat {i} is not finite")));
            }
            if w[1] <= w[0] {
                return Err(ScoreError::Invalid(format!(
                    "beats are not strictly increasing at index {i}: {} then {}",
                    w[0], w[1]
                )));
            }
        }
        if !self.beats[self.beats.len() - 1].is_finite() {
            return Err(ScoreError::Invalid("last beat is not finite".into()));
        }
        if !self.beat_positions.is_empty() && self.beat_positions.len() != self.beats.len() {
            return Err(ScoreError::Invalid(format!(
                "beat_positions has {} entries for {} beats",
                self.beat_positions.len(),
                self.beats.len()
            )));
        }
        if let Some(bad) = self.beat_positions.iter().find(|&&p| p == 0 || p > self.meter) {
            return Err(ScoreError::Invalid(format!(
                "beat position {bad} outside 1..={}",
                self.meter
            )));
        }
        Ok(())
    }

    pub fn duration_secs(&self) -> f64 {
        self.duration_ms as f64 / 1000.0
    }

    /// Index of the beat at or before `t`, or `None` before the first beat.
    pub fn beat_index_at(&self, t: f64) -> Option<usize> {
        if !t.is_finite() || t < self.beats[0] {
            return None;
        }
        // partition_point is the count of beats <= t, so subtract one for the index.
        Some(self.beats.partition_point(|&b| b <= t).saturating_sub(1))
    }

    /// Length of beat `i` in seconds.
    ///
    /// **Local, deliberately** (spec §11.1): measured from this beat to the next,
    /// not derived from `bpm`. Tracks drift, and live recordings drift a lot. The
    /// last beat has no successor, so it borrows its predecessor's interval, and a
    /// single-beat grid falls back to `bpm`.
    pub fn interval_at(&self, i: usize) -> f64 {
        let n = self.beats.len();
        if n >= 2 {
            let i = i.min(n - 2);
            self.beats[i + 1] - self.beats[i]
        } else if self.bpm > 0.0 {
            60.0 / self.bpm
        } else {
            0.5
        }
    }

    /// Beat number within the bar for beat `i`, 1-based.
    ///
    /// Prefers stored `beat_positions`; otherwise counts from [`Score::bar_origin`].
    pub fn bar_beat(&self, i: usize) -> u8 {
        if let Some(&p) = self.beat_positions.get(i) {
            return p;
        }
        let origin = self.bar_origin();
        let rel = i as i64 - origin as i64;
        let m = self.meter as i64;
        (rel.rem_euclid(m) + 1) as u8
    }

    /// Beat index that the bar grid is phased from.
    ///
    /// The *first* downbeat only — later ones are not consulted. Phase 0.1 found
    /// spurious downbeats halving bars on an otherwise clean grid, so counting
    /// arithmetically from one reference beats trusting each candidate. Fitting a
    /// proper phase with outlier rejection is M2's job, once real analyzer output
    /// exists to fit against.
    pub fn bar_origin(&self) -> usize {
        if let Some(i) = self.beat_positions.iter().position(|&p| p == 1) {
            return i;
        }
        if let Some(&d) = self.downbeats.first() {
            if let Some(i) = self.beat_index_at(d) {
                // Snap to whichever adjacent beat is actually closest — a downbeat
                // time a millisecond early would otherwise land on the beat before.
                let next = (i + 1).min(self.beats.len() - 1);
                return if (self.beats[next] - d).abs() < (d - self.beats[i]).abs() {
                    next
                } else {
                    i
                };
            }
        }
        0
    }

    /// Where `t` falls on the grid, or `None` before the first beat.
    pub fn phase_at(&self, t: f64) -> Option<Phase> {
        let beat = self.beat_index_at(t)?;
        let interval = self.interval_at(beat);
        let frac = if interval > 0.0 {
            ((t - self.beats[beat]) / interval).clamp(0.0, 1.0)
        } else {
            0.0
        };
        Some(Phase {
            beat,
            frac,
            interval,
            bar_beat: self.bar_beat(beat),
        })
    }

    /// Beats elapsed since [`Score::bar_origin`], fractional. Negative before it.
    ///
    /// This is the coordinate the animation runs in: continuous, monotonic, and
    /// phased so that whole numbers divisible by `meter` are bar starts.
    pub fn beat_offset(&self, t: f64) -> Option<f64> {
        let p = self.phase_at(t)?;
        Some(p.beat as f64 - self.bar_origin() as f64 + p.frac)
    }

    /// Progress through a loop of `beats_per_loop` beats, `0.0..1.0`.
    ///
    /// The whole of M1's animation: multiply by the cell count and floor. Because
    /// it is derived from the grid rather than accumulated per frame, it cannot
    /// drift — a dropped frame skips a cell instead of shifting the phase.
    pub fn loop_progress(&self, t: f64, beats_per_loop: u32) -> Option<f64> {
        let b = beats_per_loop.max(1) as f64;
        let off = self.beat_offset(t)?;
        Some(off.rem_euclid(b) / b)
    }

    /// Time of the first beat strictly after `t`.
    pub fn next_beat_after(&self, t: f64) -> Option<f64> {
        self.beats.get(self.beats.partition_point(|&b| b <= t)).copied()
    }

    /// Time of the next bar start strictly after `t`.
    ///
    /// Computed from the arithmetic bar grid, not from the `downbeats` list, for
    /// the reason in [`Score::bar_origin`]. Used by spec §10's "resume on the next
    /// downbeat" rule.
    pub fn next_bar_after(&self, t: f64) -> Option<f64> {
        let start = self.beats.partition_point(|&b| b <= t);
        (start..self.beats.len()).find(|&i| self.bar_beat(i) == 1).map(|i| self.beats[i])
    }

    /// Segment covering `t`, if the score has any. Frequently `None` (spec §5).
    pub fn segment_at(&self, t: f64) -> Option<&Segment> {
        self.segments.iter().find(|s| t >= s.start && t < s.end)
    }
}

/// A `Score` shared with the render thread without copying.
pub type SharedScore = Arc<Score>;

#[cfg(test)]
mod tests {
    use super::*;

    /// 120 BPM, 4/4 — a beat every 0.5 s, bar every 2 s.
    fn steady() -> Score {
        let beats: Vec<f64> = (0..64).map(|i| i as f64 * 0.5).collect();
        let positions: Vec<u8> = (0..64).map(|i| (i % 4 + 1) as u8).collect();
        Score {
            schema: SCHEMA,
            track_id: "test:steady".into(),
            duration_ms: 32_000,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::Builtin,
            confidence: 1.0,
            analyzed_at: String::new(),
            downbeats: beats.iter().copied().step_by(4).collect(),
            beats,
            beat_positions: positions,
            segments: vec![],
            cues: vec![],
        }
    }

    #[test]
    fn indexes_and_phases() {
        let s = steady();
        assert_eq!(s.beat_index_at(-0.1), None);
        assert_eq!(s.beat_index_at(0.0), Some(0));
        assert_eq!(s.beat_index_at(0.75), Some(1));
        assert_eq!(s.beat_index_at(1.0), Some(2));

        let p = s.phase_at(1.25).unwrap();
        assert_eq!(p.beat, 2);
        assert!((p.frac - 0.5).abs() < 1e-9);
        assert!((p.interval - 0.5).abs() < 1e-9);
        assert_eq!(p.bar_beat, 3);
    }

    #[test]
    fn interval_is_local_not_global() {
        // A grid that slows down: global BPM would be wrong everywhere.
        let mut s = steady();
        s.beats = vec![0.0, 0.5, 1.1, 1.8];
        s.beat_positions.clear();
        s.downbeats.clear();
        s.bpm = 120.0; // implies 0.5 everywhere — deliberately misleading
        assert!((s.interval_at(0) - 0.5).abs() < 1e-9);
        assert!((s.interval_at(1) - 0.6).abs() < 1e-9);
        assert!((s.interval_at(2) - 0.7).abs() < 1e-9);
        // Last beat borrows its predecessor rather than reaching for bpm.
        assert!((s.interval_at(3) - 0.7).abs() < 1e-9);
    }

    #[test]
    fn loop_progress_is_grid_derived() {
        let s = steady();
        // 4-beat loop = 2 s at this tempo.
        assert!((s.loop_progress(0.0, 4).unwrap() - 0.0).abs() < 1e-9);
        assert!((s.loop_progress(1.0, 4).unwrap() - 0.5).abs() < 1e-9);
        assert!((s.loop_progress(2.0, 4).unwrap() - 0.0).abs() < 1e-9);
        // 8 cells over a 4-beat loop: one cell per half beat.
        let cell = |t: f64| (s.loop_progress(t, 4).unwrap() * 8.0) as usize;
        assert_eq!(cell(0.0), 0);
        assert_eq!(cell(0.25), 1);
        assert_eq!(cell(1.75), 7);
    }

    #[test]
    fn meter_is_not_assumed_to_be_four() {
        // 3/4 waltz — Phase 0.1 found one, so this is not hypothetical.
        let beats: Vec<f64> = (0..12).map(|i| i as f64 * 0.4).collect();
        let s = Score {
            meter: 3,
            beats,
            beat_positions: vec![],
            downbeats: vec![],
            ..steady()
        };
        assert_eq!(s.bar_beat(0), 1);
        assert_eq!(s.bar_beat(2), 3);
        assert_eq!(s.bar_beat(3), 1);
        // A bar is three beats, so the next bar after t=0.5 is at 1.2, not 1.6.
        assert!((s.next_bar_after(0.5).unwrap() - 1.2).abs() < 1e-9);
    }

    #[test]
    fn bar_grid_ignores_spurious_downbeats() {
        // Phase 0.1's failure: extra downbeats halving bars on a clean grid.
        let mut s = steady();
        s.beat_positions.clear();
        s.downbeats = vec![0.0, 1.0, 2.0, 3.0, 4.0]; // every 2 beats, wrong
        // Only the first is consulted, so the bar grid stays 4 beats wide.
        assert_eq!(s.bar_origin(), 0);
        assert_eq!(s.bar_beat(2), 3);
        assert!((s.next_bar_after(0.1).unwrap() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn bar_origin_snaps_to_nearest_beat() {
        let mut s = steady();
        s.beat_positions.clear();
        // A downbeat reported a hair before its beat must not land on the one before.
        s.downbeats = vec![1.999];
        assert_eq!(s.bar_origin(), 4);
    }

    #[test]
    fn validation_catches_hand_written_mistakes() {
        let mut s = steady();
        s.beats[10] = s.beats[9]; // duplicate — a plausible typo
        assert!(matches!(s.validate(), Err(ScoreError::Invalid(_))));

        let mut s = steady();
        s.beat_positions.pop(); // length mismatch
        assert!(matches!(s.validate(), Err(ScoreError::Invalid(_))));

        let mut s = steady();
        s.beat_positions[0] = 5; // outside 1..=meter
        assert!(matches!(s.validate(), Err(ScoreError::Invalid(_))));

        let mut s = steady();
        s.schema = 99;
        assert!(matches!(s.validate(), Err(ScoreError::Schema { found: 99 })));

        let mut s = steady();
        s.confidence = 1.5;
        assert!(matches!(s.validate(), Err(ScoreError::Invalid(_))));
    }

    #[test]
    fn json_round_trips() {
        let s = steady();
        let back: Score = serde_json::from_str(&s.to_json().unwrap()).unwrap();
        back.validate().unwrap();
        assert_eq!(back.beats, s.beats);
        assert_eq!(back.source, ScoreSource::Builtin);
        // The wire name is kebab-case, per spec §5.
        assert!(s.to_json().unwrap().contains("\"builtin\""));
    }

    #[test]
    fn tolerates_empty_segments() {
        // Every beat-this score looks like this (spec §5, §8.1).
        let s = steady();
        assert!(s.segments.is_empty());
        assert!(s.segment_at(1.0).is_none());
    }
}
