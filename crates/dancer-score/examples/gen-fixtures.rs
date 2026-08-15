//! Generate the hand-written score fixtures (ROADMAP M1).
//!
//! "Hand-written" means *known-correct by construction*, not typed by hand — these
//! grids are exact arithmetic, which is the point: a grid with no analyzer error in
//! it isolates clock bugs from analyzer bugs. M2's real scores get compared against
//! these, and these stay as regression tests forever.
//!
//! ```sh
//! cargo run -p dancer-score --example gen-fixtures
//! ```

use std::path::Path;

use dancer_score::{Cue, Score, ScoreSource, Segment, SCHEMA};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures");
    std::fs::create_dir_all(&dir)?;

    write(&dir, "steady-120.json", steady_120())?;
    write(&dir, "waltz-90.json", waltz_90())?;
    write(&dir, "drifting-128.json", drifting_128())?;
    write(&dir, "labelled-124.json", labelled_124())?;
    Ok(())
}

fn write(dir: &Path, name: &str, score: Score) -> Result<(), Box<dyn std::error::Error>> {
    score.validate()?;
    let path = dir.join(name);
    std::fs::write(&path, score.to_json()?)?;
    println!(
        "{name}: {:.1} BPM, {}/4, {} beats, {:.0}s",
        score.bpm,
        score.meter,
        score.beats.len(),
        score.duration_secs()
    );
    Ok(())
}

fn grid(bpm: f64, meter: u8, secs: f64) -> (Vec<f64>, Vec<u8>, Vec<f64>) {
    let interval = 60.0 / bpm;
    let n = (secs / interval) as usize;
    let beats: Vec<f64> = (0..n).map(|i| i as f64 * interval).collect();
    let positions: Vec<u8> = (0..n).map(|i| (i % meter as usize + 1) as u8).collect();
    let downbeats = beats
        .iter()
        .zip(&positions)
        .filter(|&(_, &p)| p == 1)
        .map(|(&b, _)| b)
        .collect();
    (beats, positions, downbeats)
}

/// The baseline: 120 BPM, 4/4, three minutes. Beat every 0.5 s, bar every 2 s, so
/// every assertion about it can be checked in your head.
fn steady_120() -> Score {
    let (beats, beat_positions, downbeats) = grid(120.0, 4, 180.0);
    Score {
        schema: SCHEMA,
        track_id: "fixture:steady-120".into(),
        duration_ms: 180_000,
        bpm: 120.0,
        meter: 4,
        source: ScoreSource::Builtin,
        confidence: 1.0,
        analyzed_at: "2026-08-15T00:00:00Z".into(),
        beats,
        beat_positions,
        downbeats,
        segments: vec![],
        cues: vec![],
    }
}

/// 3/4. Phase 0.1 detected a waltz correctly at 97 % confidence, so a fixture that
/// is not in four exists to catch anything that quietly assumes it.
fn waltz_90() -> Score {
    let (beats, beat_positions, downbeats) = grid(90.0, 3, 120.0);
    Score {
        schema: SCHEMA,
        track_id: "fixture:waltz-90".into(),
        duration_ms: 120_000,
        bpm: 90.0,
        meter: 3,
        source: ScoreSource::Builtin,
        confidence: 0.97,
        analyzed_at: "2026-08-15T00:00:00Z".into(),
        beats,
        beat_positions,
        downbeats,
        segments: vec![],
        cues: vec![],
    }
}

/// A grid that speeds up 2 % across the track, as a live recording does.
///
/// Anything deriving frame timing from the global `bpm` rather than from local
/// beat intervals (spec §11.1) will visibly lag by the end of this one.
fn drifting_128() -> Score {
    let nominal = 60.0 / 128.0;
    let n = 400;
    let mut beats = Vec::with_capacity(n);
    let mut t = 0.0;
    for i in 0..n {
        beats.push(t);
        // Interval shrinks linearly: 128 BPM at the start, ~130.6 at the end.
        t += nominal * (1.0 - 0.02 * (i as f64 / n as f64));
    }
    let beat_positions: Vec<u8> = (0..n).map(|i| (i % 4 + 1) as u8).collect();
    let downbeats = beats.iter().copied().step_by(4).collect();
    Score {
        schema: SCHEMA,
        track_id: "fixture:drifting-128".into(),
        duration_ms: (t * 1000.0) as u64,
        // Deliberately the *starting* tempo, which is wrong everywhere else.
        bpm: 128.0,
        meter: 4,
        source: ScoreSource::Builtin,
        confidence: 0.85,
        analyzed_at: "2026-08-15T00:00:00Z".into(),
        beats,
        beat_positions,
        downbeats,
        segments: vec![],
        cues: vec![],
    }
}

/// The only fixture with segments, for M3's move selection.
///
/// Every score from beat-this alone has none (spec §5, §8.1), so this is the
/// unusual case and the empty ones above are the normal one.
fn labelled_124() -> Score {
    let (beats, beat_positions, downbeats) = grid(124.0, 4, 150.0);
    Score {
        schema: SCHEMA,
        track_id: "fixture:labelled-124".into(),
        duration_ms: 150_000,
        bpm: 124.0,
        meter: 4,
        source: ScoreSource::Allin1,
        confidence: 0.9,
        analyzed_at: "2026-08-15T00:00:00Z".into(),
        beats,
        beat_positions,
        downbeats,
        segments: vec![
            Segment { start: 0.0, end: 15.48, label: "intro".into(), energy: 0.21 },
            Segment { start: 15.48, end: 46.45, label: "verse".into(), energy: 0.55 },
            Segment { start: 46.45, end: 77.42, label: "chorus".into(), energy: 0.88 },
            Segment { start: 77.42, end: 108.39, label: "verse".into(), energy: 0.57 },
            Segment { start: 108.39, end: 150.0, label: "chorus".into(), energy: 0.91 },
        ],
        cues: vec![
            Cue { time: 44.51, kind: "build".into(), bars: 1 },
            Cue { time: 46.45, kind: "drop".into(), bars: 0 },
            Cue { time: 106.45, kind: "build".into(), bars: 1 },
            Cue { time: 108.39, kind: "drop".into(), bars: 0 },
        ],
    }
}
