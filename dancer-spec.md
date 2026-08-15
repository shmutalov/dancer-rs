# Predictive Desktop Dancer — Implementation Spec

**Working title:** `dancer-rs` (placeholder)
**Target language:** Rust (2021 edition, MSRV 1.75+)
**Primary platform:** Windows 10 2004+ (build 19041) and Windows 11, both
first-class. Windows 10 is still a large share of the install base, so no feature
may *require* a Windows 11-only API. Applying that rule is what removed the audio
subsystem — see §7.
**Status:** Draft v2 — stack decided, design frozen enough to start Phase 0
**Plan:** see [ROADMAP.md](ROADMAP.md)

---

## 1. Summary

A desktop sprite dancer, in the spirit of FL Studio's Fruity Dance plugin and the
FAOSDance desktop reimplementation, rewritten from scratch in Rust with one
significant addition: **the dancer anticipates the music instead of reacting to it.**

The original Fruity Dance is driven by hand-sequenced piano roll notes. FAOSDance
drops that entirely and loops at a fixed frame rate with no audio awareness. This
project keeps the sprite format of both, but drives the animation from a
**pre-computed choreography score** — a beat grid plus section labels derived from
offline music analysis — synchronised at runtime against an uncontrolled external
player.

The design goal that everything else serves: a dance move's *impact frame* must
land on the beat, which means the move has to *start before it*. That is only
possible if the timeline is known in advance.

### 1.1 Goals

- Transparent, always-on-top, click-through sprite window on the Windows desktop.
- Backward compatibility with existing FAOSDance / Fruity Dance sprite sheets.
- Track identity, playback position and play/pause state from external players
  (Spotify, Yandex Music, browsers) without controlling playback.
- Predictive scheduling: moves are queued ahead of time against a known beat grid.
- Graceful degradation to a tempo-agnostic idle for tracks with no score, rather
  than a wrong guess.

### 1.2 Non-goals (v1)

- Cross-platform support. Design for portability behind traits; ship Windows only.
- Video export or recording of the dancer.
- FL Studio plugin/VST integration.
- Sprite sheet authoring tools.
- Any form of playback control (play/pause/skip commands). Read-only observation.

### 1.3 Attribution and licensing

FAOSDance is MIT licensed. This is a clean rewrite, not a port, but the sprite
sheet format is inherited and should be credited. Fruity Dance's bundled artwork
(FL-Chan) is Image-Line's and must not be redistributed — ship a neutral default
sheet or none at all.

---

## 2. Terminology

| Term | Meaning |
|---|---|
| **Sheet** | A PNG sprite sheet, 8 cells wide, N rows tall. |
| **Row / Action** | One horizontal row of the sheet = one animation loop. |
| **Cell** | One frame within a row. Indices 0..7. |
| **Impact cell** | The cell within a row where the visual accent lands. Must align to the beat. |
| **Held row** | By FAOSDance convention, the last row — played while the user drags the sprite. |
| **Score** | The pre-computed analysis of one track: beat grid, downbeats, sections, cues. |
| **Anchor** | A (media_position, local_instant) pair from which the local clock extrapolates. |
| **Source** | An external player adapter (Spotify, Yandex, SMTC, local file). |
| **Lock** | The state in which a score is loaded and the clock is confidently aligned. |

---

## 3. Architecture

### 3.1 Crate layout

Cargo workspace. Each crate is independently testable; the render crate must never
depend on a specific source implementation.

```
dancer-rs/
├── crates/
│   ├── dancer-sprite/      Sheet loading, manifest parsing, frame indexing
│   ├── dancer-render/      winit window, compositing, Win32 window styles
│   ├── dancer-score/       Score types, SQLite cache store, library index
│   ├── dancer-clock/       BeatClock, drift correction, phase estimation
│   ├── dancer-choreo/      Move selection, anticipation scheduling
│   ├── dancer-source/      `Source` trait + adapters (smtc, spotify, yandex, file)
│   ├── dancer-analyze/     beat-this grids, optional sidecar client
│   └── dancer-app/         Binary: wiring, tray icon, config, state machine
└── sidecar/                Optional (M6). Python: allin1 wrapper for segment labels
```

### 3.2 Threading model

The render thread owns all authoritative state. Everything else sends messages in.
No shared mutable state, no locks in the render path.

| Thread | Purpose | Cadence |
|---|---|---|
| **Render** (main) | winit event loop, clock evaluation, blitting | 60 Hz, vsync-ish |
| **Source poll** | tokio runtime; HTTP polls to Spotify/Yandex | 2–5 s |
| **SMTC listener** | WinRT event subscriptions (session/media/timeline changed) | event-driven |
| **Analysis** | `beat-this` inference; optional sidecar subprocess | on demand |

Communication: `crossbeam-channel` for thread→render messages. One `enum AppEvent`
consumed by the render loop each frame. The render thread never blocks.

```rust
enum AppEvent {
    TrackChanged { id: TrackId, meta: TrackMeta },
    PositionReport { pos_ms: u64, playing: bool, at: Instant },
    PlaybackStopped,
    ScoreReady { id: TrackId, score: Arc<Score> },
    SheetReloaded(Arc<Sheet>),
    ConfigChanged(Config),
}
```

---

## 4. Sprite format

### 4.1 Inherited format (must keep working)

- PNG, exactly **8 cells wide**, any number of rows.
- All cells equal dimensions (FL-Chan is 110×128; do not hardcode).
- Sidecar `.txt`, same basename, one row name per line, last line `Held`.
- Both files live in the configured artwork directory.

Loader must accept a sheet with no `.txt` at all: synthesise names `row_0..row_n`,
treat the last row as `Held`.

### 4.2 Extended manifest (new, optional)

A `.toml` sidecar with the same basename supersedes the `.txt` when present. This
is where the choreography metadata lives. Without it, everything still works —
the scheduler just falls back to `impact_cell = 0` and a single undifferentiated
move pool, which looks like FAOSDance with a tempo lock.

