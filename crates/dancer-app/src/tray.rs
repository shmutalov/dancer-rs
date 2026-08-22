//! System tray icon and menu (spec §13, ROADMAP M5).
//!
//! Until now the only controls were mouse gestures on the sprite itself:
//! right-click for the sprite's own menu, middle-click for the A/B. That is
//! unusable for anyone who
//! has not read the source, and it breaks entirely with `click_through` on — the
//! window ignores the mouse, so there is no way to quit at all. The tray is the
//! first control surface that does not depend on hitting a sprite.
//!
//! # Why the icon is the dancer
//!
//! Built from the loaded sheet rather than shipped as an `.ico`. It costs about
//! thirty lines, avoids another file in the package, and it tells the user at a
//! glance *which sheet is loaded* — which starts to matter once artwork hot-reloads
//! and there is more than one sheet to choose between.

use tray_icon::menu::{CheckMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// What the user asked for.
///
/// Deliberately a request rather than a mutation: the tray owns no application
/// state, so nothing here can drift out of step with the config.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    ToggleClickThrough,
    ToggleAlwaysOnTop,
    ToggleAnticipation,
    /// Shift the output-latency offset by this many seconds (spec §9.2).
    NudgeOffset(f64),
    ResetOffset,
    OpenDataDir,
    /// Add a dancer wearing the sheet at this index of the list the tray was
    /// built with; `None` picks one at random from the artwork folder.
    AddDancer(Option<usize>),
    /// Close the most recently added dancer. Never the last one.
    RemoveLastDancer,
    OpenArtworkDir,
    /// Show the "where do dancers come from" help.
    SheetHelp,
    /// Open a known sheet source, after the ownership warning.
    OpenSheetSource(usize),
    /// Start the Yandex OAuth device flow.
    YandexSignIn,
    Quit,
}

/// Offset step per nudge, in seconds.
///
/// 5 ms is below what anyone can see in a single press, which is the point: the
/// error being corrected is a constant, and it is easier to find by walking past it
/// and back than by trying to land on it. The coarse step exists so that walking
/// there does not take forty clicks.
pub const NUDGE_FINE: f64 = 0.005;
pub const NUDGE_COARSE: f64 = 0.025;

/// Everything the menu displays, in one value.
///
/// Six positional booleans and floats passed twice — once to build, once to
/// refresh — is a swap waiting to happen, and `click_through` and `always_on_top`
/// are the same type in adjacent positions. It doubles as the change-detection key,
/// so the menu is only rewritten when something it shows actually differs.
#[derive(Debug, Clone, PartialEq)]
pub struct State {
    pub state: String,
    pub track: Option<String>,
    pub click_through: bool,
    pub always_on_top: bool,
    pub anticipate: bool,
    pub offset_secs: f64,
    pub yandex: String,
    /// How many dancers are on screen right now.
    pub dancers: usize,
}

pub struct Tray {
    // Held so the icon lives as long as the app: dropping it removes it from the
    // tray. Never read directly.
    _icon: TrayIcon,
    status: MenuItem,
    offset_label: MenuItem,
    click_through: CheckMenuItem,
    always_on_top: CheckMenuItem,
    anticipate: CheckMenuItem,
    /// Greyed out while there is one dancer: removing it is what Quit is for.
    remove_last: MenuItem,
    yandex: MenuItem,
    ids: Ids,
}

struct Ids {
    click_through: MenuId,
    always_on_top: MenuId,
    anticipate: MenuId,
    minus_coarse: MenuId,
    minus_fine: MenuId,
    plus_fine: MenuId,
    plus_coarse: MenuId,
    reset_offset: MenuId,
    open_dir: MenuId,
    /// One per entry of the sheet list, in the order it was given: "add a
    /// dancer wearing this".
    add: Vec<MenuId>,
    add_random: MenuId,
    remove_last: MenuId,
    open_artwork: MenuId,
    sheet_help: MenuId,
    sources: Vec<MenuId>,
    yandex_sign_in: MenuId,
    quit: MenuId,
}

