//! Beat-grid analysis (spec §8.1) — `beat-this` behind a stable interface.
//!
//! The tracker is `beat-this` 1.0 on the `rten` backend: pure Rust inference, no
//! ONNX Runtime, no C compiler, no Python. That property is why the analyzer could
//! be a first-class part of the app rather than an optional sidecar, and it is
//! worth protecting — see spec §8.1 before swapping the backend.
//!
//! What this crate adds on top of raw tracker output is the interpretation, and
//! that is where the Phase 0.1 findings live:
//!
//! - **Meter is inferred, never assumed.** A waltz was detected correctly at 97 %.
//! - **Downbeats are candidates, not ground truth.** A bar phase is *fitted* to
//!   them so spurious ones lose a vote instead of halving bars.
//! - **Confidence is conservative.** A confidently wrong grid is worse than none
//!   (spec §8.3), so partial or incoherent detections score below the `Locked` gate.
//!
//! Model weights are **not** bundled — see [`Analyzer::new`].

use std::path::{Path, PathBuf};

use beat_this::{beat_counts, calculate_bpm, load_audio, BeatThis, RtenRuntime};
use dancer_score::{Score, ScoreSource, TrackId, SCHEMA};

pub mod grid;
mod time;

pub use grid::{BarGrid, ConfidenceInputs};

/// What the tracker was trained at. Resampling happens in `load_audio`.
pub const TARGET_SR: u32 = 22_050;

/// Downbeat-to-beat snapping tolerance. Generous: a downbeat is meant to *be* a
/// beat, so anything this far off is a mismatch rather than rounding.
const SNAP_TOLERANCE: f64 = 0.05;

pub const MEL_MODEL: &str = "mel_spectrogram.onnx";
pub const BEAT_MODEL: &str = "beat_this_small.onnx";

/// Where to fetch weights, for the error message when they are missing.
pub const MODEL_URL_BASE: &str = "https://raw.githubusercontent.com/danigb/beat-this-rs/main/models";

#[derive(Debug, thiserror::Error)]
pub enum AnalyzeError {
    #[error(
        "model weights not found in {dir}\n\
         They are not bundled (~10 MB). Fetch them with:\n  \
         curl -sSL -o \"{dir}/{MEL_MODEL}\" {MODEL_URL_BASE}/{MEL_MODEL}\n  \
         curl -sSL -o \"{dir}/{BEAT_MODEL}\" {MODEL_URL_BASE}/{BEAT_MODEL}"
    )]
    ModelsMissing { dir: String },
    #[error("loading models from {dir}: {message}")]
    ModelLoad { dir: String, message: String },
    #[error("decoding {path}: {message}")]
    Decode { path: PathBuf, message: String },
    #[error("analysing {path}: {message}")]
    Analyze { path: PathBuf, message: String },
}

/// A loaded tracker. Reusable across tracks.
///
/// Phase 0.1 verified the tracker is stateless across calls — isolated processes
/// and reordered batches give identical output — so one instance can serve the
/// whole session rather than paying model load per track.
pub struct Analyzer {
    // Named through the associated type: `RtenModel` is what `RtenRuntime` loads,
    // but the crate does not re-export it at the root.
    tracker: BeatThis<<RtenRuntime as beat_this::Runtime>::Model>,
    models: PathBuf,
}

impl Analyzer {
    /// Load the models from a directory.
    ///
    /// **Weights are not bundled.** The crate does not ship them and neither do
    /// we: ~270 KB for the mel front end and ~10 MB for the small beat model,
    /// which is the difference between a download people accept and one they do
    /// not. The full-accuracy model is ~83 MB and, per spec §8.1, is chosen on
    /// measured quality rather than by default.
    pub fn new(models: &Path) -> Result<Self, AnalyzeError> {
        let mel = models.join(MEL_MODEL);
        let beat = models.join(BEAT_MODEL);
        if !mel.exists() || !beat.exists() {
            return Err(AnalyzeError::ModelsMissing {
                dir: models.display().to_string(),
            });
        }

        let tracker =
            BeatThis::new(&RtenRuntime, &mel, &beat).map_err(|e| AnalyzeError::ModelLoad {
                dir: models.display().to_string(),
                message: e.to_string(),
            })?;

        tracing::info!(models = %models.display(), "analyzer ready");
        Ok(Self {
            tracker,
            models: models.to_owned(),
        })
    }

    pub fn models_dir(&self) -> &Path {
        &self.models
    }