```toml
[sheet]
cell_width = 110
cell_height = 128
default_row = "idle"

[[row]]
name = "idle"
index = 0
impact_cell = 0
beats_per_loop = 2
pools = ["idle", "intro", "outro"]
loopable = true

[[row]]
name = "bounce"
index = 1
impact_cell = 3        # the accent lands here — schedule the START before the beat
beats_per_loop = 1
pools = ["verse", "chorus"]
energy = 0.5
loopable = true

[[row]]
name = "spin"
index = 2
impact_cell = 4
beats_per_loop = 4
pools = ["chorus"]
energy = 0.9
loopable = false       # one-shot; returns to default_row after

[[row]]
name = "windup"
index = 3
impact_cell = 7        # accent at the very end — this is an anacrusis move
beats_per_loop = 1
pools = ["build"]

[[row]]
name = "Held"
index = 4
impact_cell = 0
pools = []
```

**`impact_cell` is the single most important field in this document.** Everything
about the anticipation behaviour derives from it.

---

## 5. Score format

Stored in a single-file SQLite database (§5.1), keyed by canonical track ID. Shown
here as JSON for readability.

```json
{
  "schema": 1,
  "track_id": "spotify:4uLU6hMCjMI75M1A2tKUQC",
  "duration_ms": 214000,
  "bpm": 128.02,
  "meter": 4,
  "source": "beat-this",
  "confidence": 0.91,
  "analyzed_at": "2026-08-12T10:04:00Z",
  "beats":         [0.331, 0.799, 1.268, "..."],
  "beat_positions":[1, 2, 3, 4, 1, 2, "..."],
  "downbeats":     [0.331, 2.206, 4.081, "..."],
  "segments": [
    { "start": 0.0,   "end": 13.13,  "label": "intro",  "energy": 0.21 },
    { "start": 13.13, "end": 37.53,  "label": "chorus", "energy": 0.88 }
  ],
  "cues": [
    { "time": 12.60, "kind": "build",  "bars": 1 },
    { "time": 13.13, "kind": "drop" }
  ]
}
```

Field notes:

- `source` is one of `"beat-this"` (§8.1, the default path), `"allin1"` (§8.2, with
  segment labels), or `"builtin"`.
- `meter` is the modal bar length in beats. **Do not assume 4** — Phase 0.1 found
  correct 3/4 detection on a waltz, and `beats_per_loop` (§4.2) is relative to it.
- `downbeats` are detection *candidates*, not ground truth. Phase 0.1 found a track
  with a rock-steady beat grid whose downbeats split 29 two-beat bars against 30
  four-beat ones. Fit a regular bar phase to them and reject outliers before the
  scheduler uses them — see §11.3.
- `beats` / `beat_positions` / `downbeats` / `segments` keep `allin1`'s shape. Do
  not invent a different one; `beat-this` output maps onto it directly (beat
  numbers → `beat_positions`, number 1 → `downbeats`).
- **`segments` may be empty**, and is whenever the score came from §8.1 alone.
  Everything downstream must tolerate that — see §11.3.
- `energy` is normalised RMS, computed over the segment where segments exist and
  over the beat grid otherwise. The beat-grid form is a separate field,
  `beat_energy`, parallel to `beats` and empty when unmeasured — segments are
  absent from most scores, so energy could not live only on them. Normalised
  against the 95th percentile, not the maximum: one clipped transient would
  otherwise flatten every other beat into the floor.
- `cues` are derived, not from allin1: `build` is emitted for the last bar before a
  segment boundary where energy rises by more than a threshold; `drop` is the
  boundary itself. These drive anticipation moves. Without segments, derive
  boundaries from novelty on the beat grid.
- `confidence` gates entry into the `Locked` state. Below 0.6, stay `Unscored`.
  **It is a heuristic over proxies, not a measurement** (M2): nothing at runtime
  knows whether the beats are in the right places. It scores coverage — a grid over
  half a track is wrong for the other half — and self-consistency, which together
  catch a partial or incoherent detection presented as solid. Inter-beat deviation
  is weighted lightly on purpose: Phase 0.1's waltz measured 12.5 % with a good
  grid, and since frame timing comes from local intervals (§11.1), expressive
  timing costs nothing.
- **`bpm` is a label, not a clock.** M2 measured `calculate_bpm` reporting 125.0 on
  a track whose true tempo was 124.0, while the beat *count* was exactly right.
  Deriving frame timing from this field would drift a beat every two minutes on a
  grid that was never wrong. Use it for display and for sanity checks; use `beats`
  for everything that has to land on time.

### 5.1 Cache

**One file, `scores.db`, SQLite via `rusqlite` (`bundled`).** Not a JSON file per
track: a user with a few hundred analysed tracks should not get a few hundred files
scattered under a directory they will never find. Everything the app owns lives in
one folder beside the executable (§13).

Two tables:

| Table | Key | Value |
|---|---|---|
| `scores` | `{source}:{track_id}` | Score, serialised |
| `library` | `hash(title, artist)` | file path + duration + score key |

Track IDs are namespaced per source. The same song from Spotify and from a local
file gets two entries — masters differ, and a beat grid off by 40 ms looks broken.

`library` exists because SMTC reports `(title, artist)`, never a path (§6.2). It is
what lets an analysed local file be recognised when the user plays it through their
own player. See §8.3.

**The key is a hash of the raw strings — do not canonicalise the content.** Only
encoding-level normalisation is permitted before hashing: trim, Unicode NFC,
casefold. Those cannot merge two different recordings.

Content-level normalisation is forbidden: stripping `(Radio Edit)`,
`(Official Music Video)` or `- Remastered` merges entries that are *different
masters with different grids*. That would contradict the rule above — the same song
from two sources gets two entries on purpose.

The failure modes are not symmetric:

| Approach | Fails as | Cost |
|---|---|---|
| Canonicalise content | False **positive** | Wrong grid applied; dancer confidently off-beat, user cannot tell why |
| Hash raw strings | False **negative** | Cache miss; dancer sits `Unscored`, one re-analysis |

A miss is cheap and self-correcting. A mismatch looks like a bug. Prefer the miss —
same reasoning as §8.3's "a confidently wrong grid is worse than none".

The common case needs no cleverness anyway: when a local file is played through an
ordinary player, SMTC reports *that file's own tags*, so the strings match exactly
because they came from the same place. Variation appears across sources, and those
are different masters that should not be merged.

**Verify duration on match.** Store the track duration alongside each entry and
compare against the source's reported `EndTime`, tolerance ±2 s. A hash hit with a
disagreeing duration is treated as a miss. This costs nothing and catches the case
where a player reports an album title while playing a radio edit.

Because it is a single file, copying it elsewhere works. That is a property, not a
feature: we neither build nor support sharing (§17.4).

