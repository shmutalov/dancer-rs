//! Move selection (spec §11.3).
//!
//! Given a moment in the track, which row should dance through the next bar.
//!
//! Spec §11.3 filters rows by whether their `pools` contain the segment label —
//! but scores from §8.1 have no labels, and those are most scores. So selection
//! keys on **energy tier and boundary position**, with labels treated as
//! enrichment when they happen to be there. This is the mechanism that lets the
//! project ship without segmentation: the scheduler needs to know that energy rose
//! at a downbeat, not that the section is called a chorus.
//!
//! Energy alone turned out to be too thin an axis. It cannot tell a small gesture
//! from a small travelling step, and nothing in it stopped a full spin being chosen
//! for a quiet bar as long as its declared number landed in the window. Two further
//! filters come from Laban: [`crate::motif`] gates *what kind of action* each tier
//! admits, and [`crate::effort`] weights rows by whether their Time Effort suits how
//! punctuated the bar is. Both degrade to nothing on an untagged sheet.

use crate::effort::{self, Effort};
use crate::energy::Tier;
use crate::motif::{self, Motif};
use crate::rng::Rng;

/// What a sheet row offers the scheduler. Mirrors the manifest (spec §4.2), kept
/// separate so this crate does not depend on sprite loading.
#[derive(Debug, Clone, PartialEq)]
pub struct RowInfo {
    pub index: usize,
    pub name: String,
    pub cells: usize,
    /// Beats one pass through the row occupies, relative to the score's meter.
    pub beats_per_loop: u32,
    /// The cell that must land *on* the beat (spec §11.2).
    pub impact_cell: u32,
    pub pools: Vec<String>,
    pub energy: Option<f32>,
    /// What the move *is*, in Motif vocabulary (spec §11.3). Empty when the sheet
    /// says nothing, which every inherited sheet does.
    pub motifs: Vec<Motif>,
    /// The row's Time Effort, when its artwork clearly has one.
    pub effort_time: Option<Effort>,
    pub loopable: bool,
    /// Excluded from selection: this is the drag pose, not a dance.
    pub held: bool,
}

impl RowInfo {
    fn selectable(&self) -> bool {
        !self.held && self.cells > 0
    }
}

/// How far a row's energy may sit from the music's before it stops being a
/// candidate (spec §11.3).
pub const ENERGY_WINDOW: f32 = 0.35;

/// What the music is doing at the moment being scheduled.
#[derive(Debug, Clone)]
pub struct Context {
    /// Energy at the target beat, **ranked within the track** (see `energy`).
    pub energy: Option<f32>,
    /// Coarse band of the same value, for deciding whether to change move at all.
    pub tier: Tier,
    /// How punctuated this bar is against the rest of the track, `0.0..1.0`
    /// (see [`crate::effort`]). Low is flowing, high is stabbing.
    pub articulation: Option<f32>,
    /// Segment label, when the score has segments. Usually `None`.
    pub label: Option<String>,
    /// Energy is about to rise: this bar is a run-up (spec §11.3's `build`).
    pub building: bool,
    /// Energy just rose, or a new segment starts here (spec §11.3's `drop`).
    pub boundary: bool,
    /// Row used in the previous bar, excluded to avoid immediate repeats.
    pub previous: Option<usize>,
}

impl Default for Context {
    fn default() -> Self {
        Self {
            energy: None,
            tier: Tier::Steady,
            articulation: None,
            label: None,
            building: false,
            boundary: false,
            previous: None,
        }
    }
}

