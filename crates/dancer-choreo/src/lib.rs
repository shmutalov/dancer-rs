//! Anticipation scheduling (spec §11.2) — the thing the project exists for.
//!
//! Everything else is plumbing to feed this. A dancer that *reacts* to the beat is
//! always late by however long it takes to notice; a dancer that **anticipates** it
//! starts the move early so the accent lands on time.
//!
//! ```text
//! start_at = target_beat − (impact_cell × frame_duration) − render_latency
//! ```
//!
//! `impact_cell` is the artwork's accent — knees deepest, arm fully raised — and it
//! is what has to coincide with the beat. Everything before it is the wind-up, and
//! the wind-up has to happen *before* the beat or it is not a wind-up. On the
//! default sheet the bounce row has `impact_cell = 3`, so at 120 BPM its move
//! begins about 190 ms ahead of the downbeat it lands on.
//!
//! # Why this is scheduled rather than computed per frame
//!
//! M1 derived the cell from grid position each frame, which cannot drift. That
//! works because a loop is a pure function of phase. Anticipation is not: a move
//! that starts before its target beat needs the *decision* made before then, and
//! the decision is random (§11.3). So moves are planned into a queue over a
//! lookahead window, and each frame only reads from it.
//!
//! Drift is still impossible: `start_at` is media-time, and the cell is
//! `floor((position − start_at) / frame_duration)`. A dropped frame skips a cell.

use std::collections::VecDeque;

use dancer_score::Score;

pub mod rng;
pub mod select;

pub use rng::Rng;
pub use select::{Context, RowInfo, ENERGY_WINDOW};

/// How far ahead moves are planned (spec §11.2).
pub const LOOKAHEAD: f64 = 2.0;

/// Energy rise across a bar that counts as a boundary.
const RISE_THRESHOLD: f32 = 0.15;

/// One planned move.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduledMove {
    pub row: usize,
    /// Media-time second at which cell 0 shows. **Earlier than `target_beat`.**
    pub start_at: f64,
    pub frame_duration: f64,
    /// The beat the impact cell is meant to land on.
    pub target_beat: f64,
    pub cells: usize,
    pub loopable: bool,
}

impl ScheduledMove {
    /// When the impact cell is displayed. Should equal `target_beat`.
    pub fn impact_at(&self, impact_cell: u32) -> f64 {
        self.start_at + impact_cell as f64 * self.frame_duration
    }

    pub fn ends_at(&self) -> f64 {
        self.start_at + self.cells as f64 * self.frame_duration
    }
}

/// What the dancer should be showing right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    pub row: usize,
    pub cell: usize,
}

pub struct Scheduler {
    rows: Vec<RowInfo>,
    fallback: usize,
    queue: VecDeque<ScheduledMove>,
    current: Option<ScheduledMove>,
    previous_row: Option<usize>,
    /// Media time up to which bars have been planned.
    planned_to: f64,
    render_latency: f64,
    rng: Rng,
}

impl Scheduler {
    pub fn new(rows: Vec<RowInfo>, fallback: usize, seed: u64) -> Self {
        Self {
            rows,
            fallback,
            queue: VecDeque::new(),
            current: None,
            previous_row: None,
            planned_to: f64::NEG_INFINITY,
            render_latency: 0.0,
            rng: Rng::new(seed),
        }
    }

    /// Set the measured display latency (spec §11.2).
    ///
    /// Only the part observable from inside the process — see
    /// `dancer_render::LatencyMonitor`. Whatever the compositor adds on top is a
    /// constant, and a constant is exactly what §9.2's offset slider absorbs.
    pub fn set_render_latency(&mut self, secs: f64) {
        self.render_latency = secs.clamp(0.0, 0.1);
    }

    pub fn render_latency(&self) -> f64 {
        self.render_latency
    }

    pub fn rows(&self) -> &[RowInfo] {
        &self.rows
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    pub fn current(&self) -> Option<&ScheduledMove> {
        self.current.as_ref()
    }

    /// Drop everything planned. For track changes and seeks.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.current = None;
        self.previous_row = None;
        self.planned_to = f64::NEG_INFINITY;
    }

