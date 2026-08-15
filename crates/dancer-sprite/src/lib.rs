//! Sprite sheet loading, compatible with the FAOSDance / Fruity Dance format.
//!
//! See spec §4. The inherited format is a PNG exactly 8 cells wide with one row
//! per animation, plus a `.txt` sidecar naming the rows one per line. An optional
//! `.toml` manifest (§4.2) supersedes the `.txt` and carries choreography metadata.
//!
//! Cells are pre-sliced and **premultiplied** at load, because the render path is
//! `UpdateLayeredWindow`, which requires premultiplied BGRA (Phase 0.2). Doing it
//! per frame would be pure waste.

use std::path::{Path, PathBuf};
use std::sync::Arc;

mod manifest;
pub use manifest::{Manifest, RowManifest};

/// Cells per row in the inherited format. Kept as a hard default; the manifest may
/// override it (spec §17.3).
pub const DEFAULT_CELLS_PER_ROW: u32 = 8;

/// Row name that, by FAOSDance convention, is played while dragging.
pub const HELD: &str = "Held";

#[derive(Debug, thiserror::Error)]
pub enum SheetError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decoding {path}: {source}")]
    Decode {
        path: PathBuf,
        #[source]
        source: image::ImageError,
    },
    #[error("parsing manifest {path}: {source}")]
    Manifest {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("sheet {path} is {width}px wide, not divisible by {cells} cells")]
    NotDivisible {
        path: PathBuf,
        width: u32,
        cells: u32,
    },
    #[error("sheet {path} has zero rows")]
    NoRows { path: PathBuf },
}

/// One animation row: a horizontal strip of equally sized cells.
#[derive(Debug, Clone)]
pub struct Row {
    pub name: String,
    /// Premultiplied BGRA, one `u32` per pixel, `0xAARRGGBB` in native order.
    /// Indexed `[cell][y * width + x]`.
    pub cells: Arc<[Arc<[u32]>]>,
    /// How many beats one pass through this row occupies (spec §4.2).
    ///
    /// Relative to the score's `meter`, not assumed to be four. Defaults to 1, so
    /// a sheet with no manifest animates one loop per beat.
    pub beats_per_loop: u32,
    /// Cell whose artwork is the move's accent — the one that must land *on* the
    /// beat (spec §11.2). Carried from M0; the scheduler that uses it is M3.
    pub impact_cell: u32,
    /// Segment labels this row suits, e.g. `["chorus"]` (spec §11.3).
    pub pools: Arc<[String]>,
    /// Where this row sits on the energy scale, `0.0..1.0`. `None` when the sheet
    /// declares nothing — most inherited sheets, which have no manifest at all.
    pub energy: Option<f32>,
    /// Whether the row can repeat. A one-shot returns to the default row when done.
    pub loopable: bool,
}

/// A loaded sprite sheet, ready to blit.
#[derive(Debug, Clone)]
pub struct Sheet {
    pub cell_width: u32,
    pub cell_height: u32,
    pub rows: Arc<[Row]>,
    /// Index into `rows` used when nothing else is scheduled.
    pub default_row: usize,
    /// Index of the row played while dragging, if the sheet has one.
    pub held_row: Option<usize>,
}