impl Tray {
    /// Build the tray. `cell` is one sprite cell as premultiplied BGRA — the same
    /// buffer the renderer blits.
    pub fn new(
        cell: &[u32],
        width: u32,
        height: u32,
        now: &State,
        sheets: &[String],
        sources: &[&str],
    ) -> anyhow::Result<Self> {
        let menu = Menu::new();

        // Disabled items used as labels. `muda` has no dedicated label widget, and
        // a disabled item is how every other tray application does this.
        let status = MenuItem::new(&now.state, false, None);
        let offset_label = MenuItem::new(offset_text(now.offset_secs), false, None);

        let click_through_item = CheckMenuItem::new("Click-through", true, now.click_through, None);
        let always_on_top_item = CheckMenuItem::new("Always on top", true, now.always_on_top, None);
        let anticipate_item = CheckMenuItem::new("Anticipate the beat", true, now.anticipate, None);

        let minus_coarse = MenuItem::new("Offset  -25 ms", true, None);
        let minus_fine = MenuItem::new("Offset   -5 ms", true, None);
        let plus_fine = MenuItem::new("Offset   +5 ms", true, None);
        let plus_coarse = MenuItem::new("Offset  +25 ms", true, None);
        let reset_offset = MenuItem::new("Reset offset", true, None);

        let open_dir = MenuItem::new("Open data folder", true, None);
        let quit = MenuItem::new("Quit", true, None);

        // Which sheet a *new* dancer wears. Changing an existing dancer's sheet
        // is that dancer's own business — its right-click menu — so nothing here
        // is a radio list: these are verbs, one per sheet.
        let add = Submenu::new("Add dancer", true);
        let mut add_items = Vec::with_capacity(sheets.len());
        for name in sheets {
            let item = MenuItem::new(name, true, None);
            add.append(&item)?;
            add_items.push(item);
        }
        if sheets.is_empty() {
            // Never an empty submenu: an empty one looks broken, whereas a line
            // saying what is missing points at the fix.
            add.append(&MenuItem::new("No sheets in the artwork folder", false, None))?;
        }
        let add_random = MenuItem::new("Random", !sheets.is_empty(), None);
        add.append_items(&[&PredefinedMenuItem::separator(), &add_random])?;

        let remove_last = MenuItem::new("Remove last dancer", now.dancers > 1, None);

        let dancers = Submenu::new("Dancers", true);
        let open_artwork = MenuItem::new("Open artwork folder", true, None);
        let sheet_help = MenuItem::new("How to add dancers...", true, None);
        dancers.append_items(&[
            &add,
            &remove_last,
            &PredefinedMenuItem::separator(),
            &open_artwork,
            &sheet_help,
        ])?;

        // Links out, never downloads. Each one warns whose artwork it is before the
        // browser opens — a menu the app drew reads as an endorsement otherwise.
        let mut source_items = Vec::with_capacity(sources.len());
        for name in sources {
            let item = MenuItem::new(format!("Get {name}..."), true, None);
            dancers.append(&item)?;
            source_items.push(item);
        }

        let yandex_item = MenuItem::new(&now.yandex, false, None);
        let yandex_sign_in = MenuItem::new("Sign in to Yandex Music...", true, None);

        let ids = Ids {
            click_through: click_through_item.id().clone(),
            always_on_top: always_on_top_item.id().clone(),
            anticipate: anticipate_item.id().clone(),
            minus_coarse: minus_coarse.id().clone(),
            minus_fine: minus_fine.id().clone(),
            plus_fine: plus_fine.id().clone(),
            plus_coarse: plus_coarse.id().clone(),
            reset_offset: reset_offset.id().clone(),
            open_dir: open_dir.id().clone(),
            add: add_items.iter().map(|i| i.id().clone()).collect(),
            add_random: add_random.id().clone(),
            remove_last: remove_last.id().clone(),
            open_artwork: open_artwork.id().clone(),
            sheet_help: sheet_help.id().clone(),
            sources: source_items.iter().map(|i| i.id().clone()).collect(),
            yandex_sign_in: yandex_sign_in.id().clone(),
            quit: quit.id().clone(),
        };

        menu.append_items(&[
            &status,
            &PredefinedMenuItem::separator(),
            &dancers,
            &PredefinedMenuItem::separator(),
            &click_through_item,
            &always_on_top_item,
            &anticipate_item,
            &PredefinedMenuItem::separator(),
            &offset_label,
            &minus_coarse,
            &minus_fine,
            &plus_fine,
            &plus_coarse,
            &reset_offset,
            &PredefinedMenuItem::separator(),
            &yandex_item,
            &yandex_sign_in,
            &PredefinedMenuItem::separator(),
            &open_dir,
            &quit,
        ])?;

        let _icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("dancer-rs")
            .with_icon(icon_from_cell(cell, width, height))
            .build()?;

        Ok(Self {
            _icon,
            status,
            offset_label,
            click_through: click_through_item,
            always_on_top: always_on_top_item,
            anticipate: anticipate_item,
            remove_last,
            yandex: yandex_item,
            ids,
        })
    }