    /// Drop everything and schedule nothing until the next bar starts.
    ///
    /// Spec §10: on resume, wait for the next downbeat before resuming full moves
    /// — "starting mid-bar looks worse than a half-second of idle". Until then
    /// `frame_at` yields nothing and the caller falls back to the default row,
    /// which is the settling behaviour the same section asks for.
    pub fn resume_at_next_bar(&mut self, position: f64) {
        self.queue.clear();
        self.current = None;
        self.previous_row = None;
        // Unlike `reset`, this does *not* reach back: only bars strictly after
        // now are planned, so the bar already in progress is skipped.
        self.planned_to = position;
    }

    /// Plan any bars starting within the lookahead window.
    ///
    /// Idempotent per bar: calling this every frame is the intended usage.
    pub fn plan(&mut self, score: &Score, position: f64) {
        let horizon = position + LOOKAHEAD;
        // On the first call, and after a seek, start a window *behind* the current
        // position rather than at it. Starting exactly at `position` skips a bar
        // beginning on this instant — which is the common case at startup, where
        // the track begins on a downbeat — and leaves the dancer with nothing
        // scheduled until the next bar. Reaching back also picks up a move whose
        // anticipation began before we arrived; it plays from partway through,
        // which is what it would have been doing anyway.
        if self.planned_to < position {
            self.planned_to = position - LOOKAHEAD;
        }

        for i in 0..score.beats.len() {
            let t = score.beats[i];
            if t <= self.planned_to {
                continue;
            }
            if t > horizon {
                break;
            }
            // Moves change on downbeats only (spec §11.3), against the *fitted*
            // bar phase from M2 rather than raw downbeat detections — those are
            // the least reliable part of the analysis, and a spurious one here
            // reads as the dancer stumbling half a bar early.
            if score.bar_beat(i) != 1 {
                continue;
            }

            if let Some(m) = self.plan_bar(score, i) {
                self.previous_row = Some(m.row);
                self.queue.push_back(m);
            }
            self.planned_to = t;
        }
    }

    fn plan_bar(&mut self, score: &Score, beat: usize) -> Option<ScheduledMove> {
        let target = *score.beats.get(beat)?;
        let ctx = self.context(score, beat);
        let row_idx = select::choose(&self.rows, &ctx, self.fallback, &mut self.rng);
        let row = self.rows.get(row_idx).or_else(|| self.rows.get(self.fallback))?;

        // Local beat interval across the loop, not the global BPM (spec §11.1).
        // Tracks drift, and live recordings drift a lot.
        let b = row.beats_per_loop.max(1) as usize;
        let end = (beat + b).min(score.beats.len().saturating_sub(1));
        let span = if end > beat {
            (score.beats[end] - target) / (end - beat) as f64 * b as f64
        } else {
            score.interval_at(beat) * b as f64
        };
        let frame_duration = span / row.cells.max(1) as f64;

        Some(ScheduledMove {
            row: row.index,
            // The whole milestone, in one line.
            start_at: target - row.impact_cell as f64 * frame_duration - self.render_latency,
            frame_duration,
            target_beat: target,
            cells: row.cells,
            loopable: row.loopable,
        })
    }

    /// What the music is doing around `beat`, for selection.
    fn context(&self, score: &Score, beat: usize) -> Context {
        let t = score.beats[beat];
        let m = score.meter.max(1) as usize;

        let energy = score.energy_at(t);
        let here = bar_energy(score, beat, m);
        let prev = beat.checked_sub(m).and_then(|i| bar_energy(score, i, m));
        let next = bar_energy(score, beat + m, m);

        // Explicit cues win when the score has them; §8.2 sidecar scores do.
        let cue = score
            .cues
            .iter()
            .find(|c| (c.time - t).abs() < score.interval_at(beat) / 2.0);

        let boundary = cue.is_some_and(|c| c.kind == "drop")
            || score.segments.iter().any(|s| (s.start - t).abs() < 1e-6)
            || matches!((here, prev), (Some(h), Some(p)) if h - p > RISE_THRESHOLD);

        let building = cue.is_some_and(|c| c.kind == "build")
            || matches!((here, next), (Some(h), Some(n)) if n - h > RISE_THRESHOLD);

        Context {
            energy,
            label: score.segment_at(t).map(|s| s.label.clone()),
            // A bar cannot be both the run-up and the arrival; arriving wins.
            building: building && !boundary,
            boundary,
            previous: self.previous_row,
        }
    }

