//! `BeatClock` — position estimation and drift correction (spec §9).
//!
//! Position must be known to roughly ±20 ms from sources that report a few seconds
//! apart. The answer is a local free-running clock that observations *steer* rather
//! than *set*: media playback clocks are extremely stable, so the error accumulates
//! smoothly and infrequent coarse observations are enough.
//!
//! # Two time frames, and why mixing them is a bug
//!
//! - **Media time** is what the player reports: "42.0 s into the track".
//! - **Output time** is when that sound reaches the speakers, later by `offset`
//!   (spec §9.2, 100–300 ms).
//!
//! The dancer must move in *output* time, so [`BeatClock::position`] subtracts the
//! offset. But observations arrive in *media* time, so corrections must compare
//! against [`BeatClock::media_position`]. Comparing an observation against the
//! offset-adjusted estimate would make the clock chase its own latency
//! compensation and settle a full `offset` out of place.
//!
//! # Staleness is not the same as coarseness
//!
//! Phase 0.5 measured SMTC reporting a position 87 seconds old — but *precisely*
//! 87 seconds old, with `LastUpdatedTime` naming the moment it was true. So the
//! adapter pairs the reported position with that timestamp in `observed_at`, and
//! this module never needs to know how stale an observation was: the pair is exact
//! whenever it was taken. That is why `observed_at` must be the anchor time, not
//! the moment the value was read.

use std::sync::Arc;
use std::time::Instant;

use dancer_score::{Phase, Score};

/// Past this, treat it as a seek rather than drift, and drop to `Resync` (spec §9.1).
pub const SEEK_THRESHOLD: f64 = 1.5;
/// Past this, the rate cannot close the gap in reasonable time — re-anchor.
pub const SLEW_LIMIT: f64 = 0.25;
/// Seconds over which a slew is meant to absorb the error.
pub const SLEW_WINDOW: f64 = 5.0;
/// Rate bounds. ±2 % is imperceptible in tempo but closes 10 ms per second.
pub const RATE_MIN: f64 = 0.98;
pub const RATE_MAX: f64 = 1.02;
/// Below this, a score does not earn `Locked` (spec §5, §10).
pub const MIN_CONFIDENCE: f32 = 0.6;

/// Default output latency by source kind (spec §9.2). Manual calibration adjusts.
pub const DEFAULT_OFFSET_LOCAL: f64 = 0.180;
pub const DEFAULT_OFFSET_BROWSER: f64 = 0.250;

/// What an observation did to the clock. The state machine keys off this.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Correction {
    /// Playback stopped; the clock is frozen (spec §10).
    Frozen,
    /// Playback resumed and the clock re-anchored.
    Resumed,
    /// Still paused — nothing to do.
    Idle,
    /// Error beyond [`SEEK_THRESHOLD`]. Caller should drop to `Resync`.
    Seek { err: f64 },
    /// Error beyond [`SLEW_LIMIT`]. Stepped; see the note on [`BeatClock::observe`].
    Reanchor { err: f64 },
    /// Absorbed by bending the rate. Position stays continuous.
    Slew { err: f64, rate: f64 },
}

#[derive(Debug, Clone)]
pub struct BeatClock {
    score: Option<Arc<Score>>,
    /// Media-time seconds at the anchor.
    anchor_media: f64,
    /// Local monotonic instant the anchor was taken.
    anchor_local: Instant,
    /// 1.0 nominal, slewed to absorb drift.
    rate: f64,
    /// Calibrated output latency, seconds.
    offset: f64,
    confidence: f32,
    playing: bool,
}

impl BeatClock {
    pub fn new(now: Instant, offset: f64) -> Self {
        Self {
            score: None,
            anchor_media: 0.0,
            anchor_local: now,
            rate: 1.0,
            offset,
            confidence: 0.0,
            playing: false,
        }
    }

    pub fn score(&self) -> Option<&Arc<Score>> {
        self.score.as_ref()
    }