    /// Drain pending menu clicks. Call once per event-loop pass.
    /// The tray action this menu id means, if it is one of ours.
    ///
    /// The `MenuEvent` channel is global to the process and shared with the
    /// sprite's context menu, so nobody drains it privately any more — `main`
    /// drains it once and asks each menu in turn. Claiming an id that is not ours
    /// would hijack the other menu's click.
    pub fn action_for(&self, id: &MenuId) -> Option<Action> {
        Some(if *id == self.ids.click_through {
            Action::ToggleClickThrough
        } else if *id == self.ids.always_on_top {
            Action::ToggleAlwaysOnTop
        } else if *id == self.ids.anticipate {
            Action::ToggleAnticipation
        } else if *id == self.ids.minus_coarse {
            Action::NudgeOffset(-NUDGE_COARSE)
        } else if *id == self.ids.minus_fine {
            Action::NudgeOffset(-NUDGE_FINE)
        } else if *id == self.ids.plus_fine {
            Action::NudgeOffset(NUDGE_FINE)
        } else if *id == self.ids.plus_coarse {
            Action::NudgeOffset(NUDGE_COARSE)
        } else if *id == self.ids.reset_offset {
            Action::ResetOffset
        } else if *id == self.ids.open_dir {
            Action::OpenDataDir
        } else if *id == self.ids.open_artwork {
            Action::OpenArtworkDir
        } else if *id == self.ids.sheet_help {
            Action::SheetHelp
        } else if *id == self.ids.yandex_sign_in {
            Action::YandexSignIn
        } else if let Some(i) = self.ids.add.iter().position(|s| s == id) {
            Action::AddDancer(Some(i))
        } else if *id == self.ids.add_random {
            Action::AddDancer(None)
        } else if *id == self.ids.remove_last {
            Action::RemoveLastDancer
        } else if let Some(i) = self.ids.sources.iter().position(|s| s == id) {
            Action::OpenSheetSource(i)
        } else if *id == self.ids.quit {
            Action::Quit
        } else {
            return None;
        })
    }

    /// Push current state into the menu.
    ///
    /// The checkboxes are set from the application rather than toggled by the menu
    /// itself. `click_through` can fail to apply, and a checkbox reporting what was
    /// *asked for* rather than what happened is worse than no checkbox.
    pub fn refresh(&self, now: &State) {
        self.yandex.set_text(&now.yandex);
        self.status.set_text(match &now.track {
            Some(t) => format!("{} — {t}", now.state),
            None => now.state.clone(),
        });
        self.offset_label.set_text(offset_text(now.offset_secs));
        self.click_through.set_checked(now.click_through);
        self.always_on_top.set_checked(now.always_on_top);
        self.anticipate.set_checked(now.anticipate);
        self.remove_last.set_enabled(now.dancers > 1);
    }
}

fn offset_text(secs: f64) -> String {
    format!("Output offset: {:+.0} ms", secs * 1000.0)
}

/// Turn one premultiplied-BGRA sprite cell into a tray icon.
///
/// Two conversions, both required. The renderer keeps cells premultiplied because
/// `UpdateLayeredWindow` demands it (spec §4.1), while `tray_icon::Icon` wants
/// straight RGBA — so alpha has to be divided back out. And the sprite is cropped
/// to its opaque bounds first: sheet cells are mostly empty space, and scaling a
/// whole cell to 32 px leaves the figure a few pixels tall in the corner.
fn icon_from_cell(cell: &[u32], width: u32, height: u32) -> Icon {
    const SIZE: u32 = 32;

    let (x0, y0, side) = opaque_bounds(cell, width, height).unwrap_or((0, 0, width.min(height)));
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);

    for y in 0..SIZE {
        for x in 0..SIZE {
            // Nearest neighbour. The source is flat-shaded sprite art a few hundred
            // pixels across and the target is 32 px; filtering would buy nothing but
            // a dependency.
            let sx = x0 + x * side / SIZE;
            let sy = y0 + y * side / SIZE;
            let px = cell.get((sy * width + sx) as usize).copied().unwrap_or(0);
            let a = (px >> 24) as u8;
            let straight = |c: u32| -> u8 {
                if a == 0 {
                    0
                } else {
                    ((c & 0xff) * 255 / a as u32).min(255) as u8
                }
            };
            rgba.push(straight(px >> 16));
            rgba.push(straight(px >> 8));
            rgba.push(straight(px));
            rgba.push(a);
        }
    }

    // Infallible by construction — the buffer is exactly SIZE*SIZE*4 — but a wrong
    // icon must not stop the app starting, so fall back to a blank one.
    Icon::from_rgba(rgba, SIZE, SIZE).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not build a tray icon from the sheet");
        Icon::from_rgba(vec![0; (SIZE * SIZE * 4) as usize], SIZE, SIZE)
            .expect("a blank icon is always valid")
    })
}

