//! Generates the neutral default sprite sheet.
//!
//! Spec §1.3: Fruity Dance's FL-Chan artwork is Image-Line's and must not be
//! redistributed, so the project ships its own. This draws a deliberately abstract
//! figure — no character, no likeness — at 110x128 per cell, matching FL-Chan's
//! geometry so the sheet also exercises non-square cells.
//!
//! Rendered 3x supersampled and box-filtered down, which produces genuine alpha
//! ramps at the edges. That matters: it is what makes the sheet a real test of the
//! per-pixel alpha path from Phase 0.2 rather than a hard-edged silhouette.
//!
//!     cargo run -p dancer-sprite --example gen-sheet

use std::f32::consts::TAU;

const CELL_W: u32 = 110;
const CELL_H: u32 = 128;
const CELLS: u32 = 8;
const SS: u32 = 3; // supersample factor

const BODY: [u8; 3] = [0x3f, 0xb5, 0xaf]; // teal
const BODY_DK: [u8; 3] = [0x2c, 0x8c, 0x89];
const LIMB: [u8; 3] = [0xf2, 0x8b, 0x6b]; // coral
const DARK: [u8; 3] = [0x1e, 0x2a, 0x32];

/// Rows, in sheet order. `Held` last, per FAOSDance convention.
const ROWS: &[&str] = &["idle", "bounce", "spin", "Held"];

fn main() -> anyhow::Result<()> {
    let out_dir = std::path::Path::new("assets");
    std::fs::create_dir_all(out_dir)?;

    let w = CELL_W * CELLS;
    let h = CELL_H * ROWS.len() as u32;
    let mut sheet = image::RgbaImage::new(w, h);

    for (r, row) in ROWS.iter().enumerate() {
        for c in 0..CELLS {
            let t = c as f32 / CELLS as f32; // phase within the loop
            let cell = draw_cell(row, t);
            for y in 0..CELL_H {
                for x in 0..CELL_W {
                    sheet.put_pixel(
                        c * CELL_W + x,
                        r as u32 * CELL_H + y,
                        *cell.get_pixel(x, y),
                    );
                }
            }
        }
    }

    let png = out_dir.join("default.png");
    sheet.save(&png)?;

    // The inherited `.txt` sidecar: one row name per line.
    std::fs::write(
        out_dir.join("default.txt"),
        ROWS.iter().map(|r| format!("{r}\n")).collect::<String>(),
    )?;

    // And the extended manifest, so the default sheet demonstrates §4.2. The
    // choreography fields are unused until M3 but valid now.
    std::fs::write(
        out_dir.join("default.toml"),
        r#"# Neutral default sheet. See spec §4.2.
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
energy = 0.15
loopable = true

[[row]]
name = "bounce"
index = 1
impact_cell = 3        # knees deepest here — schedule the START before the beat
beats_per_loop = 1
pools = ["verse", "chorus"]
energy = 0.55
loopable = true

[[row]]
name = "spin"
index = 2
impact_cell = 4
beats_per_loop = 2
pools = ["chorus"]
energy = 0.9
loopable = true

[[row]]
name = "Held"
index = 3
impact_cell = 0
pools = []
"#,
    )?;

    println!("wrote {} ({}x{}, {} rows)", png.display(), w, h, ROWS.len());
    Ok(())
}