/// Pick a row for one bar.
///
/// Returns the fallback row when nothing qualifies, which is a real outcome
/// rather than an error: an inherited sheet with no manifest declares no energy
/// and no pools, and must still dance.
pub fn choose(rows: &[RowInfo], ctx: &Context, fallback: usize, rng: &mut Rng) -> usize {
    let all: Vec<&RowInfo> = rows.iter().filter(|r| r.selectable()).collect();
    if all.is_empty() {
        return fallback;
    }

    // 1. Motif ceiling. This is the rule `energy` could not express: a turn is too
    // big an action for a calm passage *however* its row is scored, which is why the
    // dancer used to spin through quiet parts. Untagged rows pass, and if the ceiling
    // would leave nothing the filter is dropped — a sheet whose every move is big
    // must still dance to quiet music.
    //
    // Applied here rather than later so the **build** override obeys it too. A
    // wind-up is preparation: it precedes the accent, so it should be smaller than
    // what follows, not bigger. Only a drop is allowed past the ceiling, and that
    // uses `all` below.
    let admitted: Vec<&RowInfo> = all
        .iter()
        .copied()
        .filter(|r| motif::admits(ctx.tier, &r.motifs))
        .collect();
    let pool = if admitted.is_empty() { all.clone() } else { admitted };

    // 2. Cue overrides, in the priority order of spec §11.3.
    if ctx.building {
        // Anacrusis moves: visibly winding up while the music winds up. A row
        // whose impact lands late in its own loop *is* a wind-up, so the
        // `impact_cell` is the structural signal and the pool name is a hint.
        if let Some(r) = weighted(
            &named_pool(&pool, "build"),
            |r| 1.0 + r.impact_cell as f32,
            rng,
        ) {
            return r;
        }
        if let Some(r) = weighted(
            &pool
                .iter()
                .copied()
                .filter(|r| r.impact_cell as usize * 2 >= r.cells)
                .collect::<Vec<_>>(),
            |r| 1.0 + r.impact_cell as f32,
            rng,
        ) {
            return r;
        }
    }

    if ctx.boundary {
        // A drop wants the biggest thing available, and a one-shot in preference
        // to a loop — it should read as a punctuation mark, not a new groove.
        //
        // "Biggest" is either declared energy or a big Motif: a row tagged
        // `motif = ["jump"]` is a drop move whether or not its author also put a
        // number on it. Drawn from `all` rather than the admitted pool — this is the
        // one place the tier ceiling is ignored outright, because a drop is
        // precisely when the big move belongs.
        let hits: Vec<&RowInfo> = all
            .iter()
            .copied()
            .filter(|r| {
                r.energy.is_some_and(|e| e >= 0.6)
                    || motif::exertion(&r.motifs).is_some_and(|x| x >= motif::BIG_MOVE)
            })
            .collect();
        if let Some(r) = weighted(&hits, |r| if r.loopable { 1.0 } else { 2.5 }, rng) {
            return r;
        }
    }

    // 3. Labels, when the score has them. Enrichment, not a requirement.
    let labelled: Vec<&RowInfo> = match ctx.label.as_deref() {
        Some(label) => pool
            .iter()
            .copied()
            .filter(|r| r.pools.iter().any(|p| p.eq_ignore_ascii_case(label)))
            .collect(),
        None => Vec::new(),
    };
    let stage: Vec<&RowInfo> = if labelled.is_empty() { pool } else { labelled };

    // 4. Energy proximity. Rows that declare no energy stay eligible: a sheet
    // without a manifest would otherwise have no candidates at all.
    let near = match ctx.energy {
        Some(e) => nearest_by_energy(&stage, e),
        None => stage.clone(),
    };
    let near = if near.is_empty() { stage } else { near };

    // 5. No immediate repeats — unless that would leave nothing to pick from.
    let fresh: Vec<&RowInfo> = near
        .iter()
        .copied()
        .filter(|r| Some(r.index) != ctx.previous)
        .collect();
    let fresh = if fresh.is_empty() { near } else { fresh };

    // 6. Weighted random, favouring rows whose energy sits closest to the music's
    // and whose Time Effort matches how punctuated the bar is. The effort term is a
    // multiplier rather than a filter because the measurement behind it is a proxy
    // (see `crate::effort`) — it should tilt a choice, not make it.
    let energy = ctx.energy;
    let articulation = ctx.articulation;
    weighted(&fresh, |r| {
        let by_energy = match (energy, r.energy) {
            (Some(e), Some(re)) => (1.0 - (re - e).abs() / ENERGY_WINDOW).max(0.05),
            // Nothing to compare on: uniform rather than arbitrary.
            _ => 1.0,
        };
        by_energy * effort::weight(r.effort_time, articulation)
    }, rng)
    .unwrap_or(fallback)
}

