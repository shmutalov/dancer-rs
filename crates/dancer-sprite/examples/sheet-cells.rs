//! Throwaway: per-row contact strips plus per-cell geometry, for tagging a sheet.
//!
//! `cargo run -p dancer-sprite --example sheet-strips -- <sheet.png> <outdir> <rows>`

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let img = image::open(&a[0]).expect("open").to_rgba8();
    let out = std::path::Path::new(&a[1]);
    let rows: u32 = a[2].parse().unwrap();
    std::fs::create_dir_all(out).unwrap();

    let (w, h) = img.dimensions();
    let (cw, ch) = (w / 8, h / rows);
    let (sw, sh) = (cw / 2, ch / 2);

    println!("cell {cw}x{ch}");
    println!("{:<5} {:>6} {:>6} {:>6} {:>6}", "row", "cell", "top", "width", "cy");

    for r in 0..rows {
        let mut strip = image::RgbaImage::from_pixel(sw * 8, sh, image::Rgba([32, 32, 40, 255]));
        for c in 0..8 {
            let cell = image::imageops::crop_imm(&img, c * cw, r * ch, cw, ch).to_image();

            // Opaque-pixel geometry. `top` is how high the figure reaches (lower is
            // higher), `width` how far it spreads, `cy` where its mass sits. A jump
            // peaks at minimum `top`; a crouch maximises `cy`; an arm at full
            // extension maximises `width`.
            let (mut top, mut x0, mut x1, mut sum_y, mut n) = (ch, cw, 0u32, 0u64, 0u64);
            for y in 0..ch {
                for x in 0..cw {
                    if cell.get_pixel(x, y).0[3] > 24 {
                        if y < top {
                            top = y;
                        }
                        x0 = x0.min(x);
                        x1 = x1.max(x);
                        sum_y += y as u64;
                        n += 1;
                    }
                }
            }
            let cy = sum_y.checked_div(n).unwrap_or(0);
            let width = x1.saturating_sub(x0);
            println!("{r:<5} {c:>6} {top:>6} {width:>6} {cy:>6}");

            let small = image::imageops::resize(&cell, sw, sh, image::imageops::FilterType::Triangle);
            image::imageops::overlay(&mut strip, &small, (c * sw) as i64, 0);
            for y in 0..sh {
                strip.put_pixel(c * sw, y, image::Rgba([200, 60, 60, 255]));
            }
        }
        strip.save(out.join(format!("row{r}.png"))).unwrap();
    }
}