/// Paint one cell at animation phase `t` in 0..1.
fn draw_cell(row: &str, t: f32) -> image::RgbaImage {
    let (sw, sh) = (CELL_W * SS, CELL_H * SS);
    let mut buf = vec![[0f32; 4]; (sw * sh) as usize];
    let mut p = Painter {
        buf: &mut buf,
        w: sw,
        h: sh,
    };

    let a = t * TAU;
    // Per-row motion. Kept crude on purpose — this is a placeholder that has to
    // read clearly at a glance, not a character animation.
    // Arm angle is measured from straight-down, positive outward, so a base of 0
    // hangs and ~2.5 rad reaches up. Both arms share the base and take the swing
    // in antiphase, which keeps poses symmetric and inside the cell.
    let (bob, squash, lean, arm_base, arm_swing, spin) = match row {
        "idle" => (a.sin() * 2.5, 1.0 + a.sin() * 0.02, 0.0, 0.35, a.sin() * 0.18, 0.0),
        // Two bounces per loop so cell 3 lands at the bottom of the first dip.
        "bounce" => {
            let b = (a * 2.0).sin();
            (b * 9.0, 1.0 - b * 0.10, 0.0, 0.55 - b * 0.35, b * 0.30, 0.0)
        }
        "spin" => (a.sin() * 3.0, 1.0, a.cos() * 0.18, 0.85, 0.22, t),
        // Held: dangling from the cursor, so both arms go up.
        _ => (a.sin() * 1.5, 0.98, a.sin() * 0.10, 2.45, a.sin() * 0.12, 0.0),
    };

    let cx = CELL_W as f32 / 2.0;
    let ground = 118.0;
    let body_ry = 30.0 * squash;
    let body_cy = ground - body_ry - 6.0 + bob;
    let head_r = 21.0;
    let head_cy = body_cy - body_ry - head_r * 0.55 + bob * 0.3;
    let head_cx = cx + lean * 18.0;

    // Legs first so the body overlaps them.
    for s in [-1.0f32, 1.0] {
        p.capsule(
            cx + s * 11.0,
            body_cy + body_ry * 0.5,
            cx + s * 13.0,
            ground,
            7.0,
            LIMB,
        );
    }

    // Arms swing in antiphase around a shared base angle.
    for s in [-1.0f32, 1.0] {
        let sh_x = cx + s * 21.0;
        let sh_y = body_cy - body_ry * 0.25;
        let angle = arm_base + s * arm_swing;
        let len = 29.0;
        p.capsule(
            sh_x,
            sh_y,
            sh_x + s * angle.sin() * len,
            sh_y + angle.cos() * len,
            7.0,
            LIMB,
        );
    }

    p.ellipse(cx, body_cy, 27.0, body_ry, BODY);
    // A darker band gives the spin row something to read as rotation.
    let band = (spin * TAU).sin() * 16.0;
    p.ellipse(cx + band, body_cy + 6.0, 8.0, body_ry * 0.55, BODY_DK);

    p.ellipse(head_cx, head_cy, head_r, head_r * 0.96, BODY);

    // Eyes. Closed (a flat bar) on the deepest bounce frame, which reads as effort.
    let eye_y = head_cy - 2.0;
    let closed = row == "bounce" && (0.30..0.45).contains(&t);
    for s in [-1.0f32, 1.0] {
        let ex = head_cx + s * 7.5;
        if closed {
            p.capsule(ex - 3.0, eye_y, ex + 3.0, eye_y, 1.6, DARK);
        } else {
            p.ellipse(ex, eye_y, 3.0, 3.6, DARK);
        }
    }

    downsample(&buf, sw, sh)
}

struct Painter<'a> {
    buf: &'a mut [[f32; 4]],
    w: u32,
    h: u32,
}

impl Painter<'_> {
    fn blend(&mut self, x: i32, y: i32, rgb: [u8; 3]) {
        if x < 0 || y < 0 || x as u32 >= self.w || y as u32 >= self.h {
            return;
        }
        // Opaque source over whatever is there; supersampling supplies the AA.
        self.buf[(y as u32 * self.w + x as u32) as usize] =
            [rgb[0] as f32, rgb[1] as f32, rgb[2] as f32, 255.0];
    }

    fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32, rgb: [u8; 3]) {
        let (cx, cy, rx, ry) = (cx * SS as f32, cy * SS as f32, rx * SS as f32, ry * SS as f32);
        let x0 = (cx - rx).floor() as i32;
        let x1 = (cx + rx).ceil() as i32;
        let y0 = (cy - ry).floor() as i32;
        let y1 = (cy + ry).ceil() as i32;
        for y in y0..=y1 {
            for x in x0..=x1 {
                let dx = (x as f32 - cx) / rx;
                let dy = (y as f32 - cy) / ry;
                if dx * dx + dy * dy <= 1.0 {
                    self.blend(x, y, rgb);
                }
            }
        }
    }

    /// A line with rounded caps — good enough for limbs.
    fn capsule(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, r: f32, rgb: [u8; 3]) {
        let steps = ((x1 - x0).hypot(y1 - y0) * SS as f32).ceil().max(1.0) as u32;
        for i in 0..=steps {
            let f = i as f32 / steps as f32;
            self.ellipse(x0 + (x1 - x0) * f, y0 + (y1 - y0) * f, r, r, rgb);
        }
    }
}

/// Box-filter the supersampled buffer down to cell size, producing edge alpha.
fn downsample(buf: &[[f32; 4]], sw: u32, sh: u32) -> image::RgbaImage {
    let mut out = image::RgbaImage::new(CELL_W, CELL_H);
    let n = (SS * SS) as f32;
    for y in 0..CELL_H {
        for x in 0..CELL_W {
            let mut acc = [0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = buf[((y * SS + sy) * sw + (x * SS + sx)) as usize];
                    // Weight colour by coverage so transparent samples do not
                    // drag the hue toward black.
                    let cov = px[3] / 255.0;
                    acc[0] += px[0] * cov;
                    acc[1] += px[1] * cov;
                    acc[2] += px[2] * cov;
                    acc[3] += px[3];
                }
            }
            let a = acc[3] / n;
            let wsum = (acc[3] / 255.0).max(1e-6);
            out.put_pixel(
                x,
                y,
                image::Rgba([
                    (acc[0] / wsum) as u8,
                    (acc[1] / wsum) as u8,
                    (acc[2] / wsum) as u8,
                    a as u8,
                ]),
            );
        }
    }
    let _ = sh;
    out
}