/// Rows whose energy is close to the music's — by threshold, widened to the
/// nearest few if that leaves too little to choose from.
///
/// The plain threshold is spec §11.3, and on a track with dynamics it does the
/// right thing. M3 measured what happens without dynamics: an analysed track whose
/// energy sat at 0.89 median put exactly one row of the default sheet inside the
/// window, so the dancer repeated one move for the whole track — which is the
/// FAOSDance behaviour this project exists to beat.
///
/// A loudness-war master does the same thing to real music, so this is not only a
/// property of synthetic test signals. Falling back to the nearest few keeps energy
/// steering the choice while guaranteeing there is a choice to make.
fn nearest_by_energy<'a>(rows: &[&'a RowInfo], energy: f32) -> Vec<&'a RowInfo> {
    let dist = |r: &RowInfo| r.energy.map_or(0.0, |re| (re - energy).abs());

    let within: Vec<&RowInfo> = rows
        .iter()
        .copied()
        .filter(|r| r.energy.is_none_or(|re| (re - energy).abs() < ENERGY_WINDOW))
        .collect();
    if within.len() >= 2 {
        return within;
    }

    let mut ranked: Vec<&RowInfo> = rows.to_vec();
    ranked.sort_by(|a, b| {
        dist(a)
            .partial_cmp(&dist(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            // Stable across runs: energy ties must not depend on sort internals.
            .then(a.index.cmp(&b.index))
    });

    // Widen, but not without limit. Reaching twice the window finds a plausible
    // neighbour; reaching further would put a full-energy spin in a quiet intro,
    // which is a worse failure than repeating a move. If nothing is even that
    // close, one row it is.
    let widened: Vec<&RowInfo> = ranked
        .iter()
        .copied()
        .filter(|r| dist(r) <= ENERGY_WINDOW * 2.0)
        .take(3)
        .collect();
    if widened.is_empty() {
        ranked.truncate(1);
        return ranked;
    }
    widened
}

fn named_pool<'a>(rows: &[&'a RowInfo], name: &str) -> Vec<&'a RowInfo> {
    rows.iter()
        .copied()
        .filter(|r| r.pools.iter().any(|p| p.eq_ignore_ascii_case(name)))
        .collect()
}

