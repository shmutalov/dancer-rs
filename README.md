# dancer-rs

A sprite dances on your desktop, in time with whatever you are playing.

[![CI](https://github.com/shmutalov/dancer-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/shmutalov/dancer-rs/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/shmutalov/dancer-rs)](https://github.com/shmutalov/dancer-rs/releases/latest)

In the spirit of FL Studio's Fruity Dance and of FAOSDance, whose sheet format it
reads. What makes it different is the one thing it is built around.

## It anticipates the beat

Every desktop dancer reacts: it hears a beat, then moves. That is late by however
long it takes to notice, and it looks late, because the eye is unforgiving about
exactly this.

A move has an **impact frame** — knees deepest, arm fully raised, the top of a
jump — and that frame is what the eye reads as *on the beat*. So the move cannot
start on the beat. It has to start before it:

    start_at = beat − impact_cell × frame_duration − render_latency

Which means the beat has to be known in advance, which is why the beat grid is
analysed ahead of time rather than detected live. The dancer is not following the
music; it is reading from the same score.

Middle-click toggles the lead off and on, changing nothing else, so you can watch
the difference on a chorus.

## Getting started

Download the zip from [releases](https://github.com/shmutalov/dancer-rs/releases/latest),
unzip anywhere, run `dancer-rs.exe`. Everything it writes stays in that folder:
no installer, nothing in the registry, nothing scattered across your profile.

Analyse your music once:

    dancer-rs.exe --scan D:/music

Then play something in any player — foobar2000, AIMP, VLC, Winamp, a browser
tab. Windows reports what is playing and where; if the track is one you have
analysed, the dancer locks to its grid.

**Trim the offset.** There is always a fixed delay between "the player says 42.0
seconds" and the sound reaching your ears — output buffer, DAC, Bluetooth. The
tray menu nudges it in 5 ms and 25 ms steps and saves as you go. At 128 BPM a beat
is 469 ms, so leaving it untrimmed can put the dancer half a beat out.

| | |
|---|---|
| Left drag | move the dancer |
| Middle click | toggle anticipation |
| Right click | quit |
| Tray | dancer, offset, click-through, always-on-top, Yandex sign-in |

## What syncs, and what does not

Tracks you own and have analysed sync fully.

Streamed tracks do not have a grid, because analysing music needs a file to read
and there is no file. The dancer still follows play, pause and track changes, and
falls back to a loop that is honest about knowing nothing.

The exception is Yandex Music: signed in, a track you are playing can be fetched,
analysed, and the audio **deleted immediately**, keeping only the beat grid. You
start it, it is off until you sign in, and nothing is stored or shared.

## Dancers

Any FAOSDance or Fruity Dance sheet works — a PNG exactly 8 cells wide, one row
per animation, plus a `.txt` naming the rows. Drop it in `assets/` and pick it
from the tray.

A `.toml` beside the PNG is what turns a loop into choreography: which cell is
each row's accent, how many beats the loop takes, how strenuous it is, and what
kind of movement it is — a step, a turn, a jump. See `assets/default.toml`, and
check your own with `--check-sheet`.

**Sprite sheets are other people's work.** The app links to places they can be
downloaded from and warns before opening each one, but those links are pointers,
not permission. Nothing is bundled or redistributed here beyond the plain default
sheet, which exists precisely so that nothing else has to be — FL-Chan is
Image-Line's artwork and is deliberately absent from this repository and from
every release.

## Building

    cargo build --release
    cargo test --workspace

Windows only, and **GNU only** — `rust-toolchain.toml` pins
`stable-x86_64-pc-windows-gnu`. The MSVC target needs the ~2 GB Windows SDK, and
every surface used here is verified working on GNU: `rten` inference,
`UpdateLayeredWindow`, rusqlite's C build, and WinRT/SMTC.

To assemble a release package, which also fetches the ONNX weights and checks
them against pinned SHA-256 hashes:

    pwsh packaging/build-release.ps1

Pushing a `v*` tag does the same thing in CI and publishes the result.

| Crate | |
|---|---|
| `dancer-choreo` | anticipation scheduling — the thing the project exists for |
| `dancer-analyze` | beat grids from `beat-this` on the `rten` backend |
| `dancer-clock` | position estimation and drift correction |
| `dancer-score` | the grid: beats, downbeats, energy, and queries over them |
| `dancer-source` | what is playing and where, via the system media session |
| `dancer-sprite` | sheet loading, FAOSDance-compatible |
| `dancer-render` | layered window, per-pixel alpha, no frame |
| `dancer-yandex` | fetch a streamed track, analyse it, delete it |
| `dancer-app` | the binary: window, tray, config, hot reload |

## Status

M0 through M5 are done: playback, beat grids, the anticipation scheduler, the
media-session source with a library scanner, and a tray with packaging. M3's A/B
apparatus was rebuilt on 2026-08-18 and its verdict is still open.

[`dancer-spec.md`](dancer-spec.md) is the design and the reasoning behind it;
[`ROADMAP.md`](ROADMAP.md) is what happened, including what was cut and why.

## Licence

MIT — see [LICENSE](LICENSE). The sheet format is inherited from FAOSDance (MIT).
Beat tracking is `beat-this`; its model weights carry their own upstream licence.
**No artwork is covered by that licence**, and none is redistributed here.