    /// Attach a score. Confidence below [`MIN_CONFIDENCE`] keeps us out of `Locked`
    /// (spec §10) — the grid is kept anyway so the caller can inspect it.
    pub fn set_score(&mut self, score: Option<Arc<Score>>) {
        self.confidence = score.as_ref().map_or(0.0, |s| s.confidence);
        self.score = score;
    }

    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    /// Is there a score good enough to schedule against?
    pub fn is_confident(&self) -> bool {
        self.score.is_some() && self.confidence >= MIN_CONFIDENCE
    }

    pub fn playing(&self) -> bool {
        self.playing
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    pub fn offset(&self) -> f64 {
        self.offset
    }

    /// Adjust output latency (spec §9.2's nudge slider).
    ///
    /// Deliberately does not touch the anchor: the offset shifts the mapping from
    /// media time to output time, and re-anchoring would cancel the change out.
    pub fn set_offset(&mut self, offset: f64) {
        self.offset = offset;
    }

    /// Position in *media* time — what the player would report right now.
    pub fn media_position(&self, now: Instant) -> f64 {
        if !self.playing {
            return self.anchor_media;
        }
        // saturating: a caller passing an instant before the anchor should get the
        // anchor back, not a panic.
        let elapsed = now.saturating_duration_since(self.anchor_local).as_secs_f64();
        self.anchor_media + elapsed * self.rate
    }

    /// Position in *output* time — where the sound is now. Use this to animate.
    pub fn position(&self, now: Instant) -> f64 {
        self.media_position(now) - self.offset
    }

    /// Grid phase at `now`, or `None` without a score or before the first beat.
    pub fn phase(&self, now: Instant) -> Option<Phase> {
        self.score.as_ref()?.phase_at(self.position(now))
    }

    /// Hard re-anchor: position jumps. For seeks and track changes.
    pub fn reanchor(&mut self, media: f64, at: Instant) {
        self.anchor_media = media;
        self.anchor_local = at;
        self.rate = 1.0;
        self.playing = true;
    }

    /// Stop advancing, holding the current estimate (spec §10).
    pub fn freeze(&mut self, at: Instant) {
        self.anchor_media = self.media_position(at);
        self.anchor_local = at;
        self.playing = false;
    }

    /// Fold in one observation and report what it did (spec §9.1).
    ///
    /// `position` is media time as the source reported it; `at` is the instant that
    /// value was true — for SMTC, `LastUpdatedTime`, **not** the moment of the read.
    ///
    /// # A known tension in the spec
    ///
    /// §9.1 prescribes a hard re-anchor above [`SLEW_LIMIT`], while §9 also says
    /// never to step position while `Locked`. Both cannot hold: ±2 % closes only
    /// 100 ms per 5 s, so a 1.4 s error would take over a minute to slew out, and
    /// staying a beat off that long is worse than one visible jump. This implements
    /// §9.1 literally and returns [`Correction::Reanchor`] so the caller knows a
    /// step happened. M3 should defer the step to a loop boundary, where it is
    /// hidden — that is the resolution, and it needs the scheduler to exist.
    pub fn observe(&mut self, position: f64, playing: bool, at: Instant) -> Correction {
        if !playing {
            return if self.playing {
                self.freeze(at);
                Correction::Frozen
            } else {
                // Keep the held position honest — a user who scrubs while paused
                // should not snap back on resume.
                self.anchor_media = position;
                self.anchor_local = at;
                Correction::Idle
            };
        }

        if !self.playing {
            self.reanchor(position, at);
            return Correction::Resumed;
        }

        // Media frame on both sides. See the module note.
        let est = self.media_position(at);
        let err = position - est;

        if !err.is_finite() {
            self.reanchor(position, at);
            return Correction::Reanchor { err: 0.0 };
        }

        if err.abs() > SEEK_THRESHOLD {
            self.reanchor(position, at);
            Correction::Seek { err }
        } else if err.abs() > SLEW_LIMIT {
            self.reanchor(position, at);
            Correction::Reanchor { err }
        } else {
            // Re-anchor at the *estimate*, not the observation, so position is
            // continuous across the correction. The rate carries the error out.
            let rate = (1.0 + err / SLEW_WINDOW).clamp(RATE_MIN, RATE_MAX);
            self.anchor_media = est;
            self.anchor_local = at;
            self.rate = rate;
            Correction::Slew { err, rate }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use dancer_score::{Score, ScoreSource, SCHEMA};

    use super::*;

    fn clock_at(t0: Instant) -> BeatClock {
        // Offset zero unless a test is specifically about offset, so that
        // media and output time coincide and assertions stay readable.
        BeatClock::new(t0, 0.0)
    }

    /// A player whose clock runs slightly fast, as real ones do.
    struct FakePlayer {
        start: Instant,
        rate: f64,
        seek: f64,
    }

    impl FakePlayer {
        fn position(&self, now: Instant) -> f64 {
            self.seek + now.duration_since(self.start).as_secs_f64() * self.rate
        }
    }

    #[test]
    fn tracks_a_drifting_player_for_three_minutes() {
        // ROADMAP M1's exit criterion, made measurable. The player runs 0.02 %
        // fast — far more than a real one — and reports every 3 s with the
        // observation timestamped 2 s in the past, as a stale SMTC read would be.
        let t0 = Instant::now();
        let player = FakePlayer {
            start: t0,
            rate: 1.0002,
            seek: 0.0,
        };
        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);

        let mut worst: f64 = 0.0;
        let mut steps = 0;
        for tick in 1..=3600 {
            // 50 ms per tick = 3 minutes.
            let now = t0 + Duration::from_millis(tick * 50);
            if tick % 60 == 0 {
                // Observation from 2 s ago, paired with the instant it was true.
                let at = now - Duration::from_secs(2);
                let c = clock.observe(player.position(at), true, at);
                if matches!(c, Correction::Reanchor { .. } | Correction::Seek { .. }) {
                    steps += 1;
                }
            }
            worst = worst.max((clock.position(now) - player.position(now)).abs());
        }

        // Spec §9 asks for ~±20 ms.
        assert!(worst < 0.020, "worst error {worst:.4}s over 3 minutes");
        // And it got there by slewing, not by jumping.
        assert_eq!(steps, 0, "clock stepped {steps} times while Locked");
    }

    #[test]
    fn slew_never_steps_the_position() {
        // The property spec §9 actually cares about: a correction must not produce
        // a visible discontinuity. Sample either side of one.
        let t0 = Instant::now();
        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);

        let at = t0 + Duration::from_secs(10);
        let before = clock.position(at);
        let c = clock.observe(10.1, true, at); // 100 ms out — inside SLEW_LIMIT
        let after = clock.position(at);

        assert!(matches!(c, Correction::Slew { .. }));
        assert!(
            (after - before).abs() < 1e-9,
            "stepped by {}s during a slew",
            after - before
        );
        assert!(clock.rate() > 1.0, "rate should speed up to catch up");
    }

    #[test]
    fn slew_actually_closes_the_error() {
        let t0 = Instant::now();
        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);
        let at = t0 + Duration::from_secs(10);
        clock.observe(10.1, true, at);

        // After SLEW_WINDOW at the slewed rate the gap should be gone.
        let later = at + Duration::from_secs_f64(SLEW_WINDOW);
        let err = (10.1 + SLEW_WINDOW) - clock.media_position(later);
        assert!(err.abs() < 0.005, "residual error {err:.4}s");
    }

