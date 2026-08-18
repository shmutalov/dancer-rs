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

pub mod effort;
pub mod energy;
pub mod motif;
pub mod rng;
pub mod select;

pub use effort::{Articulation, Effort};
pub use energy::{EnergyProfile, Tier};
pub use motif::Motif;
pub use rng::Rng;
pub use select::{Context, RowInfo, ENERGY_WINDOW};

/// How far ahead moves are planned (spec §11.2).
pub const LOOKAHEAD: f64 = 2.0;

/// Energy rise across a bar that counts as a boundary.
const RISE_THRESHOLD: f32 = 0.15;

/// Bars a move is held before another is chosen.
///
/// Re-rolling every bar was the first implementation and it does not read as
/// dancing: a person picks something and does it for a phrase. Four bars is the
/// shortest unit that sounds like one in most popular music, and holding is
/// overridden the moment the music actually changes — a new energy tier, a drop,
/// or a run-up all cut the phrase short.
pub const PHRASE_BARS: u32 = 4;

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
    /// Whether a move's start is pulled back so its impact lands on the beat.
    ///
    /// Off is not "no scheduling" — it is the *same* choreography with the lead
    /// removed, which is the only comparison that isolates anticipation. See
    /// [`Scheduler::set_anticipate`].
    anticipate: bool,
    rng: Rng,
    /// Energy distribution of the track being scheduled, rebuilt when it changes.
    profile: EnergyProfile,
    /// How punctuated each bar of the same track is (spec §11.3).
    articulation: Articulation,
    profile_for: String,
    /// The move being held, and how many bars it has run for.
    phrase_row: Option<usize>,
    phrase_bars: u32,
    phrase_tier: Tier,
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
            anticipate: true,
            rng: Rng::new(seed),
            profile: EnergyProfile::default(),
            articulation: Articulation::default(),
            profile_for: String::new(),
            phrase_row: None,
            phrase_bars: 0,
            phrase_tier: Tier::Steady,
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

    /// Turn anticipation off or on, for the A/B in ROADMAP M3.
    ///
    /// # Why this is not simply "stop scheduling"
    ///
    /// It used to be. `Playback::frame` returned `None` with anticipation off and
    /// the caller fell back to looping the **default row** against the grid — so
    /// the A/B compared nine choreographed rows against one idle row. On the
    /// default sheet that read as a difference and the test looked fine; on FL
    /// Chan, whose default row is `Waiting` and moves three pixels, it reads as
    /// "dancing" versus "standing still". Either way it was measuring
    /// choreography, not anticipation.
    ///
    /// The comparison that isolates the thesis holds everything else fixed — same
    /// rows, same phrase, same loop rate — and changes only *when the loop starts*.
    /// With anticipation the move begins `impact_cell` frames early so its accent
    /// lands on the beat; without it the move begins on the beat and the accent
    /// arrives late by exactly that much.
    ///
    /// Render latency stays applied in both, because it corrects a different error
    /// and leaving it in one arm only would confound the thing under test.
    ///
    /// Rows with `impact_cell = 0` are identical either way. That is not a bug —
    /// three of FL Chan's nine rows have their accent on the first cell, and during
    /// those the A/B genuinely has nothing to show.
    pub fn set_anticipate(&mut self, on: bool) {
        if self.anticipate == on {
            return;
        }
        self.anticipate = on;
        // Queued moves carry a `start_at` computed under the old setting.
        self.reset();
    }

    pub fn anticipating(&self) -> bool {
        self.anticipate
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
        self.phrase_row = None;
        self.phrase_bars = 0;
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
        // Rebuilt per track, not per call: what counts as loud depends on the
        // whole recording, so it cannot be judged one beat at a time.
        if self.profile_for != score.track_id {
            self.profile = EnergyProfile::new(&score.beat_energy);
            self.articulation = Articulation::new(&score.beat_energy, score.meter);
            self.profile_for = score.track_id.clone();
            self.phrase_row = None;
            self.phrase_bars = 0;
        }

        let horizon = position + LOOKAHEAD;

        // The window starts a lookahead *behind* the position, not at it. Starting
        // exactly at `position` skips a bar beginning on this instant — the common
        // case at startup, where a track begins on a downbeat — and leaves the
        // dancer with nothing scheduled for its whole first bar. Reaching back also
        // picks up a move whose anticipation began before we arrived.
        //
        // **`planned_to` must never move backwards.** This compared against
        // `position` and reset to `position - LOOKAHEAD`, which drags the mark
        // backwards every time the position drifts past it — so bars already in the
        // queue were planned again, every frame, each with a freshly chosen random
        // row. `frame_at` then popped the lot and the sprite changed row every
        // 8 ms. It showed up as visual noise rather than as a wrong move.
        //
        // It survived because every test called `plan` at a fixed position, or at
        // two positions far apart — never advancing frame by frame, which is the
        // only way the mark falls behind. Tempo set the severity: with bars
        // narrower than `LOOKAHEAD` the mark jumps ahead of the position again
        // immediately and only one bar duplicates per bar, while at 2.48 s bars
        // against a 2 s window it never gets ahead and re-plans on every frame.
        // The user saw "normal, normal, weird" at 120 BPM and continuous noise at
        // 96.8 — the same bug at two severities.
        let earliest = position - LOOKAHEAD;
        if self.planned_to < earliest {
            self.planned_to = earliest;
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

        let row_idx = self.choose_for_phrase(&ctx);
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

        // The whole milestone, in one line — and the one line the A/B removes.
        let lead = if self.anticipate {
            row.impact_cell as f64 * frame_duration
        } else {
            0.0
        };

        Some(ScheduledMove {
            row: row.index,
            start_at: target - lead - self.render_latency,
            frame_duration,
            target_beat: target,
            cells: row.cells,
            loopable: row.loopable,
        })
    }

    /// Keep dancing the current move, or pick a new one.
    ///
    /// A person picks something and does it for a phrase; they do not redraw every
    /// two seconds. So a move is held for [`PHRASE_BARS`] unless the music gives a
    /// reason to change — the energy tier moves, a drop lands, or a run-up starts.
    /// Those overrides matter more than the holding: a phrase that ignored a drop
    /// would read as the dancer not listening.
    fn choose_for_phrase(&mut self, ctx: &Context) -> usize {
        let tier = ctx.tier;
        let must_change = ctx.boundary
            || ctx.building
            || tier != self.phrase_tier
            || self.phrase_bars >= PHRASE_BARS;

        if let Some(row) = self.phrase_row {
            if !must_change {
                self.phrase_bars += 1;
                return row;
            }
        }

        // A new phrase should not be blocked from repeating the previous move
        // when that is genuinely the best fit — the no-repeat rule exists to stop
        // the *same bar* recurring, not to forbid a section from coming back.
        let ctx = Context {
            previous: if tier == self.phrase_tier { ctx.previous } else { None },
            ..ctx.clone()
        };
        let row = select::choose(&self.rows, &ctx, self.fallback, &mut self.rng);
        self.phrase_row = Some(row);
        self.phrase_bars = 1;
        self.phrase_tier = tier;
        row
    }

    /// What the music is doing around `beat`, for selection.
    fn context(&self, score: &Score, beat: usize) -> Context {
        let t = score.beats[beat];
        let m = score.meter.max(1) as usize;

        // Ranked within the track, not taken raw. Raw values are ratios against
        // the track's own loudest moment and cluster in the top half of the scale,
        // which made every passage look energetic — see `energy`.
        let raw = score.energy_at(t);
        let energy = raw.and_then(|e| self.profile.rank(e)).or(raw);
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
            // Judged against the tier already in force, so a bar hovering on a
            // boundary does not flip band and cut the phrase (see `energy`).
            tier: raw.map_or(Tier::Steady, |e| self.profile.tier_from(e, self.phrase_tier)),
            articulation: self.articulation.rank_at(beat),
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
                // Keeping time and nothing more: the move a person actually does
                // through a quiet passage.
                motifs: vec![Motif::Step, Motif::Gesture],
                effort_time: None,
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
                motifs: vec![Motif::Step, Motif::Sink],
                effort_time: Some(Effort::Sudden),
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
                motifs: vec![Motif::Turn],
                effort_time: None,
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
    fn without_anticipation_a_move_starts_on_its_beat() {
        // The other arm of the M3 A/B. Same rows, same phrase, same loop rate —
        // only the lead is gone, so the accent arrives late by exactly the amount
        // anticipation would have removed.
        let s = score(0.55);
        let mut sch = sched();
        sch.set_anticipate(false);
        sch.plan(&s, 0.0);

        for m in &sch.queue {
            assert!(
                (m.start_at - m.target_beat).abs() < 1e-9,
                "row {} starts at {} for a beat at {}",
                m.row,
                m.start_at,
                m.target_beat
            );
        }
    }

    #[test]
    fn the_two_arms_differ_only_in_phase() {
        // What makes this a valid A/B: identical choreography, identical timing,
        // and a start offset of exactly `impact_cell` frames. If the arms differed
        // in which rows they picked, the test would be measuring choreography —
        // which is precisely what it did before, by falling back to the default row.
        let s = score(0.55);

        let plan = |anticipate: bool| {
            let mut sch = sched();
            sch.set_anticipate(anticipate);
            sch.plan(&s, 0.0);
            sch.queue.iter().cloned().collect::<Vec<_>>()
        };
        let with = plan(true);
        let without = plan(false);

        assert_eq!(with.len(), without.len(), "same number of moves");
        for (a, b) in with.iter().zip(&without) {
            assert_eq!(a.row, b.row, "same row chosen");
            assert_eq!(a.target_beat, b.target_beat, "same beat targeted");
            assert_eq!(a.cells, b.cells);
            assert!((a.frame_duration - b.frame_duration).abs() < 1e-12);

            let lead = rows()[a.row].impact_cell as f64 * a.frame_duration;
            assert!(
                (b.start_at - a.start_at - lead).abs() < 1e-9,
                "row {} should lead by {lead}s, got {}",
                a.row,
                b.start_at - a.start_at
            );
        }
        // And at least one row must actually differ, or the assertion above is
        // satisfied by an all-zero `impact_cell` sheet and proves nothing.
        assert!(
            with.iter().zip(&without).any(|(a, b)| a.start_at < b.start_at),
            "no row had a lead to remove"
        );
    }

    #[test]
    fn toggling_anticipation_replans_rather_than_leaving_stale_starts() {
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        assert!(sch.queued() > 0);

        sch.set_anticipate(false);
        assert_eq!(sch.queued(), 0, "queued moves carry the old lead");

        // Setting it to what it already is must not throw away a good queue.
        sch.plan(&s, 0.0);
        let n = sch.queued();
        sch.set_anticipate(false);
        assert_eq!(sch.queued(), n);
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
        let sch = sched();
        // Bar starting at beat 16 = t 8.0.
        let ctx = sch.context(&s, 16);
        assert!(ctx.boundary, "a 0.7 rise across a bar should read as a drop");
        assert!(!ctx.building, "arrival is not a run-up");

        // The bar before it is the wind-up.
        let ctx = sch.context(&s, 12);
        assert!(ctx.building);
        assert!(!ctx.boundary);
    }

    /// A score whose bars are wider than the lookahead window.
    ///
    /// 96.8 BPM in four is a 2.48 s bar against a 2 s `LOOKAHEAD`. The fixtures
    /// were all 120 BPM, whose 2.0 s bar sits exactly on the boundary — which is
    /// why the re-planning bug survived every test until a real track hit it.
    fn slow_score() -> Score {
        let interval = 60.0 / 96.8;
        let beats: Vec<f64> = (0..400).map(|i| i as f64 * interval).collect();
        Score {
            bpm: 96.8,
            beat_positions: (0..400).map(|i| (i % 4 + 1) as u8).collect(),
            downbeats: beats.iter().copied().step_by(4).collect(),
            beat_energy: vec![0.55; beats.len()],
            beats,
            ..score(0.55)
        }
    }

    /// A track that is loud throughout but has a genuinely quiet passage —
    /// the shape that produced "it spins during the silent beats".
    fn dynamic_score() -> Score {
        let mut s = score(0.55);
        // Raw values as the analyzer produces them: ratios against the track's own
        // p95, so even the calm section reads as 0.6 in absolute terms.
        s.beat_energy = (0..360)
            .map(|i| if (40..80).contains(&i) { 0.60 } else { 0.95 })
            .collect();
        s
    }

    #[test]
    fn a_quiet_passage_gets_a_calm_move() {
        // The complaint, as a test: a human keeps time with their feet through
        // the quiet part rather than spinning.
        let s = dynamic_score();
        let mut sch = sched();
        // Bar starting at beat 40 is inside the calm section.
        sch.plan(&s, 0.0);
        let ctx = sch.context(&s, 40);

        assert_eq!(ctx.tier, Tier::Calm, "ranked {:?}", ctx.energy);
        let chosen = select::choose(&rows(), &ctx, 0, &mut Rng::new(1));
        assert_eq!(chosen, 0, "should pick idle, got {}", rows()[chosen].name);
    }

    #[test]
    fn no_turn_is_ever_scheduled_through_the_quiet_passage() {
        // The same complaint checked across the whole calm section rather than at
        // one bar, because "it spins during the silent beats" means a turn appearing
        // *somewhere* in it — which a single-bar assertion can pass right through.
        // The spin row here is scored 0.9, but it is the Motif that excludes it:
        // a turn is too big an action for a calm passage at any energy.
        let s = dynamic_score();
        let mut sch = sched();
        sch.plan(&s, 0.0);

        const SPIN: usize = 2;
        for beat in (40..80).step_by(4) {
            let ctx = sch.context(&s, beat);
            assert_eq!(ctx.tier, Tier::Calm, "beat {beat} ranked {:?}", ctx.energy);
            assert_ne!(sch.choose_for_phrase(&ctx), SPIN, "a turn at beat {beat}");
        }
    }

    #[test]
    fn the_scheduler_measures_articulation_alongside_energy() {
        // Both interpretations are rebuilt per track and reach selection through
        // the context; neither is stored in the score.
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);
        assert!(sch.context(&s, 8).articulation.is_some());

        let mut flat = score(0.55);
        flat.beat_energy.clear();
        flat.track_id = "test:no-energy".into();
        sch.plan(&flat, 0.0);
        assert!(sch.context(&flat, 8).articulation.is_none(), "nothing measured");
    }

    #[test]
    fn the_ordinary_passage_is_not_treated_as_a_climax() {
        // 89 % of this track sits at one level, so that level is *ordinary*. The
        // dancer should not spend the whole song at full tilt just because the
        // recording is loud in absolute terms — that was half the complaint.
        let s = dynamic_score();
        let mut sch = sched();
        sch.plan(&s, 0.0);
        let ctx = sch.context(&s, 100);
        assert_eq!(ctx.tier, Tier::Steady, "ranked {:?}", ctx.energy);

        // Cleared: `plan` above left a row in `previous`, and the no-repeat rule
        // would then be deciding this rather than the tier. What is under test is
        // how an ordinary energy level is *interpreted*.
        let ctx = Context { previous: None, ..ctx };

        // Sampled rather than drawn once. Selection is weighted random, so a single
        // draw asserts something about one seed rather than about the behaviour —
        // this assertion used to pass only because a third row happened to sit
        // between the right answer and the wrong one in the weighting.
        let mut rng = Rng::new(1);
        let mut counts = [0usize; 3];
        for _ in 0..500 {
            counts[select::choose(&rows(), &ctx, 0, &mut rng)] += 1;
        }

        assert!(counts[1] > counts[0] * 5, "the middle move should dominate: {counts:?}");
        assert_eq!(counts[2], 0, "and a turn is too big for an ordinary bar: {counts:?}");
    }

    #[test]
    fn a_move_is_held_for_a_phrase_rather_than_redrawn_every_bar() {
        // Re-rolling every bar is what read as "weird against the music".
        let s = score(0.55);
        let mut sch = sched();
        sch.plan(&s, 0.0);

        let mut rows_seen = Vec::new();
        for bar in 0..16 {
            // Bars are 4 beats apart in this fixture.
            let ctx = sch.context(&s, bar * 4);
            rows_seen.push(sch.choose_for_phrase(&ctx));
        }

        // Sixteen bars of unchanging music should be four phrases, not sixteen.
        let changes = rows_seen.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(changes <= 4, "{changes} changes in 16 bars: {rows_seen:?}");
        assert_eq!(rows_seen[0], rows_seen[1], "a phrase must last past one bar");
    }

    #[test]
    fn a_drop_cuts_the_phrase_short() {
        // Holding must never mean ignoring the music.
        let mut sch = sched();
        let calm = Context { tier: Tier::Steady, energy: Some(0.5), ..Default::default() };
        let first = sch.choose_for_phrase(&calm);
        assert_eq!(sch.choose_for_phrase(&calm), first, "held");

        let drop = Context {
            tier: Tier::Loud,
            energy: Some(0.95),
            boundary: true,
            ..Default::default()
        };
        let after = sch.choose_for_phrase(&drop);
        assert_ne!(after, first, "a drop should change the move mid-phrase");
        assert_eq!(sch.phrase_bars, 1, "and start a new phrase");
    }

    #[test]
    fn advancing_frame_by_frame_does_not_replan_bars() {
        // The "noise" failure, as reported: rows changing many times a second
        // instead of once a bar. Drive the scheduler exactly as the app does —
        // plan then read, every 8 ms — and count how often the move changes.
        let s = slow_score();
        let mut sch = sched();

        let mut changes = 0;
        let mut last: Option<f64> = None;
        for tick in 0..1250u32 {
            let pos = tick as f64 * 0.008; // 10 seconds
            sch.plan(&s, pos);
            sch.frame_at(pos);
            let target = sch.current().map(|m| m.target_beat);
            if target != last {
                changes += 1;
                last = target;
            }
        }

        // Ten seconds of 2.48 s bars is four moves, plus the initial one.
        assert!(changes <= 6, "{changes} move changes in 10 s — should be one per bar");
        // And the queue must not grow without bound while that happens.
        assert!(sch.queued() <= 3, "queued {} moves for a 2 s window", sch.queued());
    }

    #[test]
    fn planned_to_never_moves_backwards() {
        // The invariant underneath the bug above, stated directly.
        let s = slow_score();
        let mut sch = sched();
        let mut mark = f64::NEG_INFINITY;
        for tick in 0..600u32 {
            sch.plan(&s, tick as f64 * 0.01);
            assert!(sch.planned_to >= mark, "planned_to went backwards");
            mark = sch.planned_to;
        }
    }

    #[test]
    fn every_bar_is_planned_exactly_once() {
        let s = slow_score();
        let mut sch = sched();
        let mut targets = Vec::new();
        for tick in 0..1250u32 {
            let pos = tick as f64 * 0.008;
            sch.plan(&s, pos);
            // Drain whatever became current, recording each distinct move.
            sch.frame_at(pos);
            if let Some(m) = sch.current() {
                if targets.last() != Some(&m.target_beat) {
                    targets.push(m.target_beat);
                }
            }
        }
        let mut sorted = targets.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        sorted.dedup();
        assert_eq!(targets.len(), sorted.len(), "a bar was scheduled twice: {targets:?}");
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