### 5.2 Why SQLite and not an embedded KV store

`redb` was chosen first, on the grounds that the workload is pure key-value and
that it keeps a C compiler out of a build that otherwise has none — the property
§8.1 protects. That reasoning did not survive examination.

| | `redb` | SQLite |
|---|---|---|
| Build cost | none | **53 s, one-time** (measured, GNU toolchain, 33 packages) |
| Runtime dependency | none | none — still one executable |
| On-disk format | broke at 2.0 and again at 3.0 | stable since 2004, committed to 2050 |
| Inspectable | only by Rust at a matching major version | any SQLite tool, ad-hoc `SELECT` included |

Three things decided it:

1. **Format churn against expensive data.** At the 41–74x realtime measured in
   Phase 0.1, a 2,000-track library is roughly two hours of analysis. redb's format
   broke twice in two years, and the 2→3 migration was only reachable *through* an
   intermediate 2.6 release — so a user who skips a version can hold a cache the
   binary can neither open nor migrate. Pinning and writing in-app migrations is
   possible, but it is permanent work that SQLite simply never asks for. Schema
   versioning is still ours to do; container migration is not.
2. **Opacity.** A support request becomes "send me `scores.db`" only if the file
   opens in something. And if the app will not start, an opaque cache cannot be
   inspected at all.
3. **Ecosystem and bus factor.** No tooling, no migration story, essentially one
   maintainer.

The `rten` precedent does not transfer. Avoiding a C dependency was right there
because the alternative was PyTorch and NATTEN — a burden on *every user*, and an
install many could not complete. `libsqlite3-sys` is a one-time build step on the
developer's machine. Same shape of argument, different order of magnitude.

**Schema versioning is still required.** Use `PRAGMA user_version`, check on open,
migrate forward in code. The score shape will change; that work exists either way.

---

## 6. Sources

### 6.1 The `Source` trait

```rust
#[async_trait]
pub trait Source: Send + Sync {
    fn name(&self) -> &'static str;
    /// Cheap check — is this source usable right now?
    async fn available(&self) -> bool;
    /// One observation. Called on the poll cadence.
    async fn poll(&mut self) -> Result<Option<Observation>, SourceError>;
    /// How coarse this source's position reporting is, for drift tuning.
    fn position_granularity(&self) -> Duration;
}

pub struct Observation {
    pub track: TrackMeta,      // id, title, artist, duration
    pub position: Duration,    // as reported
    pub playing: bool,
    pub observed_at: Instant,  // local monotonic, taken as close to the read as possible
}
```

`observed_at` must be the instant the reported position was **true**, not the
instant the value was read and not the instant the message is handled. For SMTC
that is `LastUpdatedTime` (§6.2). Everything downstream depends on that pairing: a
reading 87 seconds old is exact when paired with its own timestamp and 87 seconds
wrong when paired with `Instant::now()`.

**Implemented synchronous, not `async` (M1).** Nothing that exists needs async, and
it is not free — `async_trait` boxes every call and an async trait implies a tokio
runtime in the workspace. The adapters divide cleanly: SMTC (M4) is WinRT, whose
async operations expose a blocking `join()` — Phase 0.5 used exactly that — and it
runs on its own thread anyway (§3.2), where blocking is the point. Spotify and
Yandex (M6) genuinely want async and will bring their own runtime. So deferring
costs one `Source` impl wrapping `block_on` when an HTTP adapter first appears, and
not deferring costs a runtime dependency carried from M1 to M6 for nothing.
Revisit at M6.

### 6.2 SMTC adapter (primary, universal)

Windows exposes system-wide media sessions through
`Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager`
(Windows 10 1809+, requires the `globalMediaControl` capability). Accessed from
Rust via the `windows` crate.

This covers Spotify desktop, Yandex Music desktop, and any browser tab that calls
the Media Session API — which YouTube does. **It should be the default source**,
because it needs no OAuth, no tokens, no network, and no per-service maintenance.

Per session it provides:
- `TryGetMediaPropertiesAsync()` → title, artist, album
- `GetPlaybackInfo()` → `PlaybackStatus` (Playing / Paused / Stopped), rate
- `GetTimelineProperties()` → `Position`, `StartTime`, `EndTime`, `LastUpdatedTime`

Subscribe to `MediaPropertiesChanged`, `PlaybackInfoChanged` and
`TimelinePropertiesChanged` rather than polling. Filter sessions by
`SourceAppUserModelId` against a configurable allowlist so a notification sound or
a background video doesn't hijack the dancer.

**That allowlist cannot be a hardcoded list of English executable names.** Measured
2026-08-15: Yandex Music desktop identifies as `Яндекс Музыка.exe` — the AUMID is a
localised, non-ASCII string that varies with the installer's language. Compare as
Unicode, and build the allowlist from sessions actually observed, offered in the
tray, rather than shipping a fixed table that silently excludes anyone not running
an English install.

**Caveats to handle:**
- **`Position` is only refreshed on state change, not continuously.** Use
  `LastUpdatedTime` as the anchor timestamp, never `Instant::now()` at read time.
  Measured 2026-08-15 in two unrelated applications: Edge reported `0.019s` for a
  track 59.7 s in, and Yandex Music desktop reported `43.172s` while 130 s in —
  stale by 87 s, growing to 120 s on a later sample with `Position` unchanged. This
  is not a rounding concern and not one app's bug; without the anchor, SMTC position
  data is unusable.
- Some sessions publish no timeline at all. Detect and drop to `Unscored`; there is
  no audio fallback (§7).
- **Some players publish nothing at all.** Measured 2026-08-15: Yandex Browser
  returns zero sessions with Yandex Music actively playing, while Edge on the same
  machine works. No OS cause — no policy, default flags. For such a player the
  dancer never learns anything is playing and sits `Idle`, not `Unscored`. This is
  what justifies the Yandex adapter (§6.4), and it needs a compatibility sweep
  before M4 exits: Chrome, Firefox, Opera, Spotify desktop, AIMP, foobar2000, VLC.
  The blind spot is per-*application*, not per-service: the Yandex Music **desktop**
  app publishes normally on the same machine (§6.4).
