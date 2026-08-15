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

### 2. Yandex Browser does not publish to SMTC — but the desktop app does

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

## Re-run — 2026-08-15, Yandex Music **desktop** app

Same machine, Yandex Music desktop installed and playing. It publishes normally:

```
sessions: 1
[current] Яндекс Музыка.exe
  title  : "Trance-Atlantic"
  artist : "Scooter"
  status : Ok(4)  rate 1.0
  position: 43.172s / 470.692s  (as reported)
  anchor  : LastUpdatedTime is 86.967s old
  EXTRAPOLATED position = 130.139s
```

**This resolves the M6 open question** (spec §6.4, ROADMAP §5.8): the blind spot is
Yandex *Browser*, not Yandex Music as a service. `yamuse` therefore narrows to
covering the web player only, and the honest advice for a Yandex user is "install
the desktop app" — which costs the project nothing.

Two further results from the re-run:

**The staleness is not an Edge quirk.** A second, unrelated application shows the
same behaviour at larger magnitude — 87 s stale on first read. Spec §6.2 now rests
on two independent applications rather than one browser.

**Extrapolation tracks wall clock exactly.** Sampled again 33 s later: reported
`Position` still frozen at `43.172s`, anchor aged 86.967 → 120.153 s (Δ 33.186 s),
extrapolated 130.139 → 163.325 s (Δ 33.186 s). The anchor does not drift against
wall clock while playback continues uninterrupted, which is the property `BeatClock`
depends on.

Caveat on that: it proves the arithmetic is self-consistent, not that the
extrapolated value equals true playback position. Confirming *that* needs a known
seek point, which belongs in M1's fixtures rather than here.

**New constraint for M4: `SourceAppUserModelId` is not ASCII.** This session
identifies as `Яндекс Музыка.exe` — a localised executable name. Spec §6.2's
allowlist cannot be a hardcoded English string, must compare as Unicode, and should
be populated from observed sessions in the tray rather than shipped as a fixed list.

### 3. Titles vary by source — which argues *against* normalising

Edge reported title `"Blur - Song 2 (Official Music Video)"` with artist `"Blur"`.

The first reading was that this demands aggressive normalisation: strip the
parenthetical, de-duplicate the artist out of the title. That is the wrong
conclusion. The same song appears as:

```
Blur - Song 2
Blur Song 2
Blur - Song 2 (Official Music Video)
Blur - Song 2 (Radio Edit)
```

A radio edit is a *different recording* — different length, different grid.
Canonicalising the content merges it with the album version and applies the wrong
timeline, which is a false positive. Hashing the raw string at worst produces a
miss. Spec §8.3 already holds that a confidently wrong grid is worse than none, so
the miss is the correct failure.

The common case never needed normalisation anyway: playing a local file through an
ordinary player makes SMTC report *that file's own tags*, so the strings match
exactly because they came from the same place.

Resolution (spec §5.1): hash the raw strings, with encoding-level normalisation
only — trim, NFC, casefold — and verify duration on match, ±2 s. The probe gets
duration free from `EndTime`, which read 122.541 s here.

## Caveat

One machine, two browsers, one track. Enough to prove the ABI and the anchor
mechanism; not a compatibility survey.
