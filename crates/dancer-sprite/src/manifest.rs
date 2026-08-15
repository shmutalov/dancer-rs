//! The optional `.toml` manifest (spec §4.2).
//!
//! M0 consumes only `cell_width`, `cell_height`, `default_row` and row names.
//! The choreography fields — `impact_cell`, `beats_per_loop`, `pools`, `energy`,
//! `loopable` — are parsed and carried so that manifests written now stay valid,
//! but nothing reads them until M3.

use std::path::Path;

use serde::Deserialize;

use crate::SheetError;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub sheet: SheetSection,
    #[serde(default, rename = "row")]
    pub rows: Vec<RowManifest>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SheetSection {
    pub cell_width: Option<u32>,
    pub cell_height: Option<u32>,
    pub default_row: Option<String>,
    /// Not in the original spec sketch; lets a sheet opt out of the 8-cell
    /// constraint without breaking the default (spec §17.3).
    pub cells_per_row: Option<u32>,
}

/// One `[[row]]` entry. Everything past `name`/`index` is M3 territory.
#[derive(Debug, Clone, Deserialize)]
pub struct RowManifest {
    pub name: String,
    pub index: Option<usize>,
    #[serde(default)]
    pub impact_cell: u32,
    #[serde(default = "one")]
    pub beats_per_loop: u32,
    #[serde(default)]
    pub pools: Vec<String>,
    #[serde(default)]
    pub energy: Option<f32>,
    #[serde(default = "yes")]
    pub loopable: bool,
}

fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}

impl Manifest {
    /// Load `<stem>.toml` beside the given PNG, if it exists.
    pub fn load_beside(png: &Path) -> Result<Option<Self>, SheetError> {
        let path = png.with_extension("toml");
        match std::fs::read_to_string(&path) {
            Ok(s) => toml::from_str(&s)
                .map(Some)
                .map_err(|source| SheetError::Manifest { path, source }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(SheetError::Io { path, source }),
        }
    }

    /// Name for row `r`, honouring an explicit `index` when given and otherwise
    /// falling back to declaration order.
    pub fn row_name(&self, r: usize) -> Option<String> {
        self.rows
            .iter()
            .find(|row| row.index == Some(r))
            .or_else(|| {
                // Only use positional fallback if no row declares an index at all;
                // mixing the two silently would be worse than ignoring it.
                self.rows.iter().all(|row| row.index.is_none())
                    .then(|| self.rows.get(r))
                    .flatten()
            })
            .map(|row| row.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_rows_win_over_position() {
        let m: Manifest = toml::from_str(
            r#"
            [[row]]
            name = "spin"
            index = 2
            [[row]]
            name = "idle"
            index = 0
        "#,
        )
        .unwrap();
        assert_eq!(m.row_name(0).as_deref(), Some("idle"));
        assert_eq!(m.row_name(2).as_deref(), Some("spin"));
        assert_eq!(m.row_name(1), None);
    }

    #[test]
    fn positional_fallback_when_no_indices() {
        let m: Manifest = toml::from_str(
            r#"
            [[row]]
            name = "a"
            [[row]]
            name = "b"
        "#,
        )
        .unwrap();
        assert_eq!(m.row_name(0).as_deref(), Some("a"));
        assert_eq!(m.row_name(1).as_deref(), Some("b"));
    }

    #[test]
    fn choreography_defaults_are_carried_not_dropped() {
        let m: Manifest = toml::from_str(
            r#"
            [[row]]
            name = "bounce"
            impact_cell = 3
            pools = ["verse", "chorus"]
        "#,
        )
        .unwrap();
        let r = &m.rows[0];
        assert_eq!(r.impact_cell, 3);
        assert_eq!(r.beats_per_loop, 1);
        assert!(r.loopable);
        assert_eq!(r.pools, ["verse", "chorus"]);
    }
}