fn weighted(rows: &[&RowInfo], weight: impl Fn(&RowInfo) -> f32, rng: &mut Rng) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let weights: Vec<f32> = rows.iter().map(|r| weight(r).max(0.0)).collect();
    let total: f32 = weights.iter().sum();
    if total <= 0.0 {
        return Some(rows[(rng.next_f32() * rows.len() as f32) as usize % rows.len()].index);
    }

    let mut pick = rng.next_f32() * total;
    for (r, w) in rows.iter().zip(&weights) {
        pick -= w;
        if pick <= 0.0 {
            return Some(r.index);
        }
    }
    Some(rows[rows.len() - 1].index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(index: usize, name: &str, energy: Option<f32>, pools: &[&str]) -> RowInfo {
        RowInfo {
            index,
            name: name.into(),
            cells: 8,
            beats_per_loop: 1,
            impact_cell: 0,
            pools: pools.iter().map(|s| s.to_string()).collect(),
            energy,
            motifs: Vec::new(),
            effort_time: None,
            loopable: true,
            held: false,
        }
    }

    /// Untagged rows, as every inherited sheet is. Motif rules must not touch these.
    fn sheet() -> Vec<RowInfo> {
        vec![
            row(0, "idle", Some(0.15), &["idle", "intro"]),
            row(1, "bounce", Some(0.55), &["verse", "chorus"]),
            row(2, "spin", Some(0.9), &["chorus"]),
            RowInfo { held: true, ..row(3, "Held", None, &[]) },
        ]
    }

    /// The same sheet with Motif tags, as `assets/default.toml` now ships.
    fn tagged_sheet() -> Vec<RowInfo> {
        let mut rows = sheet();
        rows[0].motifs = vec![Motif::Step, Motif::Gesture];
        rows[1].motifs = vec![Motif::Step, Motif::Sink];
        rows[2].motifs = vec![Motif::Turn];
        rows
    }

    /// Sample the distribution so a "sometimes" assertion is not flaky.
    fn sample(rows: &[RowInfo], ctx: &Context, n: usize) -> Vec<usize> {
        let mut rng = Rng::new(11);
        let mut counts = vec![0usize; rows.len()];
        for _ in 0..n {
            counts[choose(rows, ctx, 0, &mut rng)] += 1;
        }
        counts
    }

    #[test]
    fn the_held_row_is_never_chosen() {
        // It is a drag pose. Dancing it would look like the sprite is stuck.
        let counts = sample(&sheet(), &Context::default(), 500);
        assert_eq!(counts[3], 0);
    }

    #[test]
    fn energy_steers_the_choice() {
        let quiet = sample(&sheet(), &Context { energy: Some(0.15), ..Default::default() }, 500);
        let loud = sample(&sheet(), &Context { energy: Some(0.9), ..Default::default() }, 500);

        assert!(quiet[0] > quiet[2], "quiet music should favour idle: {quiet:?}");
        assert!(loud[2] > loud[0], "loud music should favour spin: {loud:?}");
        // Far rows are excluded outright: a full-energy spin during a quiet intro
        // is a worse failure than repeating a move, so widening stops at twice
        // the window.
        assert_eq!(quiet[2], 0, "0.9 is far outside 0.35 of 0.15: {quiet:?}");
        assert_eq!(loud[0], 0, "0.15 is far outside 0.35 of 0.9: {loud:?}");
    }

    #[test]
    fn labels_narrow_selection_when_present() {
        let ctx = Context {
            label: Some("chorus".into()),
            energy: Some(0.7),
            ..Default::default()
        };
        let counts = sample(&sheet(), &ctx, 500);
        assert_eq!(counts[0], 0, "idle is not in the chorus pool");
        assert!(counts[1] + counts[2] == 500);
    }

    #[test]
    fn an_unlabelled_score_still_dances() {
        // The normal case: beat-this gives no segments at all (spec §8.1).
        let ctx = Context { energy: Some(0.5), ..Default::default() };
        let counts = sample(&sheet(), &ctx, 500);
        assert!(counts.iter().take(3).any(|&n| n > 0));
        assert_eq!(counts[3], 0);
    }

    #[test]
    fn a_sheet_with_no_manifest_at_all_still_dances() {
        // FL Chan and every other inherited sheet: no energy, no pools, no
        // loopable flags. Selection must not collapse to one row.
        let rows: Vec<RowInfo> = (0..5).map(|i| row(i, &format!("row{i}"), None, &[])).collect();
        let counts = sample(&rows, &Context { energy: Some(0.5), ..Default::default() }, 500);
        assert!(counts.iter().filter(|&&n| n > 0).count() >= 4, "{counts:?}");
    }

    #[test]
    fn the_previous_row_is_avoided() {
        let ctx = Context {
            energy: Some(0.55),
            previous: Some(1),
            ..Default::default()
        };
        let counts = sample(&sheet(), &ctx, 500);
        assert_eq!(counts[1], 0, "no immediate repeats");
    }

    #[test]
    fn avoiding_repeats_never_leaves_nothing() {
        // One selectable row, and it was just used. Repeating beats freezing.
        let rows = vec![row(0, "only", Some(0.5), &[]), RowInfo { held: true, ..row(1, "Held", None, &[]) }];
        let ctx = Context { previous: Some(0), energy: Some(0.5), ..Default::default() };
        assert_eq!(choose(&rows, &ctx, 0, &mut Rng::new(3)), 0);
    }

    #[test]
    fn a_build_prefers_a_late_impact() {
        // An anacrusis move is one whose accent lands late in its own loop — that
        // is what makes it read as a wind-up.
        let mut rows = sheet();
        rows[1].impact_cell = 7;
        let ctx = Context { building: true, energy: Some(0.5), ..Default::default() };
        let counts = sample(&rows, &ctx, 500);
        assert!(counts[1] > counts[0], "{counts:?}");
    }

    #[test]
    fn a_drop_prefers_a_one_shot_hit() {
        let mut rows = sheet();
        rows[2].loopable = false;
        let ctx = Context { boundary: true, energy: Some(0.3), ..Default::default() };
        let counts = sample(&rows, &ctx, 500);
        // Energy 0.3 would normally exclude spin at 0.9; the boundary overrides.
        assert!(counts[2] > 300, "a drop should reach for the big move: {counts:?}");
    }

    #[test]
    fn selection_is_reproducible_from_its_seed() {
        // A choreography judged by eye needs to be reproducible to be debuggable.
        // Energy 0.35 leaves idle and bounce both in range, so the run genuinely
        // varies — with a single candidate every seed would agree trivially and
        // the test would prove nothing.
        let ctx = Context { energy: Some(0.35), ..Default::default() };
        let run = |seed| {
            let mut rng = Rng::new(seed);
            (0..20).map(|_| choose(&sheet(), &ctx, 0, &mut rng)).collect::<Vec<_>>()
        };
        let a = run(5);
        assert!(a.iter().any(|&r| r != a[0]), "run should vary: {a:?}");
        assert_eq!(a, run(5));
        assert_ne!(a, run(6));
    }

    #[test]
    fn a_track_with_no_dynamics_still_varies_its_moves() {
        // Measured in M3: an analysed track sat at 0.89 energy throughout, which
        // put one row of the default sheet inside the window and made the dancer
        // repeat a single move — the FAOSDance behaviour this exists to beat.
        // Loudness-war masters do this to real music too.
        let counts = sample(&sheet(), &Context { energy: Some(0.95), ..Default::default() }, 500);
        let used = counts.iter().filter(|&&n| n > 0).count();
        assert!(used >= 2, "one move for the whole track: {counts:?}");
        assert_eq!(counts[3], 0, "and still never the Held row");
        // Energy still steers: the loudest row should dominate.
        assert!(counts[2] > counts[0], "{counts:?}");
    }

    #[test]
    fn a_big_move_is_refused_by_a_calm_bar_whatever_its_energy_says() {
        // The rule a single scalar could not express. Spin is scored 0.35 here, so
        // energy proximity alone would happily admit it to a 0.2 bar — and that is
        // exactly how a full turn ended up in quiet passages.
        let mut rows = tagged_sheet();
        rows[2].energy = Some(0.35);
        let ctx = Context { tier: Tier::Calm, energy: Some(0.2), ..Default::default() };

        let counts = sample(&rows, &ctx, 500);
        assert_eq!(counts[2], 0, "a turn in a calm passage: {counts:?}");
        assert_eq!(counts[0] + counts[1], 500, "{counts:?}");
    }

    #[test]
    fn an_untagged_sheet_is_left_alone_by_the_motif_rules() {
        // The same case with no motif declared. Nothing was said about these rows,
        // so nothing may be inferred — every inherited sheet lands here.
        let mut rows = sheet();
        rows[2].energy = Some(0.35);
        let ctx = Context { tier: Tier::Calm, energy: Some(0.2), ..Default::default() };

        let counts = sample(&rows, &ctx, 500);
        assert!(counts[2] > 0, "nothing declared, nothing to exclude: {counts:?}");
    }

    #[test]
    fn a_sheet_of_only_big_moves_still_dances_to_quiet_music() {
        // The ceiling must never be the thing that empties the pool.
        let rows = vec![
            RowInfo { motifs: vec![Motif::Jump], ..row(0, "leap", None, &[]) },
            RowInfo { motifs: vec![Motif::Turn], ..row(1, "spin", None, &[]) },
        ];
        let ctx = Context { tier: Tier::Calm, energy: Some(0.1), ..Default::default() };

        let counts = sample(&rows, &ctx, 300);
        assert_eq!(counts.iter().sum::<usize>(), 300);
        assert!(counts[0] > 0 && counts[1] > 0, "{counts:?}");
    }

    #[test]
    fn a_wind_up_obeys_the_ceiling_but_a_drop_does_not() {
        // Found by the scheduler tests: the bar before a loud section is `building`,
        // and the build override was reaching straight past the tier ceiling for the
        // spin — putting a full turn in the last quiet bar, which is the original
        // complaint wearing a different hat.
        //
        // A wind-up is *preparation*; it precedes the accent, so it should be
        // smaller than what follows. A drop is the accent, so it may be anything.
        let mut rows = tagged_sheet();
        rows[2].impact_cell = 7; // the anacrusis shape the build rule looks for
        let calm = Context { tier: Tier::Calm, energy: Some(0.2), ..Default::default() };

        let building = sample(&rows, &Context { building: true, ..calm.clone() }, 300);
        assert_eq!(building[2], 0, "a turn as a wind-up in a calm bar: {building:?}");

        let dropping = sample(&rows, &Context { boundary: true, ..calm }, 300);
        assert_eq!(dropping[2], 300, "but the drop itself gets it: {dropping:?}");
    }

    #[test]
    fn a_drop_reaches_for_a_big_motif_with_no_energy_declared() {
        // `motif = ["jump"]` is a drop move whether or not its author also put a
        // number on it.
        let rows = vec![
            row(0, "idle", Some(0.2), &[]),
            RowInfo {
                motifs: vec![Motif::Jump],
                loopable: false,
                ..row(1, "leap", None, &[])
            },
        ];
        let ctx = Context {
            boundary: true,
            tier: Tier::Calm,
            energy: Some(0.2),
            ..Default::default()
        };

        let counts = sample(&rows, &ctx, 300);
        assert_eq!(counts[1], 300, "a drop should find the jump: {counts:?}");
    }

    #[test]
    fn time_effort_tilts_selection_without_deciding_it() {
        let rows = vec![
            RowInfo { effort_time: Some(Effort::Sudden), ..row(0, "stab", Some(0.5), &[]) },
            RowInfo { effort_time: Some(Effort::Sustained), ..row(1, "sway", Some(0.5), &[]) },
        ];
        let at = |a: f32| {
            sample(
                &rows,
                &Context { energy: Some(0.5), articulation: Some(a), ..Default::default() },
                600,
            )
        };

        let punctuated = at(1.0);
        let flowing = at(0.0);
        assert!(punctuated[0] > punctuated[1], "stabbing music: {punctuated:?}");
        assert!(flowing[1] > flowing[0], "flowing music: {flowing:?}");
        // And never a shutout — the measurement behind this is a proxy, so it
        // weights the choice rather than making it.
        assert!(punctuated[1] > 0 && flowing[0] > 0, "{punctuated:?} {flowing:?}");
    }

    #[test]
    fn widening_is_stable_rather_than_arbitrary() {
        // Two rows equidistant from the target must rank the same way every run,
        // or a choreography stops being reproducible from its seed.
        let rows = vec![
            row(0, "a", Some(0.2), &[]),
            row(1, "b", Some(0.8), &[]),
        ];
        let ctx = Context { energy: Some(0.5), ..Default::default() };
        let first = sample(&rows, &ctx, 200);
        assert_eq!(first, sample(&rows, &ctx, 200));
    }
}
