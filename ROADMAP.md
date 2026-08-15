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
| Windows 10 | **First-class, not degraded** | Large share of the install base. No feature may require a Win11-only API. Applying that rule is what removed the audio subsystem — see §4.1 |
| Audio capture | **Cut from v1** | No WASAPI, no loopback, no recording, no `dancer-audio`. Every purpose it served either dissolved or has a cheaper answer. Costs streaming support; see §4.1 |
| Beat + downbeat tracking | **`beat-this`** (rten backend) | Rust port of the ISMIR 2024 tracker; reports verified F-measure parity with the PyTorch reference. Emits beats, downbeats and beat numbers — maps 1:1 onto the score format |
| ML runtime | **`rten`**, with `ort` as cross-check | Ships inside `beat-this`. Do not add a second framework |
| GGUF / `candle` | **Rejected** | GGUF is a ggml container for quantized LLMs; no MIR model is published in it. `candle` is capable but redundant when `rten` is already in the tree |
| Source separation (Demucs) | **Not used** | Rust ports exist (`demucs-rs`, `charon-audio`), but a per-track stem separation pass is a large cost for section *names* |
| Functional segmentation | **Deferred to M6** | No trustworthy Rust option. `oximedia-mir` advertises it but its breadth-to-adoption ratio does not survive scrutiny. Derive boundaries from novelty + RMS instead |
| Python `allin1` sidecar | **Optional, off the critical path** | Was the primary analysis path in spec §8.1. Now an M6 enrichment supplying segment labels only |
| Yandex integration | **`yandex-music`** (vyfor) as an ID resolver | Mature: 0.7.0, ~15K downloads, maintained since June 2024. REST-only, so it cannot serve as a `Source` — see §5.8 |
| Yandex realtime (`yamuse`) | **Open — decide at M6** | Has the ynison WebSocket that vyfor's lacks. If ynison is needed, the crate changes; the two are not interchangeable behind a flag. See §5.8 and §6.6 |

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

### 0.4 Toolchain ABI — **OPEN, decide before M4**

Surfaced by 0.1: the local toolchain is `stable-x86_64-pc-windows-gnu`, and no
Visual Studio C++ toolset is installed (`vswhere` finds no product with
`VC.Tools.x86.x64`).

This did not matter for 0.1 — the analyzer path is pure Rust and built clean. It
may matter later, because MSVC is the primary target for Windows-native work:

- **M0–M3** are unaffected. `winit`, `image`, `beat-this` all build and run on GNU,
  and Phase 0.2 exercised `UpdateLayeredWindow` through the `windows` crate on it
  successfully — so even the Win32 presentation path is proven on this ABI.
- **M4+** is the risk. The `windows` crate supports both ABIs, but WinRT (SMTC) is
  better travelled on MSVC. Smaller now that WASAPI is gone (§4.1), but SMTC is the
  one remaining WinRT surface and it is M4's whole content.
- **The `ort` fallback** from 0.1 would be awkward on GNU: prebuilt
  `libonnxruntime` binaries are MSVC-built. Only relevant if we ever leave `rten`.

Switching is `rustup toolchain install stable-msvc` plus a VS Build Tools install
(several GB). Cheap to do now, annoying to discover at M4 after building on the
wrong ABI. Recommend switching before M4 rather than at it.

---

## 4. Milestones

| # | Deliverable | Exit criterion |
|---|---|---|
| **M0** | Window + sprite playback | FAOSDance parity: loads an existing sheet + `.txt`, fixed-fps loop, transparent, click-through, draggable |
| **M1** | Local file source + BeatClock | Hand-written score JSON drives a beat-locked dance against a local WAV; no visible drift over 3 min |
| **M2** | Real analyzer + score cache | `beat-this` produces a score from a local file, cached to disk, indistinguishable in use from the hand-written one |
| **M3** | **Anticipation scheduler** | `impact_cell` respected; A/B against M1 shows a difference visible to someone not told what changed |
| **M4** | SMTC source | Identity, position and pause/resume from Spotify desktop; correct freeze and resume-on-downbeat |
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

