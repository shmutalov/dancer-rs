//! Finding the dancers in the artwork folder (ROADMAP M5).
//!
//! Choosing a sheet used to mean editing `config.toml` and restarting, which is a
//! strange thing to ask for the most visible setting the app has. The tray lists
//! whatever is in the folder; this is what it lists.

use std::path::{Path, PathBuf};

/// A place sheets can be obtained from.
///
/// Links only. **Nothing here is bundled, hosted or mirrored** — every one of these
/// is somebody else's artwork, published by them under their own terms, and the
/// user downloads it from them (spec §1.3).
pub struct Source {
    pub name: &'static str,
    pub url: &'static str,
    /// Who the artwork belongs to, named in the warning before the link opens.
    pub owner: &'static str,
}

/// Known sources, in the order the menu lists them.
pub const SOURCES: &[Source] = &[
    Source {
        name: "FL-Chan (Image-Line)",
        url: "https://www.image-line.com/fl-studio-learning/fl-studio-online-manual/content/FLChan_HD.zip",
        owner: "Image-Line Software",
    },
    Source {
        name: "Umamusume (GameBanana)",
        url: "https://gamebanana.com/tools/21924",
        owner: "the people who made it; the characters belong to Cygames",
    },
];

/// The warning shown before any of those links is opened.
///
/// # Why this is a confirmation and not a footnote
///
/// The app is about to send someone to download artwork it has no rights to, from
/// a menu it drew, which reads like an endorsement — as though picking a dancer
/// from a list makes the artwork part of the product. It is not. Saying so once,
/// in front of the link, is the difference between offering a pointer and implying
/// a licence.
pub fn ownership_warning(source: &Source) -> String {
    let mut w = String::new();
    w.push_str(source.name);
    w.push_str(" is not ours, and downloading it does not make it yours.\n\n");
    w.push_str("The artwork belongs to ");
    w.push_str(source.owner);
    w.push_str(". It is published on their terms — read them.\n\n");
    w.push_str(
        "dancer-rs does not host, bundle or redistribute any sprite sheet, and neither \
         should you: please do not pass sheets on with copies of this app. The only \
         artwork shipped with it is the plain default sheet.\n\n",
    );
    w.push_str("Open the download page in your browser?");
    w
}

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

    #[test]
    fn every_source_is_a_web_link_and_names_an_owner() {
        // `dialog::open_url` refuses anything else, so a non-http entry here would
        // be a menu item that silently does nothing.
        assert!(!SOURCES.is_empty());
        for s in SOURCES {
            assert!(s.url.starts_with("https://"), "{}: {}", s.name, s.url);
            assert!(!s.owner.trim().is_empty(), "{} names no owner", s.name);
            assert!(!s.name.trim().is_empty());
        }
    }

    #[test]
    fn the_warning_names_the_owner_and_refuses_to_imply_a_licence() {
        for s in SOURCES {
            let w = ownership_warning(s);
            assert!(w.contains(s.owner), "{} does not name its owner", s.name);
            assert!(w.contains("not ours"), "{}", s.name);
            assert!(w.contains("redistribute"), "{}", s.name);
        }
    }

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
