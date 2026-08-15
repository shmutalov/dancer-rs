# Phase 0.1 — `beat-this` validation probe

Answers ROADMAP.md §0.1: are `beat-this` grids good enough to schedule against?

## Setup

Model weights are **not** bundled in the published crate and are gitignored here.
Fetch them before running:

```sh
mkdir -p models
for m in mel_spectrogram.onnx beat_this_small.onnx; do
  curl -sSL -o "models/$m" "https://raw.githubusercontent.com/danigb/beat-this-rs/main/models/$m"
done
```

That gets the ~270 KB mel front end and the ~10 MB small beat model. The
full-accuracy ~83 MB model is on the upstream repo's Releases page.

## Run

```sh
cargo run --release -- <audio>...        # real material
cargo run --release -- synthetic 128     # exact ground truth, click track
```

## Results — 2026-08-15

Rust 1.95.0, `stable-x86_64-pc-windows-gnu`, edition 2024, small model.

| Track | BPM | IBI sd | Meter | Cost |
|---|---|---|---|---|
| pachimu-vsyo-ischezaeyet | 73.17 | 1.4% | 4/4, 51% consistent | 46x realtime |
| walts with me | 176.47 | 12.5% | **3/4**, 97% consistent | 41x realtime |
| Джингл Белс - Тили Бом | 107.14 | 2.2% | 4/4, 98% consistent | 44x realtime |
| 1.wav (non-musical) | n/a | — | no beats returned | 74x realtime |

**Verdict: pass, with one caveat.**

- **Beat grids are solid.** 1.4–2.2% inter-beat deviation on steady material is
  well inside what the clock can track. The 12.5% on the waltz is expressive
  timing, not error.
- **Meter is not always 4/4.** The waltz was correctly identified as 3/4 at 97%
  consistency. The score format must carry meter rather than assume four.
- **Downbeats are the weaker signal.** `pachimu` has a rock-steady beat grid but
  splits 29 two-beat bars against 30 four-beat ones — spurious downbeats halving
  bars. Since §11.3 changes moves on downbeats only, the scheduler needs bar phase
  fitted to the downbeat *candidates*, not trust in each one.
- **Non-musical input returns an empty grid** rather than a hallucinated one. That
  is the failure mode we want: no beats means stay reactive.
- **Cost is a non-issue.** 41–74x realtime; a 3.5-minute track analyses in about
  five seconds, far inside its own duration.
- **Tracker is stateless across calls.** Verified: isolated processes and
  reordered batches give identical output, so one instance can be reused. (Two
  test files produced identical analyses — they turned out to be the same audio
  under different ID3 tags, not a state leak.)

## Caveats on this result

- Small model, not the full-accuracy one. Upstream reports ~0.99 F-measure for the
  small model against ~1.0 for the standard.
- Four tracks, no independent ground truth. This establishes plausibility and
  stability, not accuracy against annotations.
- The synthetic click-track mode scores badly (38% IBI deviation) because a bare
  click track is out of distribution for a model trained on music. It is a smoke
  test for the plumbing, not a quality measure.
- The upstream port is AI-assisted, per its own README. It has CI and claims parity
  tests against the Python reference, but that is the author's claim.
