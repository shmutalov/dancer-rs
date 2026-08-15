# Predictive Desktop Dancer — Implementation Spec

**Working title:** `dancer-rs` (placeholder)
**Target language:** Rust (2021 edition, MSRV 1.75+)
**Primary platform:** Windows 10 2004+ / Windows 11
**Status:** Draft v1 — design frozen enough to start M0

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
- Graceful degradation to reactive mode for unknown tracks, and automatic
  improvement on subsequent plays of the same track.

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
│   ├── dancer-score/       Score types, JSON (de)serialisation, cache store
│   ├── dancer-clock/       BeatClock, drift correction, phase estimation
│   ├── dancer-choreo/      Move selection, anticipation scheduling
│   ├── dancer-source/      `Source` trait + adapters (smtc, spotify, yandex, file)
│   ├── dancer-audio/       WASAPI loopback capture, ring buffer, level metering
│   ├── dancer-analyze/     Reactive DSP + sidecar client for offline analysis
│   └── dancer-app/         Binary: wiring, tray icon, config, state machine
└── sidecar/                Python: allin1 wrapper, JSON-over-stdio protocol
```

### 3.2 Threading model

The render thread owns all authoritative state. Everything else sends messages in.
No shared mutable state, no locks in the render path.

| Thread | Purpose | Cadence |
|---|---|---|
| **Render** (main) | winit event loop, clock evaluation, blitting | 60 Hz, vsync-ish |
| **Source poll** | tokio runtime; HTTP polls to Spotify/Yandex | 2–5 s |
| **SMTC listener** | WinRT event subscriptions (session/media/timeline changed) | event-driven |
| **Audio capture** | WASAPI loopback, fills ring buffer, computes RMS | ~10 ms buffers |
| **Analysis** | Owns sidecar subprocess; long-running jobs | on demand |

Communication: `crossbeam-channel` for thread→render messages. One `enum AppEvent`
consumed by the render loop each frame. The render thread never blocks.

```rust
enum AppEvent {
    TrackChanged { id: TrackId, meta: TrackMeta },
    PositionReport { pos_ms: u64, playing: bool, at: Instant },
    PlaybackStopped,
    ScoreReady { id: TrackId, score: Arc<Score> },
    AudioLevel { rms: f32, at: Instant },
    AudioSilent,
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

One JSON file per track, cached on disk, keyed by canonical track ID.

```json
{
  "schema": 1,
  "track_id": "spotify:4uLU6hMCjMI75M1A2tKUQC",
  "duration_ms": 214000,
  "bpm": 128.02,
  "source": "allin1",
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

- `beats` / `beat_positions` / `downbeats` / `segments` map 1:1 onto `allin1`'s
  output. Do not invent a different shape; ingest theirs verbatim and add fields.
- `energy` is computed by the sidecar as normalised RMS over the segment.
- `cues` are derived, not from allin1: `build` is emitted for the last bar before a
  segment boundary where energy rises by more than a threshold; `drop` is the
  boundary itself. These drive anticipation moves.
- `confidence` gates entry into the `Locked` state. Below ~0.6, stay reactive.

### 5.1 Cache

```
%LOCALAPPDATA%\dancer-rs\scores\{source}\{track_id}.json
%LOCALAPPDATA%\dancer-rs\recordings\{source}\{track_id}.wav   (transient)
```

Track IDs are namespaced per source. The same song from Spotify and from Yandex
gets two entries — masters differ, and a beat grid off by 40 ms looks broken.

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

`observed_at` must be sampled immediately after the underlying read returns, not
when the message is handled. Everything downstream depends on that pairing.

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

**Caveats to handle:**
- `Position` is only refreshed on state change, not continuously. Use
  `LastUpdatedTime` as the anchor timestamp, never `Instant::now()` at read time.
- Some sessions publish no timeline at all. Detect and fall back to audio-only.
- Track identity is `(title, artist)`, not a stable ID. Normalise (trim, casefold,
  strip `- Remastered` style suffixes) and hash for the cache key.

### 6.3 Spotify adapter (optional, better IDs)

`rspotify` with OAuth PKCE. Endpoint `GET /v1/me/player/currently-playing` gives
`progress_ms`, `is_playing`, and a stable track URI.

Advantages over SMTC: canonical track ID, works when Spotify plays on another
device (phone, Connect speaker) where SMTC sees nothing.

Disadvantages: rate-limited HTTP, ~1 s granularity, network latency in the
`observed_at` pairing, requires user auth flow.

**Verify before building:** Spotify's `audio-analysis` and `audio-features`
endpoints historically returned a full beat/bar/section breakdown — which would
be a drop-in replacement for the whole offline analysis pipeline — but access was
restricted for new applications. Check current developer docs first. If open, add
a `SpotifyAnalysisProvider` alongside the sidecar; if closed, this is only an
identity/position source.

### 6.4 Yandex Music adapter (optional)

No official public API. Community wrappers exist around the internal API and
require extracting an OAuth token from the desktop client or web session. Treat as
a moving target: put it behind a feature flag, keep SMTC as the fallback for the
same player, and do not let a Yandex API break take down the binary.

### 6.5 Local file adapter (development)

Plays nothing; reads a WAV/MP3 path plus a simulated transport. Exists so the
clock, scheduler and renderer can be tested deterministically without any
streaming service in the loop. **Build this first** — M0 through M2 depend on it.

---

## 7. Audio capture

Capture is **not** the primary sync mechanism. It serves four secondary purposes,
all of which matter:

1. **Silence watchdog.** Metadata lies. If RMS sits below the floor for >300 ms
   while state claims Playing, freeze.
2. **Offset calibration.** Cross-correlate live onsets against the score's beat
   grid to measure end-to-end output latency, once per source app.
3. **Recording for later analysis.** See §8.2.
4. **Reactive fallback** for unknown tracks.

### 7.1 WASAPI loopback

Use the `wasapi` crate (explicit loopback support) rather than `cpal`, whose
loopback story on Windows is less direct. Verify against the current version of
both before committing.

Default device loopback captures the full system mix — including Discord, browser
notifications and Windows sounds, all of which corrupt onset detection.

**Prefer per-process loopback.** Windows 10 build 20348+ supports capturing a
single process tree via `ActivateAudioInterfaceAsync` with
`AUDIOCLIENT_ACTIVATION_PARAMS` set to `PROCESS_LOOPBACK` and
`PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE`, targeting the PID of
`Spotify.exe` or the browser. This is exactly the "Windows Sound Mixer" per-app
routing the volume mixer shows, exposed programmatically. Resolve the PID from the
SMTC session's `SourceAppUserModelId`. Fall back to full-mix loopback when the API
is unavailable or the process can't be resolved.

### 7.2 Buffer and DSP

- 44.1 kHz, mono downmix, f32.
- 1024-sample analysis window, 512 hop (≈11.6 ms) — fine enough for onsets.
- `realfft` for the FFT. Spectral flux = sum of positive magnitude deltas.
- Ring buffer of 30 s for calibration correlation; separate streaming WAV writer
  for recording mode.

### 7.3 A note on recording

Recordings are local, transient, and used only to derive a beat grid — the WAV is
deleted once the score is written. Streaming services' terms generally prohibit
capturing their output, so this must be off by default, disclosed clearly in the
UI, and user-enabled. If it stays off, the app simply never leaves reactive mode
for streamed tracks.

---

## 8. Analysis

### 8.1 Offline sidecar

`allin1` (All-In-One Music Structure Analyzer, MIT) produces tempo, beats,
downbeats, `beat_positions`, and labelled functional segments — intro, verse,
chorus, bridge, outro — in one pass. It is PyTorch-based. **Do not attempt to port
it to Rust.**

Ship it as a subprocess sidecar speaking newline-delimited JSON over stdio:

```
→ {"cmd":"analyze","job":7,"path":"C:\\...\\track.wav","track_id":"..."}
← {"job":7,"status":"progress","pct":40}
← {"job":7,"status":"done","score":{ ... }}
```

The sidecar also computes the `energy` and `cues` fields, which allin1 does not
provide.

**Known deployment risk:** allin1 depends on NATTEN, which on Windows must be
built from source. This is a genuine barrier for end users. Mitigations, in order
of preference: ship a prebuilt sidecar bundle with a frozen environment; offer WSL
as a documented alternative; or make the sidecar entirely optional and accept
reduced quality from the built-in analyzer below.

### 8.2 Built-in fallback analyzer (pure Rust)

Because the sidecar may be absent, `dancer-analyze` implements a degraded
offline path in Rust: spectral flux onset envelope → autocorrelation for tempo →
comb-filter phase estimation for downbeats → fixed 8-bar blocks in place of real
segmentation, with energy tiers from RMS. Produces the same `Score` struct with
`source: "builtin"` and lower `confidence`. Good enough to beat-lock; not good
enough to know a chorus is coming.

### 8.3 Learn-on-second-listen

The behaviour that makes streaming sources work at all:

1. Unknown track starts. No score → `Reactive`.
2. If recording is enabled, capture loopback to a temp WAV.
3. Track plays to completion without seeking → hand the WAV to the analyzer.
4. Score is cached under the track ID; WAV deleted.
5. Next time that track plays → `Locked`, predictive, from the first beat.

Discard the recording if the user seeks, skips, or pauses for more than a few
seconds — a discontinuous capture produces a corrupt beat grid, and a confidently
wrong grid is worse than no grid.

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

### 9.2 Offset calibration

`offset` absorbs everything between "the player says it is at 42.0 s" and "the
sound reaches the speakers": decoder buffering, WASAPI buffer, and for HTTP
sources the request round-trip.

Measure it once per source app: with a score loaded, collect 4 s of loopback
onsets, cross-correlate against the score's beat times, take the lag at peak
correlation. Store per `SourceAppUserModelId` in config. Typical range 100–300 ms.
Expose a manual nudge slider — users will want to trim it by eye.

---

## 10. State machine

| State | Meaning | Animation behaviour |
|---|---|---|
| `Idle` | Nothing playing | Default row, slow fixed fps, or hidden |
| `Identifying` | Track known, score lookup in flight | Idle row, tempo-agnostic |
| `Reactive` | Playing, no usable score | Online tempo estimate from loopback; no anticipation |
| `Recording` | Overlaps `Reactive`; capturing for analysis | No visual difference |
| `Locked` | Score loaded, clock confident | Full predictive scheduling |
| `Resync` | Seek/skip/drift detected | Continue current row to its loop point, re-anchor |

Transitions:

- `TrackChanged` → `Identifying` from any state. Cancel queued moves.
- Score found and `confidence >= 0.6` → `Locked`. Else → `Reactive`.
- `playing == false` → freeze the clock (do not advance `anchor_local`), finish the
  current row to its loop boundary, settle into the default row. Do **not** cut
  mid-move; a hard stop reads as a crash.
- Resume → re-anchor, then wait for the next downbeat before resuming full moves.
  Starting mid-bar looks worse than a half-second of idle.
- `AudioSilent` for >300 ms while nominally playing → treat as paused.
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

Overrides, in priority order:

- A `build` cue in the score → force a row from the `build` pool for that bar.
  These are the `impact_cell = 7` anacrusis moves; they exist to be visibly
  winding up while the music winds up.
- A `drop` cue or a downbeat starting a new segment → force a one-shot
  (`loopable = false`) high-energy row, aligned so its impact lands on that downbeat.
- Non-loopable rows return to `default_row` on completion.

Change moves on downbeats only, never mid-bar, unless a cue forces it.

---

## 12. Rendering

- `winit` 0.30+ for the window: `with_transparent(true)`, `with_decorations(false)`,
  `WindowLevel::AlwaysOnTop`.
- `set_cursor_hittest(false)` implements the FAOSDance "solid" toggle — clicks pass
  through to whatever is behind.
- `softbuffer` for CPU blitting. A sprite dancer does not need `wgpu`; the entire
  frame is one alpha-blended bitmap copy. Revisit only if effects are added.
- `image` for PNG decode; pre-slice cells into an `Arc<[RgbaImage]>` at load.
- Per-pixel alpha on Windows needs a layered window (`WS_EX_LAYERED`). Verify
  winit's transparency path gives real per-pixel alpha and not colour-keying; if
  not, drop to `UpdateLayeredWindow` via the `windows` crate.
- Additional extended styles: `WS_EX_TOOLWINDOW` (keep out of Alt-Tab and taskbar),
  `WS_EX_NOACTIVATE` (never steal focus).
- Dragging: while `set_cursor_hittest(true)`, mouse-down switches to the `Held` row
  and moves the window with the cursor. This is the one interaction that must feel
  immediate — bypass the scheduler entirely.
- Multi-monitor and per-monitor DPI: persist window position as
  (monitor id, normalised x/y), not absolute pixels.

---

## 13. Configuration

`%APPDATA%\dancer-rs\config.toml`, hot-reloaded via `notify`.

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

[audio]
enabled = true
per_process_loopback = true
recording_enabled = false          # off by default; see §7.3
[audio.offset_ms]
"Spotify.exe" = 180
"chrome.exe"  = 250

[analysis]
sidecar_path = "sidecar/dancer-analyze.exe"
min_confidence = 0.6

[choreo]
lookahead_ms = 2000
change_on_downbeat_only = true
```

---

## 14. Milestones

| # | Deliverable | Exit criterion |
|---|---|---|
| **M0** | Window + sprite playback | FAOSDance parity: loads an existing sheet + `.txt`, loops at fixed fps, transparent, click-through, draggable |
| **M1** | Local file source + BeatClock | Hand-written score JSON drives a beat-locked dance against a local WAV; visually in time for 3 min with no drift |
| **M2** | Anticipation scheduler | `impact_cell` respected; A/B against M1 shows the difference is visible |
| **M3** | SMTC source | Identity, position and pause/resume from Spotify desktop; correct freeze and resume-on-downbeat behaviour |
| **M4** | WASAPI loopback | Per-process capture, silence watchdog, offset calibration producing a stable measured value |
| **M5** | Analysis pipeline | Sidecar + built-in fallback; score cache; end-to-end on a local file |
| **M6** | Learn-on-second-listen | An unknown streamed track is reactive on play 1 and locked on play 2 |
| **M7** | Spotify + Yandex adapters, tray UI | Auth flows, source failover, config UI |

M0–M2 need no audio subsystem and no network. Most of the interesting work is
there. Do not start M3 until M2 looks right against a local file.

---

## 15. Dependencies

| Crate | Purpose |
|---|---|
| `winit` | Windowing, event loop, cursor hit-test |
| `softbuffer` | CPU framebuffer presentation |
| `image` | PNG decode |
| `windows` | SMTC, WASAPI, layered window styles |
| `wasapi` | Loopback capture (verify per-process support) |
| `realfft` / `rustfft` | Spectral analysis |
| `tokio` + `reqwest` | Async source polling |
| `rspotify` | Spotify Web API + OAuth PKCE |
| `serde` / `serde_json` / `toml` | Score, manifest, config |
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
| Spotify `audio-analysis` unavailable | Lose the free score path | Sidecar is the primary path anyway; treat Spotify analysis as a bonus |
| NATTEN build on Windows | Users can't install the sidecar | Prebuilt bundle; WSL path; built-in analyzer fallback |
| Yandex internal API changes | Adapter breaks | Feature flag; SMTC covers the same player |
| Recording legality / ToS | Feature must ship disabled | Off by default, explicit opt-in, local-only, WAV deleted after analysis |
| SMTC session ambiguity | Wrong app drives the dancer | Allowlist + explicit source selection in tray |
| Per-process loopback unsupported | Noisy capture | Full-mix fallback; onset detection on a bandpassed low band is fairly robust |
| Sheets lack `impact_cell` | No anticipation, back to FAOSDance behaviour | Ship an annotated default sheet; add a small cell-picker tool in M7 |

---

## 17. Open questions

1. Is Spotify's `audio-analysis` endpoint available to new apps as of now? Resolve
   before M5 — it changes the sidecar's priority substantially.
2. Should `Reactive` mode exist at all in v1, or is idle-until-known acceptable?
   It's a meaningful chunk of DSP work for a mode users may rarely see.
3. Sheet compatibility: is 8 cells worth keeping as a hard constraint, or should
   the manifest allow arbitrary widths with 8 as the default? Keeping it costs
   nothing and buys the whole existing sheet library.
4. Should scores be shareable — a small community repo of analysed track IDs — so
   users skip the learn-on-second-listen step? Distributing derived beat grids is
   likely fine; worth a closer look before designing for it.