    /// The frame to show at `position`, or `None` to fall back to the default row.
    ///
    /// `None` means "nothing is scheduled here" — before the first bar, after a
    /// one-shot finishes, or past the end of the grid. The caller keeps dancing;
    /// it just does so on M1's grid-derived loop.
    pub fn frame_at(&mut self, position: f64) -> Option<Frame> {
        while self.queue.front().is_some_and(|m| m.start_at <= position) {
            // A move that starts before the previous one finishes simply takes
            // over. That overlap is not a bug — it is what anticipation *is*.
            self.current = self.queue.pop_front();
        }

        let m = self.current.as_ref()?;
        if position < m.start_at || m.frame_duration <= 0.0 {
            return None;
        }

        let elapsed = ((position - m.start_at) / m.frame_duration) as usize;
        if elapsed >= m.cells {
            if !m.loopable {
                // Spec §11.3: non-loopable rows return to the default row.
                return None;
            }
            // Keep looping until the next move takes over.
            return Some(Frame {
                row: m.row,
                cell: elapsed % m.cells,
            });
        }
        Some(Frame {
            row: m.row,
            cell: elapsed,
        })
    }
}

/// Mean energy across the bar starting at `beat`.
fn bar_energy(score: &Score, beat: usize, meter: usize) -> Option<f32> {
    if score.beat_energy.is_empty() || beat >= score.beat_energy.len() {
        return None;
    }
    let end = (beat + meter).min(score.beat_energy.len());
    let slice = &score.beat_energy[beat..end];
    (!slice.is_empty()).then(|| slice.iter().sum::<f32>() / slice.len() as f32)
}

#[cfg(test)]
mod tests {
    use dancer_score::{ScoreSource, SCHEMA};

    use super::*;

    fn rows() -> Vec<RowInfo> {
        vec![
            RowInfo {
                index: 0,
                name: "idle".into(),
                cells: 8,
                beats_per_loop: 2,
                impact_cell: 0,
                pools: vec!["idle".into()],
                energy: Some(0.15),
                loopable: true,
                held: false,
            },
            RowInfo {
                index: 1,
                name: "bounce".into(),
                cells: 8,
                beats_per_loop: 1,
                // Knees deepest here: the cell that must land on the beat.
                impact_cell: 3,
                pools: vec!["verse".into()],
                energy: Some(0.55),
                loopable: true,
                held: false,
            },
            RowInfo {
                index: 2,
                name: "spin".into(),
                cells: 8,
                beats_per_loop: 2,
                impact_cell: 4,
                pools: vec!["chorus".into()],
                energy: Some(0.9),
                loopable: true,
                held: false,
            },
        ]
    }

    /// 120 BPM, 4/4, three minutes. Beat every 0.5 s, bar every 2 s.
    fn score(energy: f32) -> Score {
        let beats: Vec<f64> = (0..360).map(|i| i as f64 * 0.5).collect();
        Score {
            schema: SCHEMA,
            track_id: "test:x".into(),
            duration_ms: 180_000,
            bpm: 120.0,
            meter: 4,
            source: ScoreSource::BeatThis,
            confidence: 0.9,
            analyzed_at: String::new(),
            beat_positions: (0..360).map(|i| (i % 4 + 1) as u8).collect(),
            downbeats: beats.iter().copied().step_by(4).collect(),
            beat_energy: vec![energy; beats.len()],
            beats,
            segments: vec![],
            cues: vec![],
        }
    }

    fn sched() -> Scheduler {
        Scheduler::new(rows(), 0, 42)
    }

    #[test]
    fn the_impact_cell_lands_on_the_beat() {
        // The entire thesis, as an assertion.
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);