- **Track identity is `(title, artist)`, never a file path.** Normalise (trim,
  casefold, strip `- Remastered` style suffixes) and look the result up in the
  `library` table (§5.1), keyed on a hash of the raw strings. Encoding-level
  normalisation only — trim, NFC, casefold — never content-level suffix stripping;
  see §5.1 for why. Verify duration on match. That lookup is what connects "the user pressed play in
  foobar2000" to "we analysed that file last week" — it is the mechanism the whole
  owned-music path rests on (§8.3), so the normalisation rules deserve real tests
  and a fixture set of awkward titles.

### 6.3 Spotify adapter (optional, better IDs)

`rspotify` with OAuth PKCE. Endpoint `GET /v1/me/player/currently-playing` gives
`progress_ms`, `is_playing`, and a stable track URI.

Advantages over SMTC: canonical track ID, works when Spotify plays on another
device (phone, Connect speaker) where SMTC sees nothing.

Disadvantages: rate-limited HTTP, ~1 s granularity, network latency in the
`observed_at` pairing, requires user auth flow.

**No longer a score source.** Spotify's `audio-analysis` and `audio-features`
endpoints historically returned a full beat/bar/section breakdown, and access was
restricted for new applications. This mattered when it looked like a drop-in
replacement for the offline pipeline; with §8.1 computing grids locally, it
doesn't. Treat Spotify purely as an identity/position source and don't spend time
resolving its availability.

### 6.4 Yandex Music track ID resolver (optional)

Not a `Source`. The `yandex-music` crate (vyfor, MIT, maintained since June 2024)
is a REST client: 13 API submodules, no websocket, no ynison. Its `queue` endpoint
returns queue *contents*, not continuous playback position — so it cannot supply
`position`, reliable `playing`, or a tight `observed_at` pairing.

*Confidence: high but unverified — docs.rs coverage is 13.84%. Confirm in the
source before building against it.*

It earns its place in a different role. SMTC's weakest point is identity: §6.2
resolves tracks by normalising and hashing `(title, artist)`, a pile of heuristics
that will collide. So split the work:

| Concern | Provider |
|---|---|
| Position, playing, timeline anchors | SMTC (event-driven, `LastUpdatedTime`) |
| Stable track ID, canonical metadata | `yandex-music` (REST lookup) |

Cache-key correctness is what makes learn-on-second-listen (§8.3) work at all, so
this is worth more than it looks. Failure degrades gracefully: lose the resolver
and you fall back to hashed strings, not to nothing.

Still feature-flagged, still must not take down the binary. It wraps an
undocumented internal API and needs an OAuth token extracted from the desktop
client or web session — Rust removes the Python runtime, not the moving target.

**Decided 2026-08-15: `yamuse`.** It carries the ynison realtime WebSocket, so
Yandex can be a push `Source` — continuous position rather than SMTC's event-driven
timeline — and it exposes catalogue too, so it covers identity. It replaces
`yandex-music` rather than sitting beside it.

`yamuse` is young: 0.3.2, first published 2026-07-29, 75 downloads at the time of
choosing. Keep it feature-flagged, keep SMTC covering the same player, and do not
let a break take the binary down. `yandex-music` (vyfor, ~15K downloads, maintained
since June 2024) stays the fallback for the resolver role.

**Why, as of 2026-08-15.** The original case was ynison's precise position, and it
was weak: precision only matters in `Locked`, and streamed tracks are permanently
`Unscored` (§8.3). Phase 0.5 replaced that argument with a better one — Yandex
Browser publishes *nothing* to SMTC, so the choice is presence versus nothing, not
fine position versus coarse. Without it the dancer cannot tell a track is playing.

**Settled 2026-08-15: the desktop app publishes to SMTC normally.** Title, artist,
`PlaybackStatus`, timeline and a live `LastUpdatedTime`, identifying as
`Яндекс Музыка.exe`. The blind spot is Yandex *Browser*, not Yandex Music.

This shrinks `yamuse` to the web-player case, and weakens the presence argument that
justified choosing it — a Yandex user who installs the desktop app is fully served
by §6.2 with no dependency, no auth flow and no undocumented API. Keep `yamuse`
feature-flagged and reassess at M6 whether it earns its place at all; "install the
desktop app" is a legitimate answer that costs nothing to maintain.

**Do not reach the download endpoints.** Several Yandex wrappers expose lossless
track downloads — fetch the file, analyse it, get a perfect grid.

Cutting audio capture (§7) made this *more* tempting, not less. Recording was the
legitimate route to a grid for a streamed track; with it gone, streaming is
idle-only (§8.3) and a download endpoint sitting inside a dependency we already
ship looks like the obvious way to close the gap. Losing the weaker option does not
upgrade the stronger one: recording was local capture of audio already playing on
the user's machine, disabled by default; this is retrieving masters from a CDN.

Keep `dancer-source` structurally unable to call them, rather than relying on
nobody reaching. There is no sanctioned alternative for streamed tracks: they stay
`Unscored` (§8.3). That is the decision, not a gap awaiting a workaround.

### 6.5 Local file adapter (development)

Plays nothing; reads a WAV/MP3 path plus a simulated transport. Exists so the
clock, scheduler and renderer can be tested deterministically without any
streaming service in the loop. **Build this first** — M0 through M3 depend on it.

---

## 7. Audio capture — cut from v1

**There is no audio subsystem.** No WASAPI, no loopback, no `dancer-audio` crate,
no recording. This section records why, because the reasoning is easy to lose.

Capture was never the sync mechanism — the original draft said so in its own first
line. Beat grids come from offline analysis (§8.1) and position comes from the
source adapters (§6). Capture existed only for four secondary purposes, and each
one dissolved:

| Purpose | Why it went |
|---|---|
| Silence watchdog | Guards a narrow failure class that SMTC's event subscriptions already report. On a full mix it fails toward false *non*-silence, so it is least reliable exactly where it would be needed |
| Offset calibration | Real value, but a manual nudge gets there. Visual sync error is highly perceptible; users trim to within ~30 ms by eye, once per source app. See §9.2 |
| Recording for analysis | Opt-in, disabled by default, prohibited by streaming ToS, and on a contaminated mix it yields corrupt grids — and a confidently wrong grid is worse than none |
| Reactive fallback | §17.2 already questioned whether it belonged in v1 |

The decisive one was per-process capture. It was the mitigation for every
contaminated-mix problem above, and it requires build 20348+ — Windows Server
2022's build number. Retail Windows 10 ends at 19045, so **no consumer Windows 10
build has the API at all**. With Windows 10 first-class (§1), the clean-capture
path is unavailable to a large share of users, which left an entire subsystem whose
best case was "sometimes less wrong".