    #[test]
    fn classifies_by_error_size() {
        let t0 = Instant::now();
        let at = t0 + Duration::from_secs(10);

        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);
        assert!(matches!(
            clock.observe(10.05, true, at),
            Correction::Slew { .. }
        ));

        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);
        assert!(matches!(
            clock.observe(10.5, true, at),
            Correction::Reanchor { .. }
        ));

        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);
        assert!(matches!(clock.observe(40.0, true, at), Correction::Seek { .. }));
        // A seek resets the rate: the old slew was correcting a world that is gone.
        assert_eq!(clock.rate(), 1.0);
        assert!((clock.position(at) - 40.0).abs() < 1e-9);
    }

    #[test]
    fn pause_freezes_and_resume_reanchors() {
        let t0 = Instant::now();
        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);

        let paused_at = t0 + Duration::from_secs(10);
        assert_eq!(clock.observe(10.0, false, paused_at), Correction::Frozen);

        // Time passes; position must not.
        let held = clock.position(paused_at + Duration::from_secs(30));
        assert!((held - 10.0).abs() < 1e-6, "position advanced while paused");
        assert!(!clock.playing());

        let resumed_at = paused_at + Duration::from_secs(30);
        assert_eq!(clock.observe(10.0, true, resumed_at), Correction::Resumed);
        let after = clock.position(resumed_at + Duration::from_secs(1));
        assert!((after - 11.0).abs() < 1e-6);
    }

    #[test]
    fn scrubbing_while_paused_is_honoured() {
        let t0 = Instant::now();
        let mut clock = clock_at(t0);
        clock.observe(0.0, true, t0);
        let at = t0 + Duration::from_secs(5);
        clock.observe(5.0, false, at);
        // User drags the seek bar with playback stopped.
        assert_eq!(clock.observe(90.0, false, at), Correction::Idle);
        assert!((clock.position(at) - 90.0).abs() < 1e-9);
    }

    #[test]
    fn offset_shifts_output_but_not_the_correction_frame() {
        // The bug this guards: comparing an observation against the offset-adjusted
        // estimate makes the clock chase its own latency compensation.
        let t0 = Instant::now();
        let mut clock = BeatClock::new(t0, 0.2);
        clock.observe(0.0, true, t0);

        let at = t0 + Duration::from_secs(10);
        // The player reports exactly what our media estimate says, so this is a
        // perfect observation and must produce a near-zero error.
        let c = clock.observe(10.0, true, at);
        match c {
            Correction::Slew { err, .. } => assert!(err.abs() < 1e-9, "err {err} should be ~0"),
            other => panic!("expected a no-op slew, got {other:?}"),
        }
        // Output time still trails media time by the offset.
        assert!((clock.media_position(at) - clock.position(at) - 0.2).abs() < 1e-9);
    }

    #[test]
    fn offset_change_moves_output_immediately() {
        let t0 = Instant::now();
        let mut clock = BeatClock::new(t0, 0.0);
        clock.observe(10.0, true, t0);
        let before = clock.position(t0);
        clock.set_offset(0.25);
        assert!((before - clock.position(t0) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn confidence_gates_locked() {
        let t0 = Instant::now();
        let mut clock = clock_at(t0);
        assert!(!clock.is_confident());

        let mut score = Score {
            schema: SCHEMA,
            track_id: "test:x".into(),
            duration_ms: 1000,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::Builtin,
            confidence: 0.5,
            analyzed_at: String::new(),
            beats: vec![0.0, 0.5, 1.0],
            beat_positions: vec![],
            downbeats: vec![],
            segments: vec![],
            cues: vec![],
            beat_energy: vec![],
        };
        clock.set_score(Some(Arc::new(score.clone())));
        assert!(!clock.is_confident(), "0.5 must not reach Locked");

        score.confidence = 0.6;
        clock.set_score(Some(Arc::new(score)));
        assert!(clock.is_confident(), "0.6 is the documented threshold");
    }

    #[test]
    fn phase_reads_through_the_offset() {
        let t0 = Instant::now();
        let mut clock = BeatClock::new(t0, 0.25);
        let score = Score {
            schema: SCHEMA,
            track_id: "test:x".into(),
            duration_ms: 8000,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::Builtin,
            confidence: 1.0,
            analyzed_at: String::new(),
            beats: (0..16).map(|i| i as f64 * 0.5).collect(),
            beat_positions: vec![],
            downbeats: vec![],
            segments: vec![],
            cues: vec![],
            beat_energy: vec![],
        };
        clock.set_score(Some(Arc::new(score)));
        clock.observe(2.25, true, t0);
        // Media 2.25 minus 0.25 offset = output 2.0, which is beat 4 exactly.
        let p = clock.phase(t0).unwrap();
        assert_eq!(p.beat, 4);
        assert!(p.frac < 1e-9);
    }
}