        let m = sch.queue.front().unwrap();
        let row = &rows()[m.row];
        assert!(
            (m.impact_at(row.impact_cell) - m.target_beat).abs() < 1e-9,
            "impact at {} but target beat {}",
            m.impact_at(row.impact_cell),
            m.target_beat
        );
    }

    #[test]
    fn a_move_starts_before_the_beat_it_lands_on() {
        // Anticipation, stated the other way round: if the move started *on* the
        // beat, the accent would arrive late and the whole premise would fail.
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);

        for m in &sch.queue {
            let impact = rows()[m.row].impact_cell;
            if impact > 0 {
                assert!(
                    m.start_at < m.target_beat,
                    "row {} starts at {} for a beat at {}",
                    m.row,
                    m.start_at,
                    m.target_beat
                );
            }
        }
    }

    #[test]
    fn render_latency_shifts_the_start_earlier_still() {
        let s = score(0.55);
        let mut a = sched();
        a.plan(&s, 0.0);
        let without = a.queue.front().unwrap().start_at;

        let mut b = sched();
        b.set_render_latency(0.016);
        b.plan(&s, 0.0);
        let with = b.queue.front().unwrap().start_at;

        assert!((without - with - 0.016).abs() < 1e-9);
    }

    #[test]
    fn frame_timing_comes_from_local_intervals_not_global_bpm() {
        // A grid that slows down, with a `bpm` field that is wrong everywhere but
        // the start — exactly the drifting-128 fixture's shape.
        let mut s = score(0.55);
        s.beats = (0..64)
            .scan(0.0, |t, i| {
                let out = *t;
                *t += 0.5 + i as f64 * 0.002;
                Some(out)
            })
            .collect();
        s.beat_positions = (0..64).map(|i| (i % 4 + 1) as u8).collect();
        s.beat_energy = vec![0.55; 64];
        s.bpm = 120.0;

        // One row, so the comparison cannot be confounded by two moves with
        // different `beats_per_loop` — which is what makes frame durations differ
        // for reasons that have nothing to do with tempo.
        let only = vec![RowInfo { index: 0, ..rows()[1].clone() }];
        let mut sch = Scheduler::new(only, 0, 42);

        sch.plan(&s, 0.0);
        let early = sch.queue.front().unwrap().frame_duration;
        sch.plan(&s, 30.0);
        let late = sch.queue.back().unwrap().frame_duration;

        assert!(
            late > early * 1.05,
            "frame duration should follow the slowing tempo: {early} then {late}"
        );
    }

    #[test]
    fn meter_is_respected_rather_than_assumed_to_be_four() {
        // 3/4 waltz: bars every three beats, so moves change every three.
        let mut s = score(0.55);
        s.meter = 3;
        s.beat_positions = (0..360).map(|i| (i % 3 + 1) as u8).collect();

        let mut sch = sched();
        sch.plan(&s, 0.0);
        let starts: Vec<f64> = sch.queue.iter().map(|m| m.target_beat).collect();
        for w in starts.windows(2) {
            assert!((w[1] - w[0] - 1.5).abs() < 1e-9, "bars should be 1.5 s apart: {starts:?}");
        }
    }

    #[test]
    fn cells_advance_through_the_move() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        let m = sch.queue.front().unwrap().clone();

        let f0 = sch.frame_at(m.start_at).unwrap();
        assert_eq!(f0.cell, 0);
        let f1 = sch.frame_at(m.start_at + m.frame_duration * 1.5).unwrap();
        assert_eq!(f1.cell, 1);
        let f2 = sch.frame_at(m.start_at + m.frame_duration * 7.5).unwrap();
        assert_eq!(f2.cell, 7);
    }

    #[test]
    fn a_dropped_frame_skips_a_cell_rather_than_shifting_phase() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        let m = sch.queue.front().unwrap().clone();

        // Jump straight to the middle of the move, as a stalled loop would.
        let f = sch.frame_at(m.start_at + m.frame_duration * 5.5).unwrap();
        assert_eq!(f.cell, 5, "phase is read, not accumulated");
    }

    #[test]
    fn a_one_shot_returns_to_the_default_row() {
        let mut rs = rows();
        rs[1].loopable = false;
        let s = score(0.55);
        let mut sch = Scheduler::new(rs, 0, 42);
        sch.plan(&s, 0.0);

        // Find a non-loopable move and run past its end.
        let m = sch.queue.iter().find(|m| !m.loopable).cloned();
        if let Some(m) = m {
            while sch.queue.front().is_some_and(|q| q.start_at < m.start_at) {
                sch.queue.pop_front();
            }
            assert!(sch.frame_at(m.start_at).is_some());
            assert!(
                sch.frame_at(m.ends_at() + 1e-6).is_none(),
                "a one-shot should hand back to the default row"
            );
        }
    }

    #[test]
    fn a_loopable_move_keeps_going_until_the_next_one() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        let m = sch.queue.front().unwrap().clone();
        assert!(m.loopable);
        let f = sch.frame_at(m.ends_at() + m.frame_duration * 0.5).unwrap();
        assert_eq!(f.row, m.row);
        assert_eq!(f.cell, 0, "wraps rather than stopping");
    }

    #[test]
    fn planning_is_idempotent_and_bounded() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        let n = sch.queued();
        sch.plan(&s, 0.0);
        sch.plan(&s, 0.0);
        assert_eq!(sch.queued(), n, "replanning must not duplicate bars");
        // 2 s lookahead over 2 s bars: a small handful, not the whole track.
        assert!(n <= 3, "queued {n} moves for a 2 s window");
    }

    #[test]
    fn planning_advances_with_position() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        let first = sch.queued();
        sch.plan(&s, 10.0);
        assert!(sch.queued() > first, "later positions should plan later bars");
    }

    #[test]
    fn reset_clears_everything() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        sch.frame_at(sch.queue.front().unwrap().start_at);
        sch.reset();
        assert_eq!(sch.queued(), 0);
        assert!(sch.current().is_none());
        assert!(sch.frame_at(1.0).is_none());
    }

    #[test]
    fn resuming_waits_for_the_next_bar() {
        // Spec §10: starting mid-bar looks worse than a half-second of idle.
        let s = score(0.55);
        let mut sch = sched();

        // Resume partway through the bar that starts at 4.0 s.
        sch.resume_at_next_bar(5.0);
        sch.plan(&s, 5.0);
        assert!(
            sch.frame_at(5.0).is_none(),
            "nothing should play out the bar we resumed inside"
        );
        let next = sch.queue.front().expect("the following bar should be planned");
        assert!(next.target_beat >= 6.0, "target {} should be a later bar", next.target_beat);

        // And it does start on that bar.
        assert!(sch.frame_at(next.target_beat).is_some());
    }

    #[test]
    fn moves_change_on_downbeats_only() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        for m in &sch.queue {
            // Every target is a bar start: 0, 2, 4 s at 120 BPM in four.
            assert!((m.target_beat / 2.0).fract().abs() < 1e-9, "{}", m.target_beat);
        }
    }

    #[test]
    fn an_energy_rise_is_treated_as_a_boundary() {
        // No segments, no cues — everything derived from beat energy, which is the
        // normal case for a beat-this score.
        let mut s = score(0.2);
        for e in s.beat_energy[16..].iter_mut() {
            *e = 0.9;
        }
        let mut sch = sched();
        // Bar starting at beat 16 = t 8.0.
        let ctx = sch.context(&s, 16);
        assert!(ctx.boundary, "a 0.7 rise across a bar should read as a drop");
        assert!(!ctx.building, "arrival is not a run-up");

        // The bar before it is the wind-up.
        let ctx = sch.context(&s, 12);
        assert!(ctx.building);
        assert!(!ctx.boundary);
    }

    #[test]
    fn a_bar_starting_exactly_at_the_current_position_is_planned() {
        // Regression: `plan` used to skip it, so a track beginning on a downbeat
        // had nothing scheduled for its whole first bar.
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        assert!(
            sch.queue.iter().any(|m| m.target_beat == 0.0),
            "the bar at t=0 should be scheduled"
        );
    }

    #[test]
    fn explicit_cues_take_priority_over_derived_ones() {
        let mut s = score(0.5);
        s.cues = vec![dancer_score::Cue { time: 4.0, kind: "drop".into(), bars: 0 }];
        let sch = sched();
        assert!(sch.context(&s, 8).boundary, "an explicit drop at 4.0 s");
        assert!(!sch.context(&s, 16).boundary, "flat energy elsewhere");
    }

    #[test]
    fn a_score_without_energy_still_schedules() {
        // beat_energy is empty whenever nothing measured it.
        let mut s = score(0.5);
        s.beat_energy.clear();
        let mut sch = sched();
        sch.plan(&s, 0.0);
        assert!(sch.queued() > 0);
        assert!(sch.frame_at(sch.queue.front().unwrap().start_at).is_some());
    }
}