**What this costs.** Without recording there is no learn-on-second-listen, so
streamed unknown tracks get no grid — not degraded, absent. The app is excellent
for local files; for Spotify and Yandex it knows identity and position but has
nothing to dance to. Settled at §17.4: there is no hosted-score answer, and the
product is owned music via the library index (§8.3).

**What it buys.** One crate, two milestones, the `wasapi` dependency, the recording
legal exposure, and the whole contaminated-mix design problem — all deleted.

Revisit post-v1 only if streaming support turns out to matter more than everything
above — and note that recording was never a good answer to it, merely the available
one.

---

## 8. Analysis

### 8.1 Primary analyzer (pure Rust)

`beat-this` (MIT) is a Rust port of the *Beat This!* tracker (ISMIR 2024), running
on the `rten` runtime — no Python, no system libraries, `ort`/ONNX available behind
a feature flag for cross-validation. It reports verified F-measure parity with the
PyTorch reference for the standard model.

It emits beats and downbeats with beat numbers (1 = downbeat, 2–4 otherwise), which
map directly onto `beats`, `downbeats` and `beat_positions`. `dancer-analyze` adds
`energy` from RMS over the beat grid, and emits `source: "beat-this"`.

This is the whole metrical half of the analysis, in-process, on any Windows machine
with no install story. What it does **not** provide is functional segmentation, so
scores from this path carry `segments: []` — see §11.3 for how the scheduler copes.

**Model weights are not bundled** in the published crate: a ~270 KB mel front end
plus either the ~10 MB small model or the ~83 MB full-accuracy one must ship with
the app. There is still no install story for the *user* — no Python, no system
libraries — but there is a packaging obligation.

*Validated in Phase 0.1 (2026-08-15): builds clean on the GNU toolchain, 1.4–2.2%
inter-beat deviation on steady material, 41–74x realtime, empty grid rather than a
fabricated one on non-musical input. Caveats: four tracks, no independent
annotations, small model. See `spikes/beat-this-probe`. Fallback if it degrades is
`ort` with an ONNX export of the upstream weights.*

### 8.2 Optional sidecar for segment labels

`allin1` (All-In-One Music Structure Analyzer, MIT) is the only credible source of
labelled functional segments — intro, verse, chorus, bridge, outro. It is
PyTorch-based, runs source separation first, and depends on NATTEN, which on
Windows must be built from source. **Do not attempt to port it to Rust.**

It is therefore *optional enrichment*, not the primary path: a subprocess sidecar
speaking newline-delimited JSON over stdio, supplying `segments` and `cues` on top
of a grid §8.1 already produced.

```
→ {"cmd":"analyze","job":7,"path":"C:\\...\\track.wav","track_id":"..."}
← {"job":7,"status":"progress","pct":40}
← {"job":7,"status":"done","score":{ ... }}
```

NATTEN remains a barrier, but now for a feature rather than for the product. A
native Rust route exists in principle — Demucs has Rust ports (`demucs-rs`,
`charon-audio`) and neighborhood attention is expressible as masked attention —
but weight porting plus per-layer numerical validation is a research project, and
a per-track stem separation pass is a steep price for section *names*.

`oximedia-mir` advertises structural segmentation in pure Rust. Its breadth
(tempo, key, chord, melody, structure, genre, mood) against its version and
adoption does not survive scrutiny; benchmark before trusting it.

### 8.3 Where scores come from

**Analysis needs a file it can read.** Everything follows from that.

1. **Files in the user's library.** Scan the configured folders (§13), analyse,
   cache the grid *and* an index entry keyed by normalised `(title, artist)`
   (§5.1). This is the primary path, it works entirely offline, and it is what
   makes (2) work.
2. **The user's own player.** SMTC reports title, artist and position for whatever
   is playing — foobar2000, AIMP, VLC, Winamp — but never a file path (§6.2). Match
   its `(title, artist)` against the `library` table and the grid is already there.
   The user plays their music however they normally do and the dancer locks on.
3. **Streamed tracks.** No readable file, and with the audio subsystem cut (§7) no
   recording either. There is no route to a grid, and by decision there will not
   be one (§17.4). Spotify and Yandex drive identity, position and pause/resume,
   and the dancer runs `Unscored` — a fixed-fps loop, honest about knowing nothing.

Path (2) is the point. Owned music is how most people listen to the music they care
about most, and it needs no account, no service and no network. Streaming being
unscored is a real limitation, not a temporary one — state it plainly in the UI
rather than letting users think it is broken.

Learn-on-second-listen — capture the loopback, analyse the WAV — was the previous
answer to (3) and is cut with the rest of §7: opt-in, disabled by default,
prohibited by streaming ToS, and corrupt on a contaminated mix.

---

## 9. The clock

This is the core of the runtime. Position must be known to roughly ±20 ms, from
sources that report at 1 s granularity, a few seconds apart.

The solution is to run a local free-running clock and use polls only to correct it.
Media playback clocks are extremely stable — the error accumulates slowly and
smoothly, so infrequent coarse observations are enough to steer a local estimate.

```rust
pub struct BeatClock {
    score: Option<Arc<Score>>,
    anchor_media: f64,     // seconds into the track at the anchor
    anchor_local: Instant, // local monotonic at the anchor
    rate: f64,             // 1.0 nominal; slewed to absorb drift
    offset: f64,           // calibrated output latency, seconds
    confidence: f32,
}

impl BeatClock {
    pub fn position(&self, now: Instant) -> f64 {
        self.anchor_media
            + (now - self.anchor_local).as_secs_f64() * self.rate
            - self.offset
    }
}
```

### 9.1 Correction policy

On each `Observation`:

```
est  = position(obs.observed_at)
err  = obs.position_secs - est

if !obs.playing                       -> freeze (see §10)
else if |err| > SEEK_THRESHOLD (1.5s) -> hard re-anchor; drop to Resync
else if |err| > SLEW_LIMIT (0.25s)    -> hard re-anchor; keep Locked
else                                  -> slew: rate = clamp(1.0 + err / SLEW_WINDOW, 0.98, 1.02)
                                          then re-anchor at the current estimate
```

`SLEW_WINDOW` ≈ 5 s. Never step the position while `Locked` — a visible jump in
the dancer's phase is far more noticeable than being 80 ms off for a few seconds.
Correct by bending the rate.