**Cost:** streamed unknown tracks get no grid — absent, not degraded. Local files
are unaffected and remain the primary path. Spotify and Yandex know identity and
position but have nothing to dance to, until the shared score cache (spec §17.4)
exists.

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
- Cache to `%LOCALAPPDATA%\dancer-rs\scores\{source}\{track_id}.json`, namespaced
  per source (spec §5.1).
- `segments` may be empty — everything downstream must tolerate that.
- Run analysis off-thread; `ScoreReady` arrives as an `AppEvent`.

**Exit:** a real analysed score drives the dance as convincingly as M1's fixture.

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

---

### M4 — SMTC source

**Crate:** `dancer-source` (smtc adapter)

- `GlobalSystemMediaTransportControlsSessionManager` via the `windows` crate.
- Subscribe to `MediaPropertiesChanged`, `PlaybackInfoChanged`,
  `TimelinePropertiesChanged` rather than polling.
- **Anchor on `LastUpdatedTime`, never `Instant::now()` at read time.** `Position`
  refreshes only on state change.
- Filter by `SourceAppUserModelId` against the configured allowlist.
- Track identity is `(title, artist)`: normalise and hash for the cache key.
- Handle sessions publishing no timeline at all.
- State machine per spec §10, including resume-on-next-downbeat.

**Exit:** Spotify desktop drives identity, position and pause/resume correctly,
with no mid-move cuts on pause.

---

### M5 — Tray, configuration, packaging

**Crate:** `dancer-app`

- `config.toml` per spec §13, hot-reloaded via `notify`.
- Artwork hot reload.
- `tray-icon`: source selection, click-through toggle, offset nudge (spec §9.2 —
  this is now the only latency correction, so make it easy to reach).
- Ship the `beat-this` ONNX weights: ~270 KB mel + ~10 MB small model. Not bundled
  in the crate; pin a checksum.
- **Investigate the shared score cache** (spec §17.4). Not a build task yet: settle
  the licensing read and the hosting/curation answer. It is the only route to
  streaming support, since §4.1 removed the local one — but it only earns
  infrastructure if the local-file product proves worth extending.
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

**The crate choice is contingent, and deferred to M6.** `yandex-music` is correct
*for the resolver role*. It cannot serve the other role. If Yandex ever needs to be
a real push `Source` — continuous position over ynison rather than SMTC's
event-driven timeline — then `yamuse` is the crate, and it is a swap, not a flag:
different author, different API shape, different transport.

Which means the decision belongs at implementation time, when we know whether
SMTC's anchors are actually good enough for Yandex Music desktop. Until M4 has run
against it, that is speculation. Two consequences for now:

- Keep the resolver behind a narrow internal interface, so replacing or
  supplementing it doesn't reach into `dancer-clock` or `dancer-choreo`.
- Do not design the `Source` trait around REST polling assumptions. Spec §6.1
  already takes `observed_at` per observation, which a push transport satisfies
  naturally — keep it that way.

By M6, `yamuse` will have either matured past its current 75 downloads or gone
stale. Either outcome makes the decision easier than it is today.

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
sanctioned answer to the same problem is the shared score cache (§6.4) — grids, not
audio.

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
4. **Promoted — now the only route to streaming support. Investigate at M5.**
   Should scores be shareable: a fetchable cache of analysed track IDs, so a grid
   computed once serves everyone? A score is timing metadata — ~20 KB of numbers,
   **no audio** — which is what makes this plausible where recording was not.
   Gated on two answers: whether distributing derived timing data is clear, and who
   hosts and curates it. Scheduled after the local-file product works, so we learn
   whether streaming is wanted before running infrastructure for it.
5. **New:** Rust edition and MSRV. See §0.3.
6. **New:** Does Yandex need ynison push-position, or does SMTC suffice? **Decide at
   M6, not before.** The answer picks the crate — `yandex-music` for the resolver
   role as planned, `yamuse` if a real push `Source` is required — and the two are
   a swap rather than a feature flag. Not answerable until M4 has run against
   Yandex Music desktop and shown whether SMTC's anchors hold up. See §5.8.