    /// Analyse an audio file into a score.
    ///
    /// Cost is not a concern: Phase 0.1 measured 41–74× realtime, so a 3½-minute
    /// track takes about five seconds. It still belongs off the render thread.
    pub fn analyze_file(&mut self, path: &Path, id: &TrackId) -> Result<Score, AnalyzeError> {
        let audio = load_audio(path, TARGET_SR).map_err(|e| AnalyzeError::Decode {
            path: path.to_owned(),
            message: e.to_string(),
        })?;
        let duration = audio.samples.len() as f64 / audio.sample_rate as f64;

        let t0 = std::time::Instant::now();
        let analysis = self
            .tracker
            .analyze_audio(&audio.samples, audio.sample_rate)
            .map_err(|e| AnalyzeError::Analyze {
                path: path.to_owned(),
                message: e.to_string(),
            })?;
        let wall = t0.elapsed();

        let score = self.build_score(&analysis, &audio.samples, audio.sample_rate, duration, id);

        tracing::info!(
            path = %path.display(),
            beats = score.beats.len(),
            bpm = score.bpm,
            meter = score.meter,
            confidence = score.confidence,
            secs = wall.as_secs_f32(),
            realtime = duration as f32 / wall.as_secs_f32().max(1e-6),
            "analysed"
        );
        Ok(score)
    }

    fn build_score(
        &self,
        analysis: &beat_this::BeatAnalysis,
        samples: &[f32],
        sample_rate: u32,
        duration: f64,
        id: &TrackId,
    ) -> Score {
        let beats: Vec<f64> = analysis.beats.iter().map(|&b| b as f64).collect();
        let downbeat_times: Vec<f64> = analysis.downbeats.iter().map(|&b| b as f64).collect();

        let counts: Vec<u8> = beat_counts(analysis).iter().map(|&c| c as u8).collect();
        let (meter, consistency) = grid::infer_meter(&counts);

        let downbeat_idx = grid::snap_downbeats(&beats, &downbeat_times, SNAP_TOLERANCE);
        let mut bar_grid = grid::fit_bar_phase(beats.len(), meter, &downbeat_idx);
        bar_grid.consistency = consistency;

        let beat_positions = if beats.is_empty() {
            Vec::new()
        } else {
            grid::beat_positions(beats.len(), &bar_grid)
        };

        // Emit the *fitted* downbeats, not the candidates: everything downstream
        // should see one coherent bar grid rather than the raw votes.
        let downbeats: Vec<f64> = beats
            .iter()
            .zip(&beat_positions)
            .filter(|&(_, &p)| p == 1)
            .map(|(&b, _)| b)
            .collect();

        let (ibi_mean, ibi_sd) = mean_sd(&beats);
        let span = match (beats.first(), beats.last()) {
            (Some(&a), Some(&b)) => b - a,
            _ => 0.0,
        };

        let confidence = grid::confidence(ConfidenceInputs {
            beat_count: beats.len(),
            span,
            duration,
            ibi_mean,
            ibi_sd,
            meter_consistency: consistency,
            bar_agreement: bar_grid.agreement,
        });

        Score {
            schema: SCHEMA,
            track_id: id.key(),
            duration_ms: (duration * 1000.0) as u64,
            // From the tracker's own estimate, falling back to the measured mean.
            // Note this is a *summary*: frame timing must come from local intervals
            // (spec §11.1), never from here.
            bpm: calculate_bpm(analysis)
                .map(|b| b as f64)
                .unwrap_or(if ibi_mean > 0.0 { 60.0 / ibi_mean } else { 0.0 }),
            meter: bar_grid.meter,
            source: ScoreSource::BeatThis,
            confidence,
            analyzed_at: time::now_rfc3339(),
            beat_energy: grid::beat_energy(samples, sample_rate, &beats),
            beats,
            beat_positions,
            downbeats,
            // beat-this produces no segment labels — that is §8.2's optional
            // sidecar. Empty is the normal case and everything downstream must
            // tolerate it (spec §5).
            segments: Vec::new(),
            // Cues are derived from segment boundaries, or from novelty on the
            // beat grid when there are none. That derivation is M3's, alongside
            // the scheduler that consumes it.
            cues: Vec::new(),
        }
    }
}

/// Mean and standard deviation of the inter-beat interval.
fn mean_sd(beats: &[f64]) -> (f64, f64) {
    let ibi: Vec<f64> = beats.windows(2).map(|w| w[1] - w[0]).collect();
    if ibi.is_empty() {
        return (0.0, 0.0);
    }
    let mean = ibi.iter().sum::<f64>() / ibi.len() as f64;
    let var = ibi.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / ibi.len() as f64;
    (mean, var.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_sd_of_a_perfect_grid_has_no_deviation() {
        let beats: Vec<f64> = (0..20).map(|i| i as f64 * 0.5).collect();
        let (mean, sd) = mean_sd(&beats);
        assert!((mean - 0.5).abs() < 1e-12);
        assert!(sd < 1e-12);
    }

    #[test]
    fn mean_sd_survives_degenerate_input() {
        assert_eq!(mean_sd(&[]), (0.0, 0.0));
        assert_eq!(mean_sd(&[1.0]), (0.0, 0.0));
    }

    #[test]
    fn missing_models_say_how_to_fix_it() {
        let msg = match Analyzer::new(Path::new("definitely-not-here")) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("a nonexistent directory must not load"),
        };
        assert!(msg.contains("curl"), "error should be actionable: {msg}");
        assert!(msg.contains(BEAT_MODEL));
    }
}
