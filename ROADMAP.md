# dancer-rs — Implementation Roadmap

**Companion to:** [dancer-spec.md](dancer-spec.md)
**Revised:** 2026-08-15
**Status:** Active plan. Supersedes spec §14.

The spec describes *what* is being built and why. This describes *in what order*, and
which questions must be answered before each step is worth taking.

---

## 1. Stack decisions

Decisions taken 2026-08-15, after surveying the current crate ecosystem. Each one
removes something the original spec assumed was necessary.

| Concern | Decision | Rationale |
|---|---|---|
| Primary language | **Rust, end to end** | Every layer has a first-class crate; Python survives only as an optional extra |
| Toolchain | **GNU**, pinned in `rust-toolchain.toml` | MSVC needs a ~2 GB Windows SDK for import libraries. Every surface verified on GNU instead — see §0.4 |
| Windows 10 | **First-class, not degraded** | Large share of the install base. No feature may require a Win11-only API. Applying that rule is what removed the audio subsystem — see §4.1 |
| Audio capture | **Cut from v1** | No WASAPI, no loopback, no recording, no `dancer-audio`. Every purpose it served either dissolved or has a cheaper answer. Costs streaming support; see §4.1 |
| Hosted score sharing | **Rejected** | No server, no hosting. Analyse and cache locally. Streaming is permanently `Unscored`; owned music via a library index is the product — see §4.2 |
| Score cache | **SQLite** (`rusqlite`, bundled), single file beside the exe | Reversed from `redb` after measuring both. redb's format broke twice in two years against data worth ~2 h of analysis to rebuild, and is opaque to every tool. The C-compiler objection measured 53 s of one-time build. See spec §5.2 |
| Beat + downbeat tracking | **`beat-this`** (rten backend) | Rust port of the ISMIR 2024 tracker; reports verified F-measure parity with the PyTorch reference. Emits beats, downbeats and beat numbers — maps 1:1 onto the score format |
| ML runtime | **`rten`**, with `ort` as cross-check | Ships inside `beat-this`. Do not add a second framework |
| GGUF / `candle` | **Rejected** | GGUF is a ggml container for quantized LLMs; no MIR model is published in it. `candle` is capable but redundant when `rten` is already in the tree |
| Source separation (Demucs) | **Not used** | Rust ports exist (`demucs-rs`, `charon-audio`), but a per-track stem separation pass is a large cost for section *names* |
| Functional segmentation | **Deferred to M6** | No trustworthy Rust option. `oximedia-mir` advertises it but its breadth-to-adoption ratio does not survive scrutiny. Derive boundaries from novelty + RMS instead |
| Python `allin1` sidecar | **Optional, off the critical path** | Was the primary analysis path in spec §8.1. Now an M6 enrichment supplying segment labels only |
| Yandex integration | **`yandex-music`** (vyfor) as an ID resolver | Mature: 0.7.0, ~15K downloads, maintained since June 2024. REST-only, so it cannot serve as a `Source` — see §5.8 |
| Yandex realtime (`yamuse`) | **Chosen 2026-08-15** | Replaces `yandex-music`. Justified not by ynison's precision but by Phase 0.5: Yandex Browser publishes nothing to SMTC, so without it the dancer cannot tell a track is playing at all. Still young — feature-flag it. See §5.8 |

### 1.1 What these decisions delete