**Those two rules contradict each other, and M1 found it.** The middle band
prescribes a hard re-anchor, which *is* a step, while the paragraph above forbids
stepping while `Locked`. Both cannot hold: ±2 % closes only 100 ms per 5 s, so a
1.4 s error would take over a minute to slew out, and sitting a beat off for a
minute is worse than one visible jump.

M1 implements the three bands literally and returns a distinct `Correction` variant
for each, so a step is always visible to the caller rather than silent. The real
resolution needs M3: defer the step to the current row's loop boundary, where the
sprite is already returning to its neutral pose and a phase change costs nothing to
look at. Until the scheduler exists there is no boundary to defer to.

In practice the middle band is rare — measured across a 3-minute run against a
player drifting 0.02 % with 2 s stale readings, every correction was a slew and
none was a step.

### 9.2 Offset calibration

`offset` absorbs everything between "the player says it is at 42.0 s" and "the
sound reaches the speakers": decoder buffering, the output buffer, and for HTTP
sources the request round-trip. Typical range 100–300 ms.

This is not a rounding error. At 128 BPM a beat is 469 ms, so an uncalibrated
offset can exceed half a beat — enough to make "the impact frame lands on the beat"
simply false. It has to be corrected.

**Calibration is manual.** A nudge slider per source app, stored in config,
persisted across restarts. Automatic measurement — cross-correlating loopback
onsets against the beat grid — was the last surviving justification for the audio
subsystem and did not survive the cost (§7).

Manual is a smaller loss than it looks. Sync error is highly perceptible visually,
which is the same faculty that would judge whether auto-calibration had worked, so
a user trims to within a few tens of milliseconds by eye in seconds. It is a
one-time action per source app, not a per-track one.

Ship sensible defaults so the first run is close: roughly 180 ms for local
playback, 250 ms for browsers.

---

## 10. State machine

| State | Meaning | Animation behaviour |
|---|---|---|
| `Idle` | Nothing playing | Default row, slow fixed fps, or hidden |
| `Identifying` | Track known, score lookup in flight | Idle row, tempo-agnostic |
| `Unscored` | Playing, no usable score | Default row at a fixed fps. No tempo guess — FAOSDance behaviour, honestly labelled |
| `Locked` | Score loaded, clock confident | Full predictive scheduling |
| `Resync` | Seek/skip/drift detected | Continue current row to its loop point, re-anchor |

Transitions:

- `TrackChanged` → `Identifying` from any state. Cancel queued moves.
- Score found and `confidence >= 0.6` → `Locked`. Else → `Unscored`.
- `playing == false` → freeze the clock (do not advance `anchor_local`), finish the
  current row to its loop boundary, settle into the default row. Do **not** cut
  mid-move; a hard stop reads as a crash.
- Resume → re-anchor, then wait for the next downbeat before resuming full moves.
  Starting mid-bar looks worse than a half-second of idle.
- `ScoreReady` → `Locked` if the ID still matches what's playing.
- Drift beyond `SEEK_THRESHOLD` → `Resync` → `Locked` once two consecutive polls agree.

---

## 11. Choreography scheduling

### 11.1 Frame timing

For a row with `beats_per_loop = B` over 8 cells, at beat interval `T`:

```
frame_duration = (B * T) / 8
```

Recompute per move from the local beat interval (successive entries in `beats`),
not from the global BPM — tracks drift, and live recordings drift a lot.

### 11.2 Anticipation — the important part

A move must be scheduled so its **impact cell** lands on the target beat:

```
start_time = target_beat_time
           - (impact_cell * frame_duration)
           - render_latency
```

`render_latency` is one frame of the display loop (~16 ms at 60 Hz), plus the
compositor's delay. Measure it; don't assume.

**Half of it cannot be measured, and that is fine** (M3). Present cost — from
deciding to draw to `UpdateLayeredWindow` returning — is timed per frame and kept as
a rolling median, plus half the tick interval since a cell change becomes visible at
the next tick. The compositor's delay from that call returning to photons leaving
the panel is **not observable from inside the process**: DWM composites on its own
schedule and nothing the app can call reports scan-out. That residue is a constant,
and §9.2's offset slider exists precisely to absorb constants — the user trims by
eye until the dancer looks right, and DWM's share is inside that number whether it
is modelled or not. Getting this term slightly wrong is survivable; getting
`impact_cell × frame_duration` wrong is not, and that one is exact.

The scheduler runs a lookahead window of ~2 s and maintains a small queue:

```rust
pub struct ScheduledMove {
    pub row: usize,
    pub start_at: f64,     // media-time seconds
    pub frame_duration: f64,
    pub target_beat: f64,
}
```

Each frame, the render thread evaluates `clock.position(now)`, pops any move whose
`start_at` has passed, and computes the current cell as
`floor((pos - start_at) / frame_duration)`.

### 11.3 Move selection

Given the segment label at the target beat and its energy:

1. Filter rows whose `pools` contain the segment label.
2. Filter by energy proximity: `|row.energy - segment.energy| < 0.35`.
3. Exclude the row used in the previous bar (no immediate repeats).
4. Weighted random from what remains; fall back to `default_row` if empty.

**Step 2 needs a widening rule** (M3). A hard threshold assumes the track has
dynamics. Measured on an analysed track sitting at 0.89 energy throughout, exactly
one row of the default sheet fell inside the window, so the dancer repeated one move
for the whole track — the FAOSDance behaviour this project exists to beat. Loudness-
war masters do this to real music too. So when the window leaves fewer than two
candidates, widen to the nearest few by energy distance, **capped at twice the
window**: reaching further would put a full-energy spin in a quiet intro, which is a
worse failure than repeating a move. Rank ties break on row index, so a
choreography stays reproducible from its seed.

**Unlabelled scores.** Scores from §8.1 have no segments, so step 1 has nothing to
match on. Selection then keys on **energy tier and boundary position** instead,
with labels treated as enrichment when present: bucket the local RMS into tiers,
pick from rows whose `energy` sits in the current tier, and treat a novelty peak on
a downbeat as a boundary for cue purposes.

This is the mechanism that lets the project ship without segmentation. The
scheduler needs to know that energy rose at a downbeat — not that the section is
called a chorus. Labels sharpen pool selection; they do not affect timing, and
timing is the whole thesis.

