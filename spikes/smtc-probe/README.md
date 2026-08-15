# Phase 0.5 — SMTC on the GNU toolchain

Answers ROADMAP.md §0.5: does WinRT work on `stable-x86_64-pc-windows-gnu`, and
does SMTC give us what spec §6.2 requires? Written because §0.4 rules out MSVC and
SMTC is the entire content of M4.

## Run

```sh
cargo run --release      # needs something playing to be interesting
```

## Result — 2026-08-15: **PASS**

Windows 10 Enterprise LTSC 2021 (19044), rustc 1.95.0 GNU, `windows` 0.62.2.

With a YouTube video playing in Edge:

```
sessions: 1
[current] msedge.exe
  title  : "Blur - Song 2 (Official Music Video)"
  artist : "Blur"
  status : Ok(4)  rate 1.0
  position: 0.019s / 122.541s  (as reported)
  anchor  : LastUpdatedTime is 59.646s old
  EXTRAPOLATED position = 59.666s   <- what the clock must use
```

Every call the source adapter needs works: session enumeration, media properties,
playback info, timeline, and a non-zero `LastUpdatedTime`. WinRT is fine on GNU.

**API note:** the blocking accessor on `IAsyncOperation` is `join()`, not `get()`.
`PlaybackStatus` 4 = Playing.

## Three findings that matter more than the pass

### 1. `Position` really is stale — by a minute, here

Reported position was **0.019 s** while the track was actually **59.7 s** in. The
value was captured when playback started and never refreshed.

This is spec §6.2's warning made concrete: anchor on `LastUpdatedTime`, never
`Instant::now()` at read time. Naively trusting `Position` would have put the
dancer a full minute out on a two-minute track. The `BeatClock` anchor design
(§9) is not an optimisation — without it SMTC data is unusable.

### 2. Yandex Browser does not publish to SMTC

With Yandex Music actively playing in Yandex Browser, `GetSessions()` returned
**zero** — repeatedly, unpaused, no error. Edge on the same machine returns a
session immediately, so SMTC works and the browser is the blind spot.

No OS-level cause: no policy keys disabling media controls, Yandex Browser running
default flags (`enable-quic` only), and the edition is EnterpriseS with media
features present.

Consequence: for users whose player does not publish, the dancer never learns
anything is playing at all — `Idle`, not `Unscored`. See ROADMAP §5.8; this is
what turns `yamuse` from a precision upgrade into a presence one.

Not tested: Chrome, Firefox, Opera, AIMP, foobar2000, VLC, Spotify desktop. Worth a
compatibility sweep before M4 exits, since "which players publish" determines how
much of M4's value is real.

### 3. Titles need real normalisation

Edge reported title `"Blur - Song 2 (Official Music Video)"` with artist `"Blur"`.

To match that against a library file (spec §8.3) the normaliser has to strip the
`(Official Music Video)` suffix *and* cope with the artist being duplicated into
the title. This is the first real specimen for the awkward-title fixture set M4
calls for — and it arrived from the very first track tested, which suggests the
problem is the common case rather than an edge case.

## Caveat

One machine, two browsers, one track. Enough to prove the ABI and the anchor
mechanism; not a compatibility survey.