impl Sheet {
    /// Load `<stem>.png` plus whichever sidecar is present.
    ///
    /// Resolution order for row names and geometry, per spec §4.2:
    /// 1. `<stem>.toml` — the extended manifest, if present
    /// 2. `<stem>.txt` — one row name per line
    /// 3. neither: synthesise `row_0..row_n` and assume square cells
    pub fn load(png: &Path) -> Result<Self, SheetError> {
        let img = image::open(png)
            .map_err(|source| SheetError::Decode {
                path: png.to_owned(),
                source,
            })?
            .to_rgba8();
        let (sheet_w, sheet_h) = img.dimensions();

        let manifest = Manifest::load_beside(png)?;
        let names = read_row_names(png)?;

        let cells_per_row = manifest
            .as_ref()
            .and_then(|m| m.sheet.cells_per_row)
            .unwrap_or(DEFAULT_CELLS_PER_ROW);

        if cells_per_row == 0 || sheet_w % cells_per_row != 0 {
            return Err(SheetError::NotDivisible {
                path: png.to_owned(),
                width: sheet_w,
                cells: cells_per_row,
            });
        }
        let cell_width = sheet_w / cells_per_row;

        // Row count, in the same priority order as names.
        let row_count = manifest
            .as_ref()
            .and_then(|m| m.sheet.cell_height)
            .map(|h| (sheet_h / h).max(1))
            .or_else(|| names.as_ref().map(|n| n.len() as u32))
            .unwrap_or_else(|| {
                // No metadata at all: assume square cells. Documented guess, not a
                // silent one — see the warning below.
                (sheet_h / cell_width).max(1)
            });

        if row_count == 0 {
            return Err(SheetError::NoRows {
                path: png.to_owned(),
            });
        }
        let cell_height = sheet_h / row_count;

        if manifest.is_none() && names.is_none() {
            tracing::warn!(
                sheet = %png.display(),
                cell_width,
                cell_height,
                row_count,
                "no .toml or .txt sidecar; assuming square cells and synthesising row names"
            );
        }

        let mut rows = Vec::with_capacity(row_count as usize);
        for r in 0..row_count {
            let name = manifest
                .as_ref()
                .and_then(|m| m.row_name(r as usize))
                .or_else(|| names.as_ref().and_then(|n| n.get(r as usize).cloned()))
                .unwrap_or_else(|| {
                    // Last row is Held by convention when we are guessing.
                    if r + 1 == row_count {
                        HELD.to_string()
                    } else {
                        format!("row_{r}")
                    }
                });

            let cells: Vec<Arc<[u32]>> = (0..cells_per_row)
                .map(|c| {
                    slice_premultiplied(
                        &img,
                        c * cell_width,
                        r * cell_height,
                        cell_width,
                        cell_height,
                    )
                })
                .collect();

            let rm = manifest.as_ref().and_then(|m| m.row_at(r as usize));
            rows.push(Row {
                name,
                cells: cells.into(),
                beats_per_loop: rm.map_or(1, |m| m.beats_per_loop.max(1)),
                impact_cell: rm.map_or(0, |m| m.impact_cell),
                pools: rm.map(|m| m.pools.clone().into()).unwrap_or_else(|| Vec::new().into()),
                energy: rm.and_then(|m| m.energy),
                loopable: rm.is_none_or(|m| m.loopable),
            });
        }

        let held_row = rows.iter().position(|r| r.name.eq_ignore_ascii_case(HELD));

        let default_row = manifest
            .as_ref()
            .and_then(|m| m.sheet.default_row.as_deref())
            .and_then(|want| rows.iter().position(|r| r.name == want))
            // Never default to the Held row — it is a drag state, not an idle.
            .or_else(|| (0..rows.len()).find(|i| Some(*i) != held_row))
            .unwrap_or(0);

        tracing::info!(
            sheet = %png.display(),
            cell_width,
            cell_height,
            rows = rows.len(),
            cells_per_row,
            default = %rows[default_row].name,
            "loaded sheet"
        );

        Ok(Sheet {
            cell_width,
            cell_height,
            rows: rows.into(),
            default_row,
            held_row,
        })
    }

    pub fn cells_per_row(&self) -> usize {
        self.rows.first().map(|r| r.cells.len()).unwrap_or(0)
    }

    pub fn row_by_name(&self, name: &str) -> Option<usize> {
        self.rows.iter().position(|r| r.name == name)
    }
}

/// Copy one cell out of the sheet, premultiplying as we go.
///
/// `UpdateLayeredWindow` with `AC_SRC_ALPHA` expects premultiplied BGRA. In a
/// native-endian `u32` that is `(a << 24) | (r << 16) | (g << 8) | b`, which lands
/// in memory as B, G, R, A on little-endian — what GDI wants.
fn slice_premultiplied(
    img: &image::RgbaImage,
    x0: u32,
    y0: u32,
    w: u32,
    h: u32,
) -> Arc<[u32]> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = img.get_pixel(x0 + x, y0 + y).0;
            let a = p[3] as u32;
            // Rounded rather than truncated: truncation darkens edge pixels
            // visibly on a sprite that is mostly alpha ramp.
            let pm = |c: u8| ((c as u32 * a) + 127) / 255;
            out.push((a << 24) | (pm(p[0]) << 16) | (pm(p[1]) << 8) | pm(p[2]));
        }
    }
    out.into()
}

/// Read `<stem>.txt`: one row name per line, blank lines dropped.
fn read_row_names(png: &Path) -> Result<Option<Vec<String>>, SheetError> {
    let txt = png.with_extension("txt");
    match std::fs::read_to_string(&txt) {
        Ok(s) => {
            let names: Vec<String> = s
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect();
            Ok((!names.is_empty()).then_some(names))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(SheetError::Io { path: txt, source }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn premultiply_is_rounded_and_alpha_preserved() {
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([255, 128, 0, 128]));
        let px = slice_premultiplied(&img, 0, 0, 1, 1);
        let v = px[0];
        assert_eq!(v >> 24, 128, "alpha preserved");
        assert_eq!((v >> 16) & 0xff, 128, "255*128/255 rounds to 128");
        assert_eq!((v >> 8) & 0xff, 64, "128*128/255 rounds to 64");
        assert_eq!(v & 0xff, 0);
    }

    #[test]
    fn fully_transparent_pixels_premultiply_to_zero() {
        let mut img = image::RgbaImage::new(1, 1);
        img.put_pixel(0, 0, image::Rgba([255, 255, 255, 0]));
        assert_eq!(slice_premultiplied(&img, 0, 0, 1, 1)[0], 0);
    }
}
