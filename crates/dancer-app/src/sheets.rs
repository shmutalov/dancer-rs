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
        // Credited by name, from the page's own credits panel. "Fan-made" is not
        // an owner: somebody drew these, and a warning about ownership that
        // cannot name the owner is not much of a warning.
        owner: "Steak_Bananite, who made the sheets, and Cygames, Inc., who made \
                Umamusume: Pretty Derby — with most of the Tanuki sprites supplied \
                by vonvan",
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

/// The "How to add dancers" text.
///
/// Built by pushing rather than as one long literal, and that is not a style
/// preference. A `\` line continuation in a Rust string keeps the *following*
/// line's indentation unless that line is flush left, so a paragraph laid out to
/// look tidy in source renders with a twenty-space gap in the middle of a sentence.
/// It compiles, it passes every other test, and the first sign of trouble is a
/// screenshot of the dialog. Pushing whole lines cannot do that, and living here
/// rather than in `main` means the guard test can read it.
pub fn help_text(artwork_dir: &Path) -> String {
    let mut t = String::new();
    t.push_str("A dancer is a sprite sheet: one PNG that is 8 cells wide, with one row ");
    t.push_str("per animation, plus a .txt naming those rows one per line.\n\n");
    t.push_str("The format comes from FAOSDance and Fruity Dance, so sheets made for ");
    t.push_str("either will work here.\n\n");
    t.push_str("To add one:\n\n");
    t.push_str("1. Put the .png and its .txt in:\n");
    t.push_str(&artwork_dir.display().to_string());
    t.push_str("\n\n2. Pick it from the tray, under Dancer.\n\n");
    t.push_str("For it to dance in time rather than just loop, add a .toml beside the ");
    t.push_str("PNG saying which cell is each move's accent. See default.toml in that ");
    t.push_str("folder for a worked example, and check your work with:\n\n");
    t.push_str("dancer-rs.exe <sheet.png> --check-sheet\n\n");
    t.push_str("----\n\n");
    t.push_str("Sprite sheets are other people's work. Neither dancer-rs nor you own ");
    t.push_str("the artwork on the pages linked in that menu — it belongs to whoever ");
    t.push_str("made it, published on their terms. Nothing is bundled or redistributed ");
    t.push_str("here, and please do not pass sheets on with copies of this app. The ");
    t.push_str("only artwork shipped with it is the plain default sheet.");
    t
}

/// Every sheet in `dir` — and one level of subfolders, sorted by name.
///
/// A sheet is a `.png`, and nothing further is checked here. The alternative —
/// requiring a `.txt` or `.toml` sidecar — would hide a sheet that has neither, and
/// the loader handles that case perfectly well by synthesising row names (spec
/// §4.1). Hiding a usable file is worse than listing one that turns out not to load.
///
/// One level deep because that is how people actually keep collections — a folder
/// per dancer, sidecars beside each PNG — and because unbounded recursion pointed
/// at a configurable path is how a menu ends up walking somebody's photo library.
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        tracing::debug!(dir = %dir.display(), "no artwork folder to list");
        return Vec::new();
    };

    let mut out: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let p = entry.path();
        if is_sheet(&p) {
            out.push(p);
        } else if p.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&p) {
                out.extend(sub.flatten().map(|e| e.path()).filter(|p| is_sheet(p)));
            }
        }
    }

    // Sorted so the menu order is stable. Directory order is not, and a menu whose
    // entries move between runs is one people misclick.
    out.sort_by_key(|p| label_in(dir, p).to_lowercase());
    out
}

fn is_sheet(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) && p.is_file()
}

/// What to call a sheet in the menu.
pub fn label(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// The menu name for a sheet found under `root`: `flchan/Dance_Large` rather than
/// a bare `Dance_Large`, so two same-named sheets in different folders stay
/// distinguishable.
pub fn label_in(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => {
            let mut l = rel.with_extension("").to_string_lossy().into_owned();
            // One separator, regardless of what the OS handed back: this is a
            // menu label, not a path.
            l = l.replace('\\', "/");
            l
        }
        Err(_) => label(path),
    }
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
    fn no_user_facing_string_has_collapsed_line_continuations() {
        // A `\` continuation in a Rust string keeps the *next* line's indentation
        // unless the following line is flush left. Get it wrong and the text still
        // compiles, still passes every other test, and renders in a dialog with a
        // twenty-space gap in the middle of a sentence. Only reading it catches it —
        // so this reads it.
        let help = help_text(Path::new("C:/app/assets"));
        let mut texts: Vec<String> = vec![help];
        for s in SOURCES {
            texts.push(s.name.to_string());
            texts.push(s.owner.to_string());
            texts.push(ownership_warning(s));
        }
        for text in &texts {
            for line in text.lines() {
                assert!(
                    !line.trim_start().contains("  "),
                    "run of spaces mid-line: {line:?}"
                );
            }
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
    fn sheets_one_folder_down_are_listed_with_their_folder_in_the_label() {
        // People keep collections as a folder per dancer. Before this, an artwork
        // directory organised that way produced an empty Dancer menu.
        let d = tmp("nested");
        std::fs::write(d.join("top.png"), b"x").unwrap();
        std::fs::create_dir_all(d.join("flchan")).unwrap();
        std::fs::write(d.join("flchan").join("Dance_Large.png"), b"x").unwrap();
        std::fs::create_dir_all(d.join("deep").join("deeper")).unwrap();
        std::fs::write(d.join("deep").join("deeper").join("far.png"), b"x").unwrap();

        let found: Vec<String> = list(&d).iter().map(|p| label_in(&d, p)).collect();
        // One level only: `far.png` stays out.
        assert_eq!(found, ["flchan/Dance_Large", "top"], "{found:?}");
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