Overrides, in priority order:

- A `build` cue in the score → force a row from the `build` pool for that bar.
  These are the `impact_cell = 7` anacrusis moves; they exist to be visibly
  winding up while the music winds up.
- A `drop` cue or a downbeat starting a new segment → force a one-shot
  (`loopable = false`) high-energy row, aligned so its impact lands on that downbeat.
- Non-loopable rows return to `default_row` on completion.

Change moves on downbeats only, never mid-bar, unless a cue forces it — against the
**fitted bar phase**, not raw `downbeats` entries. Beat detection is markedly more
reliable than downbeat detection (§5), and this is the one place the scheduler
depends on the weaker signal: a spurious downbeat means a move change half a bar
early, which reads as the dancer stumbling.

---

## 12. Rendering

- `winit` 0.30+ for the window: `with_transparent(true)`, `with_decorations(false)`,
  `WindowLevel::AlwaysOnTop`.
- Click-through is `WS_EX_TRANSPARENT`: clicks pass through to whatever is behind.
  Toggled off while dragging. This *improves on* the FAOSDance "solid" toggle
  rather than reimplementing it — upstream has no `WS_EX_TRANSPARENT`, no window
  shape and no JNA, so `solid` merely gates its own mouse handlers and the window
  still swallows every click.
- **`UpdateLayeredWindow` via the `windows` crate for presentation.** Not
  `softbuffer`: its pixel format is documented as
  `00000000RRRRRRRRGGGGGGGGBBBBBBBB` — top 8 bits zero, no alpha channel — and its
  Win32 backend blits with `SRCCOPY`. It cannot express per-pixel alpha at all.
  Measured in Phase 0.2; see `spikes/alpha-probe`.
- A sprite dancer still does not need `wgpu`. The layered path costs 0.066–0.112 ms
  per frame at 128–512 px, under 1% of a 60 Hz budget.
- Presentation replaces the whole window surface, so there is no `WM_PAINT` and
  winit's redraw path goes unused. The loop is: build a premultiplied BGRA DIB,
  call `UpdateLayeredWindow`.
- **`UpdateLayeredWindow` carries pixels only — never geometry.** It can reposition
  the window through its `pptDst` argument, which is tempting because it makes a
  drag one call instead of two. Doing so moves the window without winit knowing,
  and winit owns and caches window geometry: after enough fast drags its view
  diverges from reality and mouse input stops being delivered to the window at all.
  Move with `Window::set_outer_position` and pass `NULL` for `pptDst`. Found in M0;
  the symptom is bizarre (window visible, enabled, correctly styled, hit-testable,
  event loop alive, yet no mouse events — not even synthetic ones) and gives no
  hint of its cause.
- `image` for PNG decode; pre-slice cells into an `Arc<[RgbaImage]>` at load,
  **premultiplied** — the present path requires it, so per-frame conversion is
  pure waste.
- Additional extended styles: `WS_EX_TOOLWINDOW` (keep out of Alt-Tab and taskbar),
  `WS_EX_NOACTIVATE` (never steal focus).
- Dragging: while `set_cursor_hittest(true)`, mouse-down switches to the `Held` row
  and moves the window with the cursor. This is the one interaction that must feel
  immediate — bypass the scheduler entirely.
- Multi-monitor and per-monitor DPI: persist window position as
  (monitor id, normalised x/y), not absolute pixels.

---

## 13. Configuration

**Portable layout.** Everything the app owns sits in one folder beside the
executable:

```
dancer-rs/
├── dancer-rs.exe
├── config.toml        hand-editable; hot-reloaded via `notify`
├── scores.db          single-file SQLite cache — grids + library index (§5.1)
├── models/            beat-this ONNX weights (§8.1) — must ship anyway
└── artwork/           sprite sheets
```

Fall back to `%LOCALAPPDATA%\dancer-rs\` only when the executable's directory is
not writable — installed under Program Files, or run from read-only media. Detect
by attempting a write, not by inspecting the path.

The earlier draft split config across `%APPDATA%` and the cache across
`%LOCALAPPDATA%`, one JSON per track. A user could not answer "where does this
thing keep its stuff" without being told. Now they open one folder and see it.

Config stays TOML because it is meant to be hand-edited; the cache is a database
because it is not.

```toml
[sprite]
sheet = "dance.png"
artwork_dir = "C:\\Users\\me\\Documents\\dancer\\artwork"
scale = 1.0
mirror = false
opacity = 1.0

[window]
always_on_top = true
click_through = true
monitor = 0
x = 0.82
y = 0.65

[sources]
order = ["spotify", "smtc"]
allowlist = ["Spotify.exe", "YandexMusic.exe", "chrome.exe"]
poll_interval_ms = 3000

# Manual output-latency trim per source app, milliseconds. See §9.2.
[offset_ms]
"Spotify.exe" = 180
"chrome.exe"  = 250

# Folders scanned and analysed so SMTC-reported tracks can be matched to a
# file we already have a grid for. See §8.3.
[library]
folders = ["C:\\Users\\me\\Music"]
watch = true                       # re-scan on change via `notify`
analyze_on_scan = false            # true = analyse everything up front

[analysis]
model_dir = "models"               # beat-this ONNX weights; see §8.1
cache = "scores.db"                # single-file SQLite store; see §5.1
sidecar_path = "sidecar/dancer-analyze.exe"    # optional, M6
min_confidence = 0.6