/// Square crop around everything not fully transparent, as `(x, y, side)`.
///
/// Square because the icon is square: cropping to the figure's own aspect ratio and
/// then squashing it into 32x32 would stretch a standing sprite into a blob.
fn opaque_bounds(cell: &[u32], width: u32, height: u32) -> Option<(u32, u32, u32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0u32, 0u32);
    for y in 0..height {
        for x in 0..width {
            if cell.get((y * width + x) as usize).is_some_and(|p| (p >> 24) > 8) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }
    if x1 < x0 || y1 < y0 {
        return None;
    }

    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    let side = w.max(h).min(width).min(height);
    // Centre the square on the figure, then push it back inside the cell.
    let cx = x0 + w / 2;
    let cy = y0 + h / 2;
    let nx = cx.saturating_sub(side / 2).min(width - side);
    let ny = cy.saturating_sub(side / 2).min(height - side);
    Some((nx, ny, side))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(w: u32, h: u32, opaque: &[(u32, u32)]) -> Vec<u32> {
        let mut c = vec![0u32; (w * h) as usize];
        for (x, y) in opaque {
            c[(y * w + x) as usize] = 0xFF_80_80_80;
        }
        c
    }

    #[test]
    fn bounds_find_the_figure_not_the_cell() {
        // Sheet cells are mostly empty. Scaling a whole cell would leave the sprite
        // a few pixels tall in the corner of the icon.
        let c = cell(100, 100, &[(40, 40), (44, 48)]);
        let (x, y, side) = opaque_bounds(&c, 100, 100).unwrap();
        assert!(side < 100, "crop should be tighter than the cell: {side}");
        assert!(x <= 40 && y <= 40, "crop starts after the figure: {x},{y}");
        assert!(x + side > 44 && y + side > 48, "crop ends before the figure");
    }

    #[test]
    fn the_crop_is_square_so_the_icon_is_not_stretched() {
        // A tall thin figure squashed into a square icon would be unrecognisable.
        let c = cell(64, 64, &[(30, 10), (32, 50)]);
        let (x, y, side) = opaque_bounds(&c, 64, 64).unwrap();
        assert!(side >= 41, "should span the figure's height: {side}");
        assert!(x + side <= 64 && y + side <= 64, "crop must stay inside the cell");
    }

    #[test]
    fn the_crop_never_leaves_the_cell() {
        // A figure hard against an edge is the case that overflows a naive centring.
        for spot in [(0, 0), (63, 63), (0, 63), (63, 0)] {
            let c = cell(64, 64, &[spot]);
            let (x, y, side) = opaque_bounds(&c, 64, 64).unwrap();
            assert!(x + side <= 64 && y + side <= 64, "{spot:?} gave {x},{y}+{side}");
        }
    }

    #[test]
    fn a_fully_transparent_cell_has_no_bounds() {
        assert!(opaque_bounds(&cell(16, 16, &[]), 16, 16).is_none());
    }

    #[test]
    fn an_icon_is_produced_even_from_an_empty_cell() {
        // Straight into the fallback path: an unrecognisable icon beats no tray.
        let _ = icon_from_cell(&cell(16, 16, &[]), 16, 16);
    }

    #[test]
    fn offset_is_shown_with_its_sign() {
        // The value is a correction, and which way it is going matters more than
        // its magnitude when nudging by eye.
        assert_eq!(offset_text(0.180), "Output offset: +180 ms");
        assert_eq!(offset_text(-0.020), "Output offset: -20 ms");
        assert_eq!(offset_text(0.0), "Output offset: +0 ms");
    }
}
