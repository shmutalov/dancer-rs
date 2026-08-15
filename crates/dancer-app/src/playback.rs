//! Playback state: the clock, the state machine, and cell selection.
//!
//! Split out of `main.rs` so it can be tested without a window. Everything here is
//! pure given a `Instant`, which is what lets M1's exit criterion be a test rather
//! than a judgement call.

use std::time::Instant;

use dancer_choreo::{Frame, RowInfo, Scheduler};
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
    scheduler: Scheduler,
    /// When false, the scheduler is bypassed and the dancer loops one row off the
    /// grid — M1's behaviour exactly. This is the A/B switch M3 exits on.
    anticipate: bool,
}

impl Playback {
    pub fn new(now: Instant, offset: f64, rows: Vec<RowInfo>, fallback: usize, seed: u64) -> Self {
        Self {
            state: State::Idle,
            clock: BeatClock::new(now, offset),
            track: None,
            agreements: 0,
            scheduler: Scheduler::new(rows, fallback, seed),
            anticipate: true,
        }
    }

    pub fn set_render_latency(&mut self, secs: f64) {
        self.scheduler.set_render_latency(secs);
    }

    pub fn anticipating(&self) -> bool {
        self.anticipate
    }

    /// Flip between anticipation and M1's plain grid loop.
    ///
    /// Exists for the A/B in ROADMAP M3: the difference is meant to be visible to
    /// someone who has not been told what changed, and that is much easier to
    /// judge when both can be seen back to back on the same track.
    pub fn toggle_anticipation(&mut self) -> bool {
        self.anticipate = !self.anticipate;
        self.scheduler.reset();
        tracing::info!(anticipate = self.anticipate, "anticipation toggled");
        self.anticipate
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
                // Queued moves belong to a track that is no longer playing.
                self.scheduler.reset();
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
                self.on_correction(correction, at);
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

    /// `at` is the instant the observation described, not the moment it arrived —
    /// the same pairing rule as everywhere else (spec §6.1).
    fn on_correction(&mut self, c: Correction, at: Instant) {
        match c {
            Correction::Seek { err } => {
                tracing::debug!(err, "seek detected");
                self.agreements = 0;
                // Everything queued was planned for a position we are no longer at.
                self.scheduler.reset();
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
                // Spec §10: resume, then wait for the next downbeat before full
                // moves. Deferred in M1 for want of a scheduler; reinstated here
                // now that there is something to delay.
                self.scheduler.resume_at_next_bar(self.clock.position(at));
                if self.clock.is_confident() && self.state != State::Locked {
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

    /// The row and cell to show at `now`, from the anticipation scheduler.
    ///
    /// `None` means nothing is scheduled — a non-`Locked` state, anticipation
    /// switched off, or a gap between moves. The caller falls back to
    /// [`Playback::grid_cell`] on the default row, which is M1's behaviour and
    /// keeps the dancer moving rather than freezing.
    pub fn frame(&mut self, now: Instant) -> Option<Frame> {
        if !self.anticipate || !self.state.is_grid_driven() {
            return None;
        }
        let score = self.clock.score()?.clone();
        let pos = self.clock.position(now);
        self.scheduler.plan(&score, pos);
        self.scheduler.frame_at(pos)
    }

    /// The move currently playing, for diagnostics.
    pub fn current_move(&self) -> Option<&dancer_choreo::ScheduledMove> {
        self.scheduler.current()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use dancer_score::{Score, ScoreSource, TrackId, SCHEMA};

    use super::*;

    /// Two rows: a quiet loop with its accent on cell 0, and a bounce whose accent
    /// is cell 3 — the one that makes anticipation observable.
    fn test_rows() -> Vec<RowInfo> {
        vec![
            RowInfo {
                index: 0,
                name: "idle".into(),
                cells: 8,
                beats_per_loop: 2,
                impact_cell: 0,
                pools: vec![],
                energy: Some(0.15),
                loopable: true,
                held: false,
            },
            RowInfo {
                index: 1,
                name: "bounce".into(),
                cells: 8,
                beats_per_loop: 1,
                impact_cell: 3,
                pools: vec![],
                energy: Some(0.55),
                loopable: true,
                held: false,
            },
        ]
    }

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
            beat_energy: vec![],
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

    /// Playback locked and past the first downbeat, so moves are scheduled.
    ///
    /// The wait matters: spec §10 holds full moves until the next bar after a
    /// resume, and the first position report is a resume. At 120 BPM in four,
    /// bars are 2 s apart, so 2.5 s is comfortably inside the second one.
    fn dancing(now: Instant) -> (Playback, Instant) {
        let p = locked(now);
        (p, now + Duration::from_millis(2500))
    }

    fn locked(now: Instant) -> Playback {
        let mut p = Playback::new(now, 0.0, test_rows(), 0, 42);
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
    fn anticipation_starts_the_move_before_the_beat() {
        // M3 end to end through the state machine: the impact cell must be on
        // screen at the target beat, which means the move began before it.
        let now = Instant::now();
        let mut p = locked(now);

        // Drive the loop as the app does, sampling every 8 ms.
        let mut first_bounce: Option<(f64, usize)> = None;
        for tick in 0..500u64 {
            let at = now + Duration::from_millis(tick * 8);
            if let Some(f) = p.frame(at) {
                if f.row == 1 && first_bounce.is_none() {
                    first_bounce = Some((tick as f64 * 0.008, f.cell));
                }
            }
        }

        let m = p.current_move().expect("a move should be scheduled");
        assert!(
            m.start_at < m.target_beat,
            "move starts at {} for a beat at {}",
            m.start_at,
            m.target_beat
        );
    }

    #[test]
    fn resuming_holds_moves_until_the_next_downbeat() {
        // Spec §10, deferred in M1 and reinstated in M4 now that there are moves
        // to hold back: starting mid-bar looks worse than a moment of idle.
        let now = Instant::now();
        let mut p = locked(now);
        assert!(
            p.frame(now).is_none(),
            "the bar in progress when playback started must not be joined mid-way"
        );
        assert!(
            p.frame(now + Duration::from_millis(2100)).is_some(),
            "but the next bar should pick up"
        );
        // Meanwhile the dancer is not frozen — the grid loop keeps it moving.
        assert!(p.grid_cell(now, 4, 8).is_some());
    }

    #[test]
    fn the_impact_cell_is_on_screen_at_the_target_beat() {
        let (mut p, start) = dancing(Instant::now());
        p.frame(start); // plan

        let m = p.current_move().cloned().expect("scheduled");
        let impact_cell = test_rows()[m.row].impact_cell;
        // Sample at the exact target beat. Media time and the test's base instant
        // coincide because the clock was anchored at 0 with no offset.
        let at = (start - Duration::from_millis(2500)) + Duration::from_secs_f64(m.target_beat);
        let f = p.frame(at).expect("a frame at the target beat");
        assert_eq!(
            f.cell, impact_cell as usize,
            "row {} should be showing cell {impact_cell} on its beat",
            m.row
        );
    }

    #[test]
    fn toggling_anticipation_falls_back_to_the_m1_loop() {
        // The A/B switch. With anticipation off, `frame` yields nothing and the
        // caller drives the default row off the grid, exactly as M1 did.
        let (mut p, at) = dancing(Instant::now());
        assert!(p.frame(at).is_some());

        assert!(!p.toggle_anticipation());
        assert!(p.frame(at).is_none(), "anticipation off means no scheduled frame");
        assert!(p.grid_cell(at, 4, 8).is_some(), "but the grid loop still runs");

        assert!(p.toggle_anticipation());
        // Back on, the next bar resumes moves.
        assert!(p.frame(at + Duration::from_secs(2)).is_some());
    }

    #[test]
    fn a_seek_discards_moves_planned_for_the_old_position() {
        let (mut p, now) = dancing(Instant::now());
        p.frame(now);
        assert!(p.current_move().is_some());

        p.apply(AppEvent::PositionReport {
            pos_secs: 90.0,
            playing: true,
            at: now + Duration::from_secs(1),
        });
        assert_eq!(p.state, State::Resync);
        assert!(
            p.current_move().is_none(),
            "moves planned around 0 s must not survive a jump to 90 s"
        );
    }

    #[test]
    fn low_confidence_score_stays_unscored() {
        let now = Instant::now();
        let mut p = Playback::new(now, 0.0, test_rows(), 0, 42);
        p.apply(AppEvent::TrackChanged { id: meta().id, meta: meta() });
        assert_eq!(p.state, State::Identifying);
        p.apply(AppEvent::ScoreReady { id: meta().id, score: score(0.5) });
        assert_eq!(p.state, State::Unscored);
        assert!(p.grid_cell(now, 4, 8).is_none(), "Unscored must not read the grid");
    }

    #[test]
    fn stale_score_for_an_old_track_is_ignored() {
        let now = Instant::now();
        let mut p = Playback::new(now, 0.0, test_rows(), 0, 42);
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