[choreo]
lookahead_ms = 2000
change_on_downbeat_only = true
```

---

## 14. Milestones

| # | Deliverable | Exit criterion |
|---|---|---|
**See [ROADMAP.md](ROADMAP.md)** for the working plan: per-milestone task
breakdowns, Phase 0 de-risking spikes, and the stack decisions behind them. Summary
table below.

| # | Deliverable | Exit criterion |
|---|---|---|
| **M0** | Window + sprite playback | FAOSDance parity: loads an existing sheet + `.txt`, loops at fixed fps, transparent, click-through, draggable |
| **M1** | Local file source + BeatClock | Hand-written score JSON drives a beat-locked dance against a local WAV; visually in time for 3 min with no drift |
| **M2** | Real analyzer + score cache | `beat-this` produces a score from a local file, cached to disk, indistinguishable in use from the hand-written one |
| **M3** | Anticipation scheduler | `impact_cell` respected; A/B against M1 shows the difference is visible |
| **M4** | SMTC source | Identity, position and pause/resume from Spotify desktop; correct freeze and resume-on-downbeat behaviour |
| **M5** | Tray UI, config, packaging | Installable by a stranger |
| **M6** | *Optional:* segment labels, Yandex resolver, Spotify | Only if M5 shows unlabelled pools are the visible gap |

The old M5 (WASAPI loopback) and M6 (learn-on-second-listen) were cut with the
audio subsystem — see §7.

Analysis moved ahead of the scheduler: it stopped being a Python deployment problem
(§8.1) and became a crate call, and testing anticipation against *real* grids —
with their jitter and drift — is a much sharper test than testing it against a
fixture authored to be correct.

M0–M3 need no audio subsystem and no network. Most of the interesting work is
there. **Do not start M4 until M3 looks right against a local file.** M3 is the
gate: everything before it is a sprite player with a metronome, everything after is
plumbing to feed it.

---

## 15. Dependencies

| Crate | Purpose |
|---|---|
| `winit` | Windowing, event loop, cursor hit-test |
| `image` | PNG decode |
| `windows` | SMTC, `UpdateLayeredWindow` presentation and window styles |
| `beat-this` | Beat + downbeat tracking (§8.1) |
| `rten` | ML runtime backing `beat-this`; `ort` behind a feature flag for cross-check |
| `symphonia` / `rubato` | Audio decode and resampling (arrive via `beat-this`) |
| `tokio` + `reqwest` | Async source polling |
| `rspotify` | Spotify Web API + OAuth PKCE (M6) |
| `yamuse` | Yandex ynison push source (M6, §6.4); `yandex-music` is the fallback |
| `serde` / `serde_json` / `toml` | Score, manifest, config |
| `rusqlite` (`bundled`) | Single-file score cache and library index (§5.1) |
| `crossbeam-channel` | Thread messaging |
| `notify` | Config and artwork hot reload |
| `tray-icon` | System tray |
| `directories` | Platform config paths |
| `tracing` / `tracing-subscriber` | Logging |
| `thiserror` / `anyhow` | Errors |

---

## 16. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| ~~`beat-this` quality below its claims~~ | — | **Retired.** Phase 0.1 validated beat grids at 1.4–2.2% deviation, 41–74x realtime |
| Downbeat detection unreliable | Moves change half a bar early; reads as stumbling | Fit bar phase to candidates and reject outliers (§11.3). Confirmed real in Phase 0.1 |
| Meter assumed 4/4 | Waltzes and odd meters schedule wrong | `meter` field in the score (§5); `beats_per_loop` relative to it |
| Model weights not bundled in the crate | ~10–83 MB to ship and version | Vendor with the installer; pin a checksum |
| GNU toolchain vs MSVC | WinRT (SMTC) less travelled on GNU; `ort` fallback awkward | Unaffected through M3. Switch to `stable-msvc` before M4 |
| No functional segmentation available | Pools can't key on section labels | Energy-tier selection (§11.3); labels are enrichment, not timing. Optional sidecar in M6 |
| NATTEN build on Windows | Optional segment labels unavailable | No longer blocks the product (§8.2). Ship without labels |
| ~~winit gives colour-keying, not per-pixel alpha~~ | — | **Retired.** Phase 0.2 measured the `UpdateLayeredWindow` path exact to ±1 across an alpha ramp. `softbuffer` dropped: it has no alpha channel |
| Yandex internal API changes | ID resolution breaks | Feature flag; SMTC still supplies position and identity, degraded to hashed strings |
| ~~Recording legality / ToS~~ | — | **Retired.** Audio capture cut from v1 (§7); nothing records anything |
| Streamed tracks have no grid path | Spotify/Yandex are permanently `Unscored` | Settled, not deferred: no hosted cache (§17.4). Owned music via the library index is the product (§8.3). Say so in the UI so it does not read as a bug |
| Track download endpoints used for analysis | Retrieving masters from a CDN. **The most tempting shortcut in the project** — with recording cut and no hosted cache, it is the only remaining way to give streaming a grid | Keep `dancer-source` structurally unable to reach them (§6.4). There is no sanctioned alternative: streaming stays `Unscored` |
| SMTC session ambiguity | Wrong app drives the dancer | Allowlist + explicit source selection in tray |
| ~~Per-process loopback needs build 20348+~~ | — | **Retired.** This constraint is what removed the audio subsystem rather than something to mitigate (§7) |
| Sheets lack `impact_cell` | No anticipation, back to FAOSDance behaviour | Ship an annotated default sheet; add a small cell-picker tool in M5 |

---

## 17. Open questions

1. ~~Is Spotify's `audio-analysis` endpoint available to new apps?~~ **Resolved:
   moot.** §8.1 computes our own grids, so the answer no longer changes anything.
2. ~~Should `Reactive` mode exist in v1?~~ **Resolved: no.** It needed live DSP,
   which went with the audio subsystem (§7). Replaced by `Unscored` — the default
   row at a fixed fps, which is honest about knowing nothing rather than guessing.
3. Sheet compatibility: is 8 cells worth keeping as a hard constraint, or should
   the manifest allow arbitrary widths with 8 as the default? **Keep it**, with the
   manifest free to override later. Costs nothing and buys the whole existing
   sheet library.
4. ~~Should scores be shareable via a hosted cache?~~ **Resolved: no.** Everything
   is analysed and cached locally (§5.1). We will not run a server, host grids, or
   build fetch/upload paths.

   `scores.db` is a single file, so a user who copies it to a friend will find that
   it works. That is a consequence of the storage choice, not a supported feature —
   nothing in the app assists it and nothing depends on it.

   The cost is settled rather than deferred: streamed tracks have no route to a
   grid and will not get one (§8.3). Owned music via the library index is the
   product; streaming is `Unscored`.
5. Rust edition and MSRV. §1 says 2021 / 1.75+, written before this dependency set;
   edition 2024 (Rust 1.85+) is likely the better default. Settle before `cargo new`.
6. ~~Does Yandex need ynison push-position?~~ **Resolved: `yamuse` chosen**, and
   then largely undercut on 2026-08-15 — the Yandex Music desktop app publishes to
   SMTC normally, so only browser playback is invisible. Streaming is permanently
   `Unscored` and precise position buys nothing visible. The live question at M6 is
   whether to build the adapter at all. See §6.4.
