//! Playback state: the clock, the state machine, and cell selection.
//!
//! Split out of `main.rs` so it can be tested without a window. Everything here is
//! pure given a `Instant`, which is what lets M1's exit criterion be a test rather
//! than a judgement call.

use std::time::Instant;

use dancer_clock::{BeatClock, Correction};
use dancer_score::TrackMeta;

use crate::events::AppEvent;

/// Spec §10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// Nothing playing.
    Idle,
    /// Track known, score lookup in flight.
    Identifying,
    /// Playing, no usable score. Default row at a fixed fps — FAOSDance behaviour,
    /// honestly labelled. Not a tempo guess.
    Unscored,
    /// Score loaded, clock confident. Full predictive scheduling.
    Locked,
    /// Seek or skip detected; re-anchoring.
    Resync,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Idle => "Idle",
            State::Identifying => "Identifying",
            State::Unscored => "Unscored",
            State::Locked => "Locked",
            State::Resync => "Resync",
        }
    }

    /// Does the animation follow the beat grid in this state?
    pub fn is_grid_driven(self) -> bool {
        matches!(self, State::Locked | State::Resync)
    }
}

pub struct Playback {
    pub state: State,
    pub clock: BeatClock,
    pub track: Option<TrackMeta>,
    /// Consecutive observations that agreed after a seek. Spec §10 requires two
    /// before `Resync` returns to `Locked`.
    agreements: u8,
}

impl Playback {
    pub fn new(now: Instant, offset: f64) -> Self {
        Self {
            state: State::Idle,
            clock: BeatClock::new(now, offset),
            track: None,
            agreements: 0,
        }
    }

    /// Fold one message in. Returns true if the state changed.
    pub fn apply(&mut self, ev: AppEvent) -> bool {
        let before = self.state;
        match ev {
            AppEvent::TrackChanged { id, meta } => {
                tracing::info!(track = %id, title = %meta.title, "track changed");
                self.track = Some(meta);
                self.clock.set_score(None);
                self.agreements = 0;
                // From any state (spec §10). Queued moves are cancelled by having
                // no score to schedule against.
                self.state = State::Identifying;
            }
            AppEvent::ScoreReady { id, score } => {
                // Only if the track has not moved on underneath us.
                let still_current = self.track.as_ref().is_some_and(|t| t.id == id);
                if !still_current {
                    tracing::debug!(track = %id, "score arrived for a track no longer playing");
                    return false;
                }
                let confident = score.confidence >= dancer_clock::MIN_CONFIDENCE;
                self.clock.set_score(Some(score));
                self.state = if confident {
                    State::Locked
                } else {
                    // A confidently wrong grid is worse than none (spec §8.3).
                    tracing::info!(
                        confidence = self.clock.confidence(),
                        "score below the confidence threshold; staying Unscored"
                    );
                    State::Unscored
                };
            }
            AppEvent::PositionReport { pos_secs, playing, at } => {
                let correction = self.clock.observe(pos_secs, playing, at);
                self.on_correction(correction);
            }
            AppEvent::PlaybackStopped => {
                self.clock.freeze(Instant::now());
                self.state = State::Idle;
            }
            AppEvent::SourceLost(msg) => {
                tracing::warn!(error = %msg, "source lost");
                self.clock.set_score(None);
                self.track = None;
                self.state = State::Idle;
            }
        }
        if self.state != before {
            tracing::info!(from = before.name(), to = self.state.name(), "state");
        }
        self.state != before
    }

    fn on_correction(&mut self, c: Correction) {
        match c {
            Correction::Seek { err } => {
                tracing::debug!(err, "seek detected");
                self.agreements = 0;
                if self.state == State::Locked {
                    self.state = State::Resync;
                }
            }
            Correction::Reanchor { err } => {
                // A step, and spec §9 would rather it were not. See the note on
                // BeatClock::observe: M3 hides it at a loop boundary.
                tracing::debug!(err, "hard re-anchor");
            }
            Correction::Slew { .. } => {
                if self.state == State::Resync {
                    self.agreements = self.agreements.saturating_add(1);
                    // Two consecutive polls agreeing (spec §10).
                    if self.agreements >= 2 && self.clock.is_confident() {
                        self.state = State::Locked;
                        self.agreements = 0;
                    }
                }
            }
            Correction::Frozen => {
                // Do not cut mid-move; the row plays out to its loop boundary. The
                // clock is frozen, so the animation stops advancing on its own.
                tracing::debug!("playback paused; clock frozen");
            }
            Correction::Resumed => {
                self.agreements = 0;
                if self.clock.is_confident() && self.state != State::Locked {
                    // Spec §10: resume waits for the next downbeat before full
                    // moves. With M1's single looping row there is nothing to wait
                    // for; M3 reinstates the wait when it has moves to delay.
                    self.state = State::Locked;
                }
            }
            Correction::Idle => {}
        }
    }