- The NATTEN-on-Windows build barrier (spec §16) — gone from the main path.
- The prebuilt-sidecar-bundle and WSL mitigations — no longer needed.
- Spec §17.1 (is Spotify's `audio-analysis` still open?) — no longer worth resolving.
  We compute our own grids.

---

## 2. Ordering principle

Two changes from spec §14, and one thing deliberately kept.

**Analysis moves early (old M5 → new M2).** It was scheduled late because it meant
Python, PyTorch and a source build. It is now a crate call, so it lands before the
scheduler.

This is not merely a convenience. The old plan judged the anticipation scheduler
against a *hand-written* score — a grid authored to be correct. Real grids carry
jitter, occasional half-beat errors and tempo drift, which is exactly where
anticipation either holds up or falls apart. Testing the scheduler against real
analyzer output is a sharper test and now costs nothing to reach first.

**M3 is the gate.** Everything before it is a sprite player with a metronome;
everything after is plumbing to feed it. Spec §14's instruction — do not start SMTC
until anticipation looks right against a local file — is the most valuable line in
the document, and moving real scores earlier strengthens it.

If M3's A/B is unconvincing, stop and reconsider the premise rather than building
five more milestones on top of it.

**Kept: no audio subsystem and no network until M4.** M0–M3 are the interesting
work and none of it needs a device or a token.

---

## 3. Phase 0 — De-risk

Two unknowns are load-bearing. Both are cheap to answer and both change the design
if they fail. Answer them before writing production code.

### 0.1 `beat-this` validation — **DONE 2026-08-15: pass, with one caveat**

Harness and full results: [spikes/beat-this-probe](spikes/beat-this-probe/README.md).

Builds clean on `stable-x86_64-pc-windows-gnu`, edition 2024, 104 packages, 40 s,
no system libraries. Beat grids run 1.4–2.2% inter-beat deviation on steady
material — comfortably inside what the clock can track. Cost is 41–74x realtime, so
analysis finishes far inside a track's own duration. Non-musical input returns an
empty grid rather than a fabricated one. The tracker is stateless across calls, so
one instance can be reused.

Three findings that change the design:

1. **Meter is not always 4/4.** A waltz in the test set was correctly identified as
   3/4 at 97% consistency. The score format must carry meter rather than assume
   four — see spec §5.
2. **Downbeats are weaker than beats.** One track had a rock-steady beat grid but
   split 29 two-beat bars against 30 four-beat ones — spurious downbeats halving
   bars. Since §11.3 changes moves on downbeats only, the scheduler must fit bar
   phase to the downbeat *candidates* rather than trust each one. Added to M3.
3. **Model weights are not bundled** in the published crate. ~270 KB mel front end
   plus ~10 MB small model (or ~83 MB full) must ship with the app. Added to M5.

Remaining caveats: four tracks, no independent annotations, small model rather than
the full-accuracy one. This establishes plausibility and stability, not accuracy
against ground truth. Widen the corpus if M3 shows scheduling problems that trace
back to the grid. The upstream port is also AI-assisted per its own README, with CI
and claimed parity tests — the reason this spike existed.

### 0.2 Per-pixel alpha — **DONE 2026-08-15: softbuffer dropped, layered path passes**

Harness and full results: [spikes/alpha-probe](spikes/alpha-probe/README.md).

The question was framed as "does winit's transparency path give per-pixel alpha".
It turned out to be the wrong layer. **`softbuffer` cannot express alpha at all**:
its documented pixel format is `00000000RRRRRRRRGGGGGGGGBBBBBBBB`, top 8 bits
specified as zero, and its Win32 backend presents with `BitBlt(SRCCOPY)`, an opaque
copy. No winit setting routes around that.

**winit + `UpdateLayeredWindow` passes cleanly** — worst channel delta of 1 across
a five-step alpha ramp measured against screen captures, with all four extended
styles (`LAYERED`, `TOOLWINDOW`, `NOACTIVATE`, `TRANSPARENT`) applied on a
winit-created window. Present cost is 0.066–0.112 ms/frame at 128–512 px, i.e.
0.4–0.7% of a 16.7 ms budget, so the per-frame GDI blit is a non-issue.

Consequences, folded into M0:

- Drop `softbuffer` (spec §12, §15).
- Premultiply sprite cells at load — `UpdateLayeredWindow` requires premultiplied
  BGRA, so doing it per frame would be waste.
- Presentation is not `WM_PAINT`. The call replaces the whole window surface and
  winit's redraw path goes unused.
- Click-through is `WS_EX_TRANSPARENT`, toggled off for dragging.

Caveat: one machine, Windows 10 build 19044. Re-check on Windows 11 and on a
high-DPI multi-monitor setup before calling M0 done.

### 0.3 Housekeeping — **DONE 2026-08-15**

Rust edition: **2024**. Local toolchain is 1.95.0, well past the 1.85 floor, and
0.1 built and ran the full dependency tree on it. Spec §1's 2021 / 1.75+ predates
this dependency set.

### 0.4 Toolchain ABI — **DECIDED 2026-08-15: GNU**

`stable-x86_64-pc-windows-gnu`, pinned in [rust-toolchain.toml](rust-toolchain.toml).

MSVC was investigated and dropped on cost. Visual Studio Build Tools 2026 installs
`cl.exe` and `link.exe`, but Rust's MSVC target also links every binary against
system import libraries — `kernel32.lib`, `ntdll.lib`, `ws2_32.lib`, `dbghelp.lib`
— which ship in the **Windows SDK**, not with the VC tools. That is a ~2 GB
component with no smaller option exposed in the installer, bought for benefits we
may never collect.

**Everything this project needs is verified on GNU:**

| Surface | Status |
|---|---|
| `beat-this` + `rten` | Builds and runs (0.1) |
| `UpdateLayeredWindow` via `windows` | Builds and runs, alpha exact to ±1 (0.2) |
| `rusqlite` (`bundled`, C compile via mingw gcc) | Builds and runs, 53 s |
| WinRT / SMTC | Activates and enumerates (0.5) |

**Accepted risk.** The `ort` cross-check from 0.1 is materially weaker here:
prebuilt `libonnxruntime` binaries are MSVC-built, so if `beat-this` ever degrades
we would have to build onnxruntime for mingw or install the SDK at that point.
Phase 0.1 passed, so this is a contingency, not a plan — but it is no longer cheap.

#### Two environment traps, if MSVC is ever revisited

Both cost time to diagnose and neither error message points at the cause.

1. **`link.exe` is shadowed by busybox.** On this machine PATH resolves `link.exe`
   to `scoop\shims\link.exe`, which is busybox's hardlink utility:
   `path = "...usybox\currentusybox.exe"`, `args = link`. Handed MSVC linker
   arguments it fails with `link: extra operand ... Try 'link --help'` — which
   looks nothing like a toolchain problem. It shadows in PowerShell as well as Git
   Bash.
2. **rustc falls through to PATH when MSVC detection fails.** Normally rustc
   locates the MSVC toolchain and prepends its paths, which would beat the shim.
   With the SDK missing, detection fails and it invokes a bare `link.exe` — so
   trap 1 only becomes visible because of trap 2. Fixing the SDK may resolve both.

**SDK version note, if it is ever installed:** the SDK version does not set the
minimum supported Windows version. Building against the 24H2 SDK (10.0.26100.x)
still produces binaries that run on our Windows 10 build 19041 floor — what matters
is which APIs are called, not which SDK is linked. The `windows` crate also carries
its own WinRT metadata, so SMTC needs no SDK headers, only the import libraries.

### 0.5 SMTC on GNU — **PASS 2026-08-15**

Harness and full results: [spikes/smtc-probe](spikes/smtc-probe/README.md).

Every call the source adapter needs works on the GNU toolchain: session
enumeration, media properties, playback info, timeline, non-zero
`LastUpdatedTime`. WinRT is not a problem here.

API notes for M4: the blocking accessor on `IAsyncOperation` is `join()`, not
`get()`; `PlaybackStatus` 4 = Playing.

Three findings outweigh the pass itself.

**`Position` was stale by 59.6 seconds.** Edge reported `0.019s` for a track
actually 59.7 s in — the value was captured at playback start and never refreshed.
Spec §6.2 predicted this; the magnitude did not. Trusting `Position` with
`Instant::now()` would have put the dancer a full minute out on a two-minute track.
The `BeatClock` anchor design is not an optimisation — without it SMTC data is
unusable. M1 should keep a fixture built from these exact numbers.

**Yandex Browser does not publish to SMTC at all.** Zero sessions with Yandex Music
actively playing, repeatedly; Edge on the same machine returns one immediately. No
OS-level cause found — no policy keys, default browser flags, media features
present. For a user on such a player the dancer never learns anything is playing:
`Idle`, not `Unscored`. This changes the `yamuse` calculus (§5.8).

**Re-run the same day with the Yandex Music *desktop* app: it publishes normally.**
So the blind spot is the browser, not the service. Staleness reproduced at 87 s in
this second, unrelated application, and the extrapolation was shown to track wall
clock exactly across a 33 s gap. New M4 constraint: `SourceAppUserModelId` came back
as `Яндекс Музыка.exe`, so the allowlist must handle non-ASCII, locale-dependent
identifiers. Details in the probe README; consequences for `yamuse` in §5.8.

**Titles vary wildly across sources** — the first track tested came back as title
`"Blur - Song 2 (Official Music Video)"`, artist `"Blur"`. The initial reading was
that this demands aggressive normalisation. That was wrong: canonicalising content
merges different masters (`(Radio Edit)` is a different recording with a different
grid), which is a false positive and worse than a miss. Hash the raw strings with
encoding-level normalisation only, and verify duration on match — spec §5.1.

Follow-up before M4 exits: sweep which players actually publish — Chrome, Firefox,
Opera, Spotify desktop, AIMP, foobar2000, VLC. That list determines how much of
M4's value is real.

---

## 4. Milestones

| # | Deliverable | Exit criterion |
|---|---|---|
| **M0** | Window + sprite playback — **done 2026-08-15** | FAOSDance parity: loads an existing sheet + `.txt`, fixed-fps loop, transparent, click-through, draggable |
| **M1** | Local file source + BeatClock — **done 2026-08-15** | Hand-written score JSON drives a beat-locked dance against a local WAV; no visible drift over 3 min |
| **M2** | Real analyzer + score cache — **done 2026-08-15** | `beat-this` produces a score from a local file, cached to disk, indistinguishable in use from the hand-written one |
| **M3** | **Anticipation scheduler** — built 2026-08-15, **A/B not yet judged** | `impact_cell` respected; A/B against M1 shows a difference visible to someone not told what changed |
| **M4** | SMTC source — **done 2026-08-15** | Identity, position and pause/resume from Spotify desktop; correct freeze and resume-on-downbeat |
| **M5** | Tray UI, config, packaging | Installable by a stranger |
| **M6** | *Optional:* segment labels, Spotify, Yandex resolver | Only if M5 shows unlabelled pools are the visible gap |

Two milestones were cut, not renamed. The old M5 (WASAPI loopback) and M6
(learn-on-second-listen) are gone with the audio subsystem — see §4.1.

### 4.1 What cutting audio removed, and what it cost

Capture was never the sync mechanism — spec §7 said so in its own first line, but
the design had not followed the sentence to its conclusion. Grids come from
offline analysis, position comes from the source adapters. The four purposes
capture served each dissolved:

| Purpose | Resolution |
|---|---|
| Silence watchdog | Guards a narrow class SMTC already reports, and on a full mix fails toward false *non*-silence — least reliable where most needed |
| Offset calibration | Real value, but a manual nudge reaches it. Sync error is highly perceptible, so users trim by eye in seconds, once per source app |
| Recording for analysis | Opt-in, off by default, prohibited by streaming ToS, and corrupt on a contaminated mix |
| Reactive fallback | Needed live DSP. Replaced by `Unscored`: default row, fixed fps, honest about knowing nothing |

The decisive factor was per-process capture, which was the mitigation for every
contamination problem and needs build 20348+ — Windows Server 2022's number. Retail
Windows 10 ends at 19045, so no consumer Win10 build has it. With Windows 10
first-class, that left a whole subsystem whose best case was "sometimes less wrong".

**Deleted:** the `dancer-audio` crate, two milestones, the `wasapi` and `realfft`
dependencies, the recording legal exposure, the `Reactive` and `Recording` states,
and the entire contaminated-mix design problem.

**Cost:** streamed tracks get no grid — absent, not degraded, and now permanent
(§4.2). Owned music is unaffected and is the product. Spotify and Yandex know
identity and position but have nothing to dance to, so they run `Unscored`.

### 4.2 What the product actually is

Cutting audio (§4.1) and rejecting hosted score sharing together settle the scope,
so it is worth stating positively rather than as a list of removals.

**dancer-rs syncs to music you own, played through whatever player you already
use.** Point it at your music folders; it analyses them and remembers. Then when
you press play in foobar2000, AIMP, VLC or anything else that talks to SMTC, it
recognises the track and locks to its grid.

That path needs no account, no service, no network and no permission from anyone.
Everything is analysed locally and cached locally in one file.

The mechanism is a two-step: analysis keys grids by file, and SMTC reports
`(title, artist)` with no path — so a `library` table maps normalised title/artist
back to an analysed file (spec §5.1, §6.2). M4 is where those meet, and the
normalisation is the fragile part worth testing hard.

**Streaming is `Unscored` and stays that way.** Spotify and Yandex give identity,
position and pause/resume, so the dancer tracks play state correctly — it just has
no grid, and runs a fixed-fps loop. Three routes to a grid were each considered and
each rejected on their merits: recording (§4.1), a hosted score cache (§6.4), and
track downloads (§5.9). Nothing is pending; this is the answer.

Say so in the UI. A user whose Spotify does not sync should learn that it is a
decision, not a failure.

---

### M0 — Window and sprite playback

**Crates:** `dancer-sprite`, `dancer-render`, `dancer-app`

- Workspace skeleton per spec §3.1.
- Sheet loader: PNG ÷ 8 columns, rows from the `.txt` line count. Synthesise
  `row_0..row_n` and treat the last row as `Held` when no `.txt` exists.
  Reference: `SpriteSheet.kt` upstream is 64 lines; this is not a port.
- Optional `.toml` manifest (spec §4.2) parsed but only `cell_width`/`cell_height`/
  `default_row` consumed for now.
- Pre-slice cells into `Arc<[RgbaImage]>` at load, **premultiplied** — the present
  path requires premultiplied BGRA (Phase 0.2).
- Window: winit for creation, events, DPI and monitors; `UpdateLayeredWindow` for
  presentation. No `softbuffer`, no `WM_PAINT`.
- Extended styles: `WS_EX_LAYERED`, `WS_EX_TOOLWINDOW`, `WS_EX_NOACTIVATE`,
  `WS_EX_TRANSPARENT`. All four verified applying on a winit window in Phase 0.2.
- Click-through toggle flips `WS_EX_TRANSPARENT`; drag switches to the `Held` row
  and bypasses everything else.
- Persist position as (monitor id, normalised x/y), not absolute pixels.
- Re-check alpha and DPI on Windows 11 and a multi-monitor setup before calling
  this done — Phase 0.2 ran on one machine.

**Exit:** an existing FAOSDance sheet loads and loops, transparent and draggable.

---

### M1 — Local file source and BeatClock

**Crates:** `dancer-score`, `dancer-clock`, `dancer-source` (file adapter)

- `Score` types and JSON (de)serialisation per spec §5.
- Local file adapter: no playback, just a simulated transport over a WAV path.
  Deterministic, no streaming service in the loop.
- `BeatClock` with anchor/rate/offset per spec §9.
- Correction policy per §9.1: freeze on pause, hard re-anchor past
  `SEEK_THRESHOLD`, otherwise slew. **Never step position while `Locked`.**
- `AppEvent` enum and the crossbeam channel into the render loop.
- Frame timing from *local* beat intervals, not global BPM.

**Exit:** hand-written score drives a beat-locked dance for 3 minutes with no
visible drift.

**Why hand-written first:** a known-correct grid isolates clock bugs from analyzer
bugs. Keep these fixtures permanently as regression tests.

#### Done 2026-08-15

The exit criterion is a test, not a judgement call:
`playback::tests::no_visible_drift_over_three_minutes` runs a player drifting
0.02 % fast, polled every 3 s with each reading 2 s stale, and asserts that across
3 600 samples **no frame ever shows the wrong cell** and worst-case position error
stays under 20 ms. Fixtures live in `crates/dancer-score/fixtures`, regenerated by
`cargo run -p dancer-score --example gen-fixtures`.

Four things worth carrying forward:

**Cell selection is derived, not accumulated.** `loop_progress` reads the cell out
of the beat grid each frame rather than advancing a counter. This is why a dropped
frame skips a cell instead of shifting phase, and it is what makes drift a property
of the clock alone — there is no second place for it to accumulate.

**Media time and output time must not be mixed.** `position()` subtracts the
output-latency offset; corrections compare against `media_position()`, which does
not. Comparing an observation against the offset-adjusted estimate would make the
clock chase its own latency compensation and settle a full `offset` out of place.
There is a test pinning this specifically.

**Staleness is not coarseness.** Phase 0.5's 87-second-old SMTC reading was
*precise*, just old, so the fix is entirely in the pairing: carry the instant the
value was true and the clock needs no notion of staleness at all. The file source
simulates it with `--stale`, and doing so immediately exposed a start-up artifact —
a reading backdated before the transport began — which is now clamped.

**§9.1's middle band contradicts §9's "never step while Locked".** Implemented
literally, with a distinct `Correction` variant so a step is never silent. M3
resolves it by deferring the step to a loop boundary. See spec §9.1.

Two deliberate divergences from the spec, both recorded there: the `Source` trait
is synchronous (spec §6.1) and the `BeatClock` keeps the score so it can answer
phase queries, as spec §9 sketches it.

---

### M2 — Real analyzer and score cache

**Crates:** `dancer-analyze`, `dancer-score` (cache store)

- Wire `beat-this`: `load_audio()` → `analyze_audio()` → `BeatAnalysis`.
- Map its output onto the score format: `BeatAnalysis::beats` → `beats`,
  `beat_counts()` → `beat_positions`, `downbeats` → `downbeats`,
  `calculate_bpm()` → `bpm`.
- **Carry meter.** Phase 0.1 found correct 3/4 detection on a waltz; do not assume
  four. Infer modal bar length from `beat_counts()` and store it.
- **Fit bar phase rather than trusting each downbeat.** Phase 0.1 found spurious
  downbeats halving bars on an otherwise clean grid. With a stable inter-beat
  interval, fit a regular bar grid to the candidates and reject outliers.
- Derive `energy` from RMS over the beat grid.
- Ship the ONNX weights: ~270 KB mel + ~10 MB small model. Not bundled in the
  crate. Decide small vs full (~83 MB) on measured quality, not by default.
- Emit `source: "beat-this"`, with a confidence heuristic.
- Cache into a single SQLite file, `scores.db`, beside the exe (spec §5.1, §13).
  Two tables: `scores` keyed `{source}:{track_id}`, and `library` keyed on
  normalised `(title, artist)` → path. Namespaced per source.
- **Set `PRAGMA user_version` from the start** and check it on open. The score
  shape will change; migrating from version 1 is far easier than retrofitting a
  version onto files already in the wild.
- `segments` may be empty — everything downstream must tolerate that.
- Run analysis off-thread; `ScoreReady` arrives as an `AppEvent`.

**Exit:** a real analysed score drives the dance as convincingly as M1's fixture.

#### Done 2026-08-15

End to end on a 90 s 124 BPM test track: analysed in **2.4 s (37× realtime)**, 186
beats, meter 4, confidence 0.998, `Idle → Identifying → Locked`. Second run hit the
cache and reached `Locked` with no model load at all. `scores.db` is 20 KB for one
score, `user_version = 1`, both tables populated.

**`calculate_bpm` reported 125.0 against a true 124.0** — and the beat *count* was
exactly right (186 beats is what 124 BPM gives over 90 s). So the grid was correct
while the summary tempo was 0.8 % out. This is spec §11.1 demonstrated rather than
argued: anything deriving frame timing from `bpm` would have drifted a beat every
two minutes on a grid that was never wrong. `bpm` is a label, not a clock.

**The bar phase is fitted, not trusted.** `fit_bar_phase` scores every phase
`0..meter` by how many downbeat candidates it explains and takes the winner, so
Phase 0.1's spurious downbeats lose a vote instead of halving bars. Tested against
that exact failure, and against a pickup bar where the track does not start on a
downbeat.

**Confidence is a heuristic over proxies, and says so.** Nothing at runtime knows
whether the beats are in the right places. What it can check is coverage and
self-consistency, which catch the failure that matters — a partial detection
presented as solid. Weighting came from Phase 0.1: inter-beat deviation counts for
little, because the waltz measured 12.5 % and its grid was good, and the clock reads
local intervals anyway. Tests pin both Phase 0.1 tracks against the 0.6 gate.

**`beat_energy` was added to the score.** Spec §5 puts energy on segments, but
scores from §8.1 have none — which is most of them — and M3's move selection for
unlabelled scores keys on energy tier. Per-beat RMS, normalised against the 95th
percentile so one clipped transient cannot flatten everything else into the floor.
Writing the test for that found the percentile index rounding up to the maximum on
short tracks, which made it its own normaliser; capped at `n-2`.

**Cues are still empty.** Deriving `build`/`drop` from novelty on the beat grid
belongs with the scheduler that consumes them (M3), not here.

**Risk:** `beat-this` quality (settled in Phase 0.1). Analysis wall-time should
finish well before a typical track does — measure it.

---

### M3 — Anticipation scheduler ★

**Crate:** `dancer-choreo`

The milestone the project exists for.

- Lookahead window (~2 s) and the `ScheduledMove` queue per spec §11.2.
- `start_time = target_beat − (impact_cell × frame_duration) − render_latency`.
- **Measure `render_latency`**; do not assume 16 ms.
- Move selection per §11.3, with the fallback path below.
- Change moves on downbeats only, unless a cue forces otherwise — but against the
  **fitted** bar phase from M2, not raw downbeat detections. Phase 0.1 showed raw
  downbeats are the least reliable part of the analysis, and this is the one place
  the scheduler depends on them.
- Respect meter: `beats_per_loop` is relative to the score's bar length, not a
  hardcoded four.
- Non-loopable rows return to `default_row`.

**Segment-label fallback.** Spec §11.3 filters rows by `pools` containing the
segment label, but M2 scores have no labels. Selection must therefore key on
**energy tier and boundary position**, treating labels as an enrichment when
present. This is the mechanism that lets the project ship without segmentation:
the scheduler needs to know energy rose at a downbeat, not that the section is
called a chorus.

**Exit:** A/B against M1. Show both to someone who has not been told what changed.
If they cannot tell, the premise needs re-examining before M4.

#### Built 2026-08-15 — the exit criterion is still open

The mechanism works and is measured. Whether it is *visible* is a judgement only a
person can make, and nobody has made it yet. **Middle-click toggles anticipation on
and off at runtime**, on the same track, which is the only honest way to run that
comparison; `--no-anticipate` starts in M1 mode.

**The gate is blocked on M4, not on M3.** The A/B cannot be judged yet because
there is nothing to hear: the file source is a *simulated* transport (spec §6.5) and
the app has no audio subsystem by design (§7). Watching a sprite move against
silence says nothing about whether its accents land on the beat. M4 supplies the
missing half — the user plays the track in their own player, SMTC reports position,
and M2's library index matches it to the analysed score. **Judge M3 after M4**, with
audible music, before building anything on top of the premise.

Measured on a 124 BPM track with the default sheet:

| Row | `impact_cell` | `beats_per_loop` | Predicted lead | Measured |
|---|---|---|---|---|
| spin | 4 | 2 | 484 ms | 484 ms |
| bounce | 3 | 1 | 181 ms | 184 ms |

The 3 ms difference is measured render latency. Bars landed 1.935 s apart, which is
four beats at that tempo.

**`render_latency` is measured, but only the half that can be.** Present cost is
timed per frame and kept as a rolling median; the compositor's delay between
`UpdateLayeredWindow` returning and photons leaving the panel is **not observable
from inside the process**. That part is a constant, and a constant is exactly what
§9.2's offset slider absorbs — so mis-estimating it is not fatal, while getting
`impact_cell × frame_duration` wrong would be, and that one is exact.

**Running it found a flat-energy failure.** The analysed test track sat at 0.89
energy throughout, which put exactly one row of the default sheet inside the ±0.35
window — so the dancer repeated a single move for the whole track, which is the
FAOSDance behaviour this project exists to beat. Loudness-war masters do the same to
real music, so it is not only a synthetic-signal problem. Selection now widens to
the nearest few rows when the strict window leaves fewer than two, capped at twice
the window so a full-energy spin cannot land in a quiet intro. After the fix the
same track alternates spin and bounce.

**A startup gap, also found by running it.** `plan` skipped a bar starting exactly
at the current position, so a track beginning on a downbeat had nothing scheduled
for its entire first bar. The planning window now reaches back rather than starting
at the playhead.

**Move selection is seeded and the seed is logged.** A choreography judged by eye
has to be reproducible to be debuggable, and `rand` would not have given that for
free.

Deliberately deferred: hiding a hard re-anchor at a loop boundary, which is the
resolution to spec §9.1's contradiction with §9 and needs the scheduler that now
exists. It is a real remaining defect, not a finished item.

---

### M4 — SMTC source

**Crate:** `dancer-source` (smtc adapter)

- `GlobalSystemMediaTransportControlsSessionManager` via the `windows` crate.
- Subscribe to `MediaPropertiesChanged`, `PlaybackInfoChanged`,
  `TimelinePropertiesChanged` rather than polling.
- **Anchor on `LastUpdatedTime`, never `Instant::now()` at read time.** `Position`
  refreshes only on state change.
- Filter by `SourceAppUserModelId` against the configured allowlist.
- **Library matching is the point of this milestone.** SMTC gives `(title, artist)`
  and never a path, so hash it and look it up in the `library` table from M2. That
  lookup connects "the user pressed play in foobar2000" to "we analysed that file
  last week" — it is what makes owned music work through the user's own player
  (spec §8.3).
- **Hash the raw strings; do not canonicalise content.** Trim, NFC and casefold
  only. Stripping `(Radio Edit)` or `(Official Music Video)` merges different
  masters with different grids — a false positive, which is worse than the false
  negative of a miss (spec §5.1). Test that the *unsafe* merges do not happen, not
  just that the safe ones do.
- Verify duration on match, ±2 s. A hash hit with a disagreeing duration is a miss.
- Scan the configured library folders and analyse what is found, on a worker.
- Handle sessions publishing no timeline at all — drop to `Unscored`, since there
  is no audio fallback.
- State machine per spec §10, including resume-on-next-downbeat.

**Exit:** playing an analysed local file through an ordinary player (foobar2000,
AIMP, VLC) locks the dancer to its grid. Spotify drives identity, position and
pause/resume correctly, with no mid-move cuts on pause, and sits in `Unscored`.

#### Done 2026-08-15

Reads the live session: `Rhythm Is A Dancer` / `SNAP!` from `Яндекс Музыка.exe`,
with a **measured 2.0 ms read cost**. Resume-on-next-downbeat, deferred in M1 for
want of a scheduler, is now implemented and tested.

**It polls at 500 ms rather than subscribing, and the measurement is why.** Spec
§6.2 asks for `MediaPropertiesChanged` and friends. The correctness argument for
subscribing does not apply here: every reading carries its own `LastUpdatedTime`, so
re-reading unchanged state produces a zero error and does nothing. What polling
costs is *track-change latency*, bounded by the cadence — 2.0 ms per read at 500 ms
is 0.4 % of one core. Subscriptions remain better and want WinRT event handlers on a
thread with the right COM apartment, which Phase 0.5 never validated.

**Two gaps found by running it, both of which made the whole path dead:**

*Analysis was indexing under the filename, so nothing ever matched.* SMTC reports a
file's own tags — spec §5.1 said exactly this and the M2 implementation ignored it,
using the filename stem with an empty artist. `Rhythm Is A Dancer` / `SNAP!` could
never match `01 - rhythm is a dancer` / `""`. Now reads tags with symphonia, already
in the tree as beat-this's decoder, falling back to the filename as players do for
untagged files.

*There was no way to fill the cache.* Only `--audio <one file>` existed, so the
library was empty and every SMTC track missed — correct behaviour, useless product.
`--scan <DIR>` now walks a folder and analyses it, skipping anything already cached
so an interrupted scan resumes for free.

**What SMTC is and is not worth.** It gives identity, position and pause/resume for
*everything* — that is `Unscored`, and it is genuinely better than FAOSDance, which
never knew whether music was playing at all. It gives `Locked` only for music that
exists as a file and has been analysed. Streaming has no file, so it stays
`Unscored` permanently (§4.2, §8.3). That limit was set when audio capture was cut,
and no amount of work on this adapter moves it.

**Note:** streaming being unscored is the design (§4.2), not an unfinished edge.
Surface it in the UI so it does not read as a bug.

---

### M5 — Tray, configuration, packaging

**Crate:** `dancer-app`

- `config.toml` per spec §13, hot-reloaded via `notify`.
- Artwork hot reload.
- `tray-icon`: source selection, click-through toggle, offset nudge (spec §9.2 —
  this is now the only latency correction, so make it easy to reach).
- Ship the `beat-this` ONNX weights: ~270 KB mel + ~10 MB small model. Not bundled
  in the crate; pin a checksum.
- Packaging and a neutral default sheet. **Do not ship FL-Chan** — Image-Line's
  artwork must not be redistributed (spec §1.3). Credit FAOSDance (MIT) for the
  sheet format.

**Exit:** a stranger can install and run it.

---

### M6 — Optional extras

Ordered by expected value, none on the critical path.

1. **Segment labels.** Only if M5 shows unlabelled pools are the visible gap.
   Cheapest route is the Python `allin1` sidecar (spec §8.1) as pure enrichment:
   newline-delimited JSON over stdio, supplying `segments` and `cues` on top of a
   grid we already have. NATTEN remains a Windows barrier, but now for an optional
   feature rather than the product.
2. **Yandex track ID resolver.** `yandex-music` (vyfor) as a `TrackIdResolver`,
   *not* a `Source` — see §5.8 below.
3. **Spotify adapter.** `rspotify` OAuth PKCE. Buys canonical IDs and coverage when
   playback is on another device where SMTC sees nothing. Costs an auth flow.

---

## 5. Notes on specific decisions

### 5.8 Why Yandex is a resolver, not a source

`yandex-music` exposes 13 REST submodules — `queue`, `rotor`, `track`, `playlist`
and others — with no websocket or ynison module. Its `queue` returns queue
*contents*, not continuous playback position. It therefore cannot satisfy the
`Source` contract in spec §6.1: no `position`, no reliable `playing`, no tight
`observed_at` pairing.

*Confidence: high but unverified — docs.rs coverage is 13.84%, too thin to be
conclusive. Confirm in the source before building against it.*

The useful division of labour is a hybrid:

| Concern | Provider |
|---|---|
| Position, playing, timeline anchors | **SMTC** (event-driven, `LastUpdatedTime`) |
| Stable track ID, canonical metadata | **`yandex-music`** (REST lookup) |

This is strictly better than either alone. SMTC's weakest point is identity — the
normalise-trim-casefold-strip-`- Remastered` hashing of spec §6.2 is a pile of
heuristics that will collide. Resolving a real Yandex track ID turns the cache key
from "mostly reliable" into "correct", and cache-key correctness is what makes
learn-on-second-listen work at all.

Failure degrades gracefully: lose the resolver and you fall back to hashed strings,
not to nothing.

**Decided 2026-08-15: `yamuse`.** It carries ynison, so Yandex can be a real push
`Source` — continuous position over a WebSocket rather than SMTC's event-driven
timeline — and it also exposes catalogue, so it covers identity too. That makes it
a replacement for `yandex-music` rather than a companion to it.

Two things to carry into M6 with it.

**It is young.** 0.3.2, first published 2026-07-29, five releases in six days, 75
downloads at the time of choosing. Keep it feature-flagged, keep SMTC covering the
same player, and do not let a break take down the binary. `yandex-music` (vyfor,
~15K downloads, maintained since June 2024) remains the fallback for the resolver
role if yamuse stalls.

**Its justification changed on 2026-08-15 — for the better.** The original case was
precise position via ynison, and that case was weak: position precision only matters
in `Locked`, and streamed tracks are permanently `Unscored` (§4.2), where a tighter
anchor changes nothing visible.

Phase 0.5 then found that **Yandex Browser publishes nothing to SMTC** — zero
sessions with Yandex Music actively playing, while Edge on the same machine works
fine. So for that setup the choice is not "SMTC with coarse position" versus
"ynison with fine position". It is **presence versus nothing**: without `yamuse`
the dancer does not learn a track is playing at all and sits `Idle`.

That is a much stronger argument than the one it replaces, and it was the user's
instinct before the evidence existed.

Two things still to carry into M6:

- It remains young — keep it feature-flagged, and do not let a break take the
  binary down.
- It ships the download endpoints §5.9 fences off, so the structural guard matters.

**That open question closed the same day, against `yamuse`.** The Yandex Music
desktop app publishes to SMTC normally — full metadata, timeline and a live anchor.
The blind spot is Yandex *Browser* specifically.

So the presence argument now covers only users who play in the browser and will not
install the desktop app. That is a real set, but a much smaller one than "Yandex
users", and the alternative costs us nothing: no dependency, no auth flow, no
undocumented API, no download endpoints to fence off (§5.9). `yamuse` stays
feature-flagged and unbuilt; M6 should ask whether it earns its place at all rather
than assuming it does.

Worth noting the reasoning here has now swung twice on evidence — weak (precision),
then strong (presence), then weak again (presence, but only in the browser). The
decision was never wrong to defer; it was right to keep it feature-flagged and
unbuilt until M6, which is exactly why this costs nothing to revise.

### 5.9 A capability to fence off

`yamuse` advertised lossless downloads, and other Yandex wrappers expose similar
endpoints. Fetch the file, analyse it directly, get a perfect grid.

**Cutting audio capture made this more tempting, not less.** Recording was the
legitimate — if awkward — route to a grid for a streamed track. With it gone
(§4.1), streaming is idle-only, and a download endpoint sitting right there in a
dependency we already ship is the obvious-looking way to close that gap.

It should not be closed that way. Recording was local capture of audio already
playing on the user's own machine, off by default and disclosed; this is retrieving
masters from a CDN. Losing the weaker option does not upgrade the stronger one.

So the guard matters more than before: keep `dancer-source` structurally unable to
reach download endpoints, rather than relying on nobody reaching for them. The
there is no sanctioned alternative: streamed tracks stay `Unscored`. That is the
decision, not a gap awaiting a workaround.

#### Reversed 2026-08-15 — see spec §6.4.1

The owner overruled this, and the argument that carried it was the one this section
had underweighted: **the gap is not at the edges, it is the whole product for a
streaming user.** `Unscored` means every part of M1 through M3 sits idle — the
feature does not exist for them. "That is the decision, not a gap awaiting a
workaround" was true about the mechanism and wrong about the size of what it cost.

The reasoning above still governs the *shape*, and the shape is what makes the
owner's line — the app can fetch, the user initiates, nothing is redistributed —
structural rather than a claim: fetch, analyse, **delete**; retain a grid, not
audio; take the **lowest** bitrate offered because the grid cannot tell the
difference; one track at a time, never a batch; off unless a token is supplied.
Spec §6.4.1 has the full list and the reasoning for each.

The guard also moved rather than vanished: `dancer-source` still cannot reach a
download endpoint. The capability lives in `dancer-yandex`, behind a feature flag,
reachable only from the score-lookup path.

---

## 6. Open questions

Carried forward, with the resolved ones struck.

1. ~~Is Spotify's `audio-analysis` endpoint open to new apps?~~ **Moot.** We compute
   our own grids.
2. ~~Should `Reactive` mode exist in v1?~~ **Resolved: no.** It needed live DSP,
   which went with the audio subsystem (§4.1). Replaced by `Unscored`: default row,
   fixed fps, honest about knowing nothing.
3. Is 8 cells worth keeping as a hard constraint? **Keep it**, with the manifest
   free to override later. Costs nothing, buys the existing sheet library.
4. ~~Should scores be shareable via a hosted cache?~~ **Resolved: no.** Everything
   analysed and cached locally. No server, no hosting, no fetch/upload paths. The
   cache is one file, so copying it to a friend happens to work — a consequence of
   the storage choice, not a feature. Streamed tracks stay `Unscored` (§4.2).
5. **New:** Rust edition and MSRV. See §0.3.
6. ~~Does Yandex need ynison push-position?~~ **Resolved: `yamuse` chosen**, then
   substantially undercut on 2026-08-15 when the Yandex Music desktop app turned out
   to publish to SMTC normally. Streaming is permanently `Unscored`, so precise
   position buys nothing visible, and presence is now only missing in the *browser*.
   The live question at M6 is whether to build the Yandex adapter at all. See §5.8.
