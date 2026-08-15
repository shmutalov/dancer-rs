//! Phase 0.1 — `beat-this` validation probe.
//!
//! Answers the question the roadmap gates on: does this crate produce grids we can
//! build a scheduler against? Two modes:
//!
//!   synthetic <bpm>   exact ground truth, in-memory click track
//!   <path>...         real audio: plausibility, stability, cost
//!
//! Pass criteria are in ROADMAP.md §0.1.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use beat_this::{beat_counts, calculate_bpm, load_audio, BeatAnalysis, BeatThis, RtenRuntime};

const TARGET_SR: u32 = 22050;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!("usage: beat-this-probe synthetic <bpm> | beat-this-probe <audio>...");
        std::process::exit(2);
    }

    let models = model_dir()?;
    let runtime = RtenRuntime;
    let mut tracker = BeatThis::new(
        &runtime,
        &models.join("mel_spectrogram.onnx"),
        &models.join("beat_this_small.onnx"),
    )
    .context("loading models")?;

    if args[0] == "synthetic" {
        let bpm: f32 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(120.0);
        return synthetic(&mut tracker, bpm);
    }

    for path in &args {
        if let Err(e) = real(&mut tracker, Path::new(path)) {
            println!("\n{}\n  FAILED: {e:#}", path);
        }
    }
    Ok(())
}

fn model_dir() -> Result<PathBuf> {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models");
    anyhow::ensure!(d.join("beat_this_small.onnx").exists(), "models/ missing");
    Ok(d)
}

/// Exact-ground-truth check. A click track is out-of-distribution for a model
/// trained on music, so this is a floor, not a ceiling — read a failure here as
/// "investigate", not "the crate is broken".
fn synthetic(tracker: &mut BeatThis<impl beat_this::Model>, bpm: f32) -> Result<()> {
    let secs = 30.0_f32;
    let period = 60.0 / bpm;
    let n = (secs * TARGET_SR as f32) as usize;
    let mut samples = vec![0.0_f32; n];

    // Click + a decaying low tone on downbeats, so it reads as vaguely musical.
    let mut k = 0usize;
    loop {
        let t = k as f32 * period;
        if t >= secs {
            break;
        }
        let start = (t * TARGET_SR as f32) as usize;
        let downbeat = k % 4 == 0;
        let (freq, dur, amp) = if downbeat {
            (80.0, 0.18, 0.9)
        } else {
            (1000.0, 0.04, 0.5)
        };
        let len = (dur * TARGET_SR as f32) as usize;
        for i in 0..len.min(n.saturating_sub(start)) {
            let x = i as f32 / TARGET_SR as f32;
            let env = (-x * 24.0).exp();
            samples[start + i] +=
                amp * env * (2.0 * std::f32::consts::PI * freq * x).sin();
        }
        k += 1;
    }

    let a = tracker.analyze_audio(&samples, TARGET_SR)?;
    println!("synthetic click track @ {bpm:.2} BPM, {secs:.0}s");
    report(&a);

    // Ground truth: beat k at k * period. Match each detected beat to nearest.
    let expected: Vec<f32> = (0..).map(|i| i as f32 * period).take_while(|t| *t < secs).collect();
    let mut errs = Vec::new();
    for e in &expected {
        if let Some(b) = a.beats.iter().min_by(|x, y| {
            (*x - e).abs().partial_cmp(&(*y - e).abs()).unwrap()
        }) {
            errs.push((b - e).abs() * 1000.0);
        }
    }
    if !errs.is_empty() {
        let mean = errs.iter().sum::<f32>() / errs.len() as f32;
        let max = errs.iter().cloned().fold(0.0_f32, f32::max);
        let within_20 = errs.iter().filter(|e| **e <= 20.0).count();
        println!(
            "  ground truth: {}/{} expected beats matched within 20 ms  (mean {mean:.1} ms, max {max:.1} ms)",
            within_20,
            expected.len()
        );
        println!(
            "  detected {} vs expected {} beats",
            a.beats.len(),
            expected.len()
        );
    }
    Ok(())
}

fn real(tracker: &mut BeatThis<impl beat_this::Model>, path: &Path) -> Result<()> {
    let audio = load_audio(path, TARGET_SR).context("decode")?;
    let dur = audio.samples.len() as f32 / audio.sample_rate as f32;

    let t0 = std::time::Instant::now();
    let timed = tracker.analyze_audio_timed(&audio.samples, audio.sample_rate)?;
    let wall = t0.elapsed().as_secs_f32();

    println!(
        "\n{}\n  {:.1}s audio, decoded @ {} Hz",
        path.file_name().unwrap_or_default().to_string_lossy(),
        dur,
        audio.sample_rate
    );
    report(&timed.analysis);
    println!(
        "  cost: {wall:.2}s wall ({:.0}x realtime)  [mel {:.2}s, predict {:.2}s, decode {:.3}s]",
        dur / wall,
        timed.timing.mel.as_secs_f32(),
        timed.timing.predict.as_secs_f32(),
        timed.timing.decode.as_secs_f32(),
    );
    Ok(())
}

fn report(a: &BeatAnalysis) {
    let bpm = calculate_bpm(a);
    let counts = beat_counts(a);

    // Inter-beat interval stability. High deviation means either genuine tempo
    // drift or a grid we cannot schedule against.
    let ibi: Vec<f32> = a.beats.windows(2).map(|w| w[1] - w[0]).collect();
    let (mean, sd) = mean_sd(&ibi);

    print!(
        "  bpm {:>7}  beats {:>4}  downbeats {:>3}  ibi {:.4}s sd {:.4}s ({:.1}%)",
        bpm.map(|b| format!("{b:.2}")).unwrap_or_else(|| "n/a".into()),
        a.beats.len(),
        a.downbeats.len(),
        mean,
        sd,
        if mean > 0.0 { sd / mean * 100.0 } else { 0.0 }
    );

    // Bar lengths from beat numbering: a stable 4/4 grid should be nearly all 4s.
    let mut bars: Vec<usize> = Vec::new();
    let mut cur = 0usize;
    for c in &counts {
        if *c == 1 && cur > 0 {
            bars.push(cur);
            cur = 0;
        }
        cur += 1;
    }
    if !bars.is_empty() {
        // Histogram, not "% are 4" — a waltz is 3/4 and that is not an error.
        let mut hist = std::collections::BTreeMap::new();
        for b in &bars {
            *hist.entry(*b).or_insert(0usize) += 1;
        }
        let modal = hist.iter().max_by_key(|(_, n)| **n).map(|(k, n)| (*k, *n));
        let dist: Vec<String> = hist
            .iter()
            .filter(|(_, n)| **n * 20 >= bars.len())
            .map(|(k, n)| format!("{k}x{n}"))
            .collect();
        if let Some((len, n)) = modal {
            println!(
                "\n  bars {:>3}  modal {len}/4 ({:.0}% consistent)  dist [{}]",
                bars.len(),
                n as f32 / bars.len() as f32 * 100.0,
                dist.join(" ")
            );
        }
    } else {
        println!();
    }

    // Downbeats must be a subset of beats — the scheduler assumes this.
    let orphans = a
        .downbeats
        .iter()
        .filter(|d| !a.beats.iter().any(|b| (b - *d).abs() < 1e-3))
        .count();
    if orphans > 0 {
        println!("  WARNING: {orphans} downbeats not aligned to any beat");
    }
}

fn mean_sd(v: &[f32]) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32;
    (mean, var.sqrt())
}