    /// Which cell of `row` should be showing at `now`.
    ///
    /// Derived from the grid rather than accumulated per frame, so it cannot drift:
    /// a dropped frame skips a cell instead of shifting the phase. Returns `None`
    /// when there is no grid to read, which is every non-`Locked` state.
    pub fn grid_cell(&self, now: Instant, beats_per_loop: u32, cells: usize) -> Option<usize> {
        if !self.state.is_grid_driven() || cells == 0 {
            return None;
        }
        let score = self.clock.score()?;
        let progress = score.loop_progress(self.clock.position(now), beats_per_loop)?;
        Some(((progress * cells as f64) as usize).min(cells - 1))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use dancer_score::{Score, ScoreSource, TrackId, SCHEMA};

    use super::*;

    fn score(confidence: f32) -> Arc<Score> {
        // 120 BPM, 4/4, three minutes.
        let beats: Vec<f64> = (0..360).map(|i| i as f64 * 0.5).collect();
        Arc::new(Score {
            schema: SCHEMA,
            track_id: "file:test.wav".into(),
            duration_ms: 180_000,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::Builtin,
            confidence,
            analyzed_at: String::new(),
            beat_positions: (0..360).map(|i| (i % 4 + 1) as u8).collect(),
            downbeats: beats.iter().copied().step_by(4).collect(),
            beats,
            segments: vec![],
            cues: vec![],
        })
    }

    fn meta() -> TrackMeta {
        TrackMeta {
            id: TrackId::new("file", "test.wav"),
            title: "test".into(),
            artist: String::new(),
            duration_secs: Some(180.0),
        }
    }

    fn locked(now: Instant) -> Playback {
        let mut p = Playback::new(now, 0.0);
        p.apply(AppEvent::TrackChanged {
            id: meta().id,
            meta: meta(),
        });
        p.apply(AppEvent::ScoreReady {
            id: meta().id,
            score: score(0.9),
        });
        p.apply(AppEvent::PositionReport {
            pos_secs: 0.0,
            playing: true,
            at: now,
        });
        p
    }

    #[test]
    fn low_confidence_score_stays_unscored() {
        let now = Instant::now();
        let mut p = Playback::new(now, 0.0);
        p.apply(AppEvent::TrackChanged { id: meta().id, meta: meta() });
        assert_eq!(p.state, State::Identifying);
        p.apply(AppEvent::ScoreReady { id: meta().id, score: score(0.5) });
        assert_eq!(p.state, State::Unscored);
        assert!(p.grid_cell(now, 4, 8).is_none(), "Unscored must not read the grid");
    }

    #[test]
    fn stale_score_for_an_old_track_is_ignored() {
        let now = Instant::now();
        let mut p = Playback::new(now, 0.0);
        p.apply(AppEvent::TrackChanged { id: meta().id, meta: meta() });
        p.apply(AppEvent::ScoreReady {
            id: TrackId::new("file", "something-else.wav"),
            score: score(0.9),
        });
        assert_eq!(p.state, State::Identifying, "must not lock onto the wrong track");
    }

    #[test]
    fn cells_follow_the_beat_grid() {
        let now = Instant::now();
        let p = locked(now);
        // 4-beat loop at 120 BPM = 2 s, over 8 cells = one cell per quarter second.
        let at = |ms: u64| p.grid_cell(now + Duration::from_millis(ms), 4, 8).unwrap();
        assert_eq!(at(0), 0);
        assert_eq!(at(250), 1);
        assert_eq!(at(1750), 7);
        assert_eq!(at(2000), 0, "loop wraps on the bar");
    }

    #[test]
    fn a_dropped_frame_skips_a_cell_it_does_not_shift_phase() {
        // The property that makes grid-derived selection worth it.
        let now = Instant::now();
        let p = locked(now);
        // Sample as if the app stalled for a second and a half.
        assert_eq!(p.grid_cell(now + Duration::from_millis(1750), 4, 8), Some(7));
        // Phase is still correct afterwards, not 1.5 s behind.
        assert_eq!(p.grid_cell(now + Duration::from_millis(2000), 4, 8), Some(0));
    }

    #[test]
    fn beats_per_loop_is_honoured() {
        let now = Instant::now();
        let p = locked(now);
        // An 8-beat loop takes 4 s, so the halfway cell arrives at 2 s.
        assert_eq!(p.grid_cell(now, 8, 8), Some(0));
        assert_eq!(p.grid_cell(now + Duration::from_secs(2), 8, 8), Some(4));
    }

    #[test]
    fn seek_drops_to_resync_and_two_agreements_restore_lock() {
        let now = Instant::now();
        let mut p = locked(now);

        let t1 = now + Duration::from_secs(10);
        p.apply(AppEvent::PositionReport { pos_secs: 90.0, playing: true, at: t1 });
        assert_eq!(p.state, State::Resync, "a 80 s jump is a seek");

        // One agreeing poll is not enough.
        let t2 = t1 + Duration::from_secs(3);
        p.apply(AppEvent::PositionReport { pos_secs: 93.0, playing: true, at: t2 });
        assert_eq!(p.state, State::Resync);

        let t3 = t2 + Duration::from_secs(3);
        p.apply(AppEvent::PositionReport { pos_secs: 96.0, playing: true, at: t3 });
        assert_eq!(p.state, State::Locked);
    }

    #[test]
    fn pause_freezes_the_animation_rather_than_cutting_it() {
        let now = Instant::now();
        let mut p = locked(now);
        let paused = now + Duration::from_millis(1250);
        let cell = p.grid_cell(paused, 4, 8);
        p.apply(AppEvent::PositionReport { pos_secs: 1.25, playing: false, at: paused });

        // Same cell half a minute later: the clock is frozen, not reset.
        assert_eq!(p.grid_cell(paused + Duration::from_secs(30), 4, 8), cell);
        assert!(!p.clock.playing());
    }

    #[test]
    fn track_change_clears_the_score() {
        let now = Instant::now();
        let mut p = locked(now);
        let next = TrackMeta {
            id: TrackId::new("file", "other.wav"),
            title: "other".into(),
            artist: String::new(),
            duration_secs: Some(100.0),
        };
        p.apply(AppEvent::TrackChanged { id: next.id.clone(), meta: next });
        assert_eq!(p.state, State::Identifying);
        assert!(p.clock.score().is_none(), "stale grid must not survive a track change");
        assert!(p.grid_cell(now, 4, 8).is_none());
    }

    /// ROADMAP M1's exit criterion, as a test.
    ///
    /// A player drifting 0.02 % fast, polled every 3 s with each reading 2 s stale,
    /// against a hand-written 120 BPM grid. Three minutes of it, checked every
    /// 50 ms: the cell shown must never differ from the cell the true position
    /// calls for.
    #[test]
    fn no_visible_drift_over_three_minutes() {
        let now = Instant::now();
        let mut p = locked(now);
        let s = score(0.9);
        let player_rate = 1.0002;
        let true_pos = |t: Duration| t.as_secs_f64() * player_rate;

        let mut worst_err: f64 = 0.0;
        let mut mismatches = 0;

        for tick in 1..=3600u64 {
            let elapsed = Duration::from_millis(tick * 50);
            let at = now + elapsed;

            if tick % 60 == 0 {
                let stale_at = at - Duration::from_secs(2);
                p.apply(AppEvent::PositionReport {
                    pos_secs: true_pos(elapsed - Duration::from_secs(2)),
                    playing: true,
                    at: stale_at,
                });
            }

            assert_eq!(p.state, State::Locked, "must stay Locked at tick {tick}");

            let shown = p.grid_cell(at, 4, 8).unwrap();
            let want = (s.loop_progress(true_pos(elapsed), 4).unwrap() * 8.0) as usize % 8;
            if shown != want {
                mismatches += 1;
            }
            worst_err = worst_err.max((p.clock.position(at) - true_pos(elapsed)).abs());
        }

        assert!(worst_err < 0.020, "worst position error {worst_err:.4}s");
        // Cell boundaries land every 250 ms here; a handful of samples straddling
        // one would be sampling noise, not drift. Zero is the honest bar.
        assert_eq!(mismatches, 0, "{mismatches} frames showed the wrong cell");
    }
}
