//! Finding the dancers in the artwork folder (ROADMAP M5).
//!
//! Choosing a sheet used to mean editing `config.toml` and restarting, which is a
//! strange thing to ask for the most visible setting the app has. The tray lists
//! whatever is in the folder; this is what it lists.

use std::path::{Path, PathBuf};

/// Every sheet in `dir`, sorted by name.
///
/// A sheet is a `.png`, and nothing further is checked here. The alternative —
/// requiring a `.txt` or `.toml` sidecar — would hide a sheet that has neither, and
/// the loader handles that case perfectly well by synthesising row names (spec
/// §4.1). Hiding a usable file is worse than listing one that turns out not to load.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!(dir = %dir.display(), "no artwork folder to list");
        return Vec::new();
    };

    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("png"))
                && p.is_file()
        })
        .collect();

    // Sorted so the menu order is stable. Directory order is not, and a menu whose
    // entries move between runs is one people misclick.
    out.sort_by_key(|p| label(p).to_lowercase());
    out
}

/// What to call a sheet in the menu.
pub fn label(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dancer-sheets-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn only_png_files_are_offered() {
        let d = tmp("kinds");
        for f in ["a.png", "b.PNG", "notes.txt", "c.toml", "d.jpg"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        std::fs::create_dir_all(d.join("sub.png")).unwrap();

        let found: Vec<String> = list(&d).iter().map(|p| label(p)).collect();
        assert_eq!(found, ["a", "b"], "{found:?}");
    }

    #[test]
    fn a_sheet_without_sidecars_is_still_listed() {
        // The loader synthesises row names for these, so hiding them would conceal
        // a file that works.
        let d = tmp("bare");
        std::fs::write(d.join("lonely.png"), b"x").unwrap();
        assert_eq!(list(&d).len(), 1);
    }

    #[test]
    fn the_order_is_stable_and_case_insensitive() {
        // Directory order is not stable, and a menu whose entries move between runs
        // is one people misclick.
        let d = tmp("order");
        for f in ["Zebra.png", "apple.png", "Mango.png"] {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        let found: Vec<String> = list(&d).iter().map(|p| label(p)).collect();
        assert_eq!(found, ["apple", "Mango", "Zebra"], "{found:?}");
    }

    #[test]
    fn a_missing_folder_is_empty_rather_than_an_error() {
        // `artwork_dir` is configurable and can point anywhere, including nowhere.
        assert!(list(Path::new("Z:/no/such/folder")).is_empty());
    }
}
