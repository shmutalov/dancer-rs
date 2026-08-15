# Phase 0.2 — per-pixel alpha probe

Answers ROADMAP.md §0.2: does the render path give real per-pixel alpha on Windows?

## Run

```sh
cargo run --release
```

Opens a 400x200 always-on-top window at (300,300) for about a second, then exits.
It captures the screen region before and after showing an alpha ramp and checks the
composited pixels against the blend that ramp should have produced, so the result
is measured rather than eyeballed.

## Result — 2026-08-15

Windows 10 build 19044, Rust 1.95.0 GNU, winit 0.30.13, softbuffer 0.4.8,
windows 0.62.2.

**softbuffer cannot do this, by contract.** Its documented pixel format is
`00000000RRRRRRRRGGGGGGGGBBBBBBBB` — the top 8 bits are specified as zero, there is
no alpha channel — and the Win32 backend presents with `BitBlt(SRCCOPY)`, an opaque
copy. This is not a winit question, which is where spec §12 was looking; the
blocker sits one layer lower, and no winit setting can route around it.

**winit + `UpdateLayeredWindow` passes cleanly.**

| alpha | backdrop | expected | measured | delta |
|---|---|---|---|---|
| 0 | 193,151,91 | 193,151,91 | 193,151,91 | 0 |
| 64 | 30,30,30 | 86,22,22 | 86,22,22 | 0 |
| 128 | 30,30,30 | 142,14,14 | 143,15,15 | 1 |
| 192 | 30,30,30 | 199,7,7 | 199,7,7 | 0 |
| 255 | 30,30,30 | 255,0,0 | 255,0,0 | 0 |

Worst channel delta 1, which is rounding. All four extended styles applied on a
winit-created window: `WS_EX_LAYERED`, `WS_EX_TOOLWINDOW`, `WS_EX_NOACTIVATE`,
`WS_EX_TRANSPARENT`.

The alpha=0 band happened to sit over a different backdrop on the second run and
still matched exactly — incidental confirmation that full transparency holds
regardless of what is behind it.

**Present cost is negligible.**

| Size | ms/frame | share of a 16.7 ms budget |
|---|---|---|
| 128x128 | 0.066 | 0.4% |
| 256x256 | 0.066 | 0.4% |
| 512x512 | 0.112 | 0.7% |

`UpdateLayeredWindow` is comfortably viable as the 60 Hz present path. The concern
that a per-frame full-window GDI blit might be too slow does not survive contact.

## Consequences

- **Drop `softbuffer`.** Spec §12 and §15 updated.
- **Premultiply at load.** `UpdateLayeredWindow` requires premultiplied BGRA, so
  sprite cells must be premultiplied when sliced, not per frame.
- **Presentation is not `WM_PAINT`.** The whole window surface is replaced by the
  `UpdateLayeredWindow` call; winit's redraw path goes unused. The render loop
  becomes "build a premultiplied BGRA DIB, call `UpdateLayeredWindow`".
- **Click-through is `WS_EX_TRANSPARENT`**, toggled off for the drag interaction.

## Caveats

- One machine, one GPU, Windows 10 build 19044. Re-check on Windows 11, and on a
  high-DPI multi-monitor setup, before M0 is called done.
- The benchmark reuses one DIB and does not include the cost of compositing sprite
  cells into it. That work is a memcpy per frame at these sizes and will not change
  the conclusion.
