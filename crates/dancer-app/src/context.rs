//! Right-click context menu on a dancer: everything that is about *this* dancer.
//!
//! # Why right-click stopped meaning quit
//!
//! Quit-on-right-click dates from M0, when the sprite was the only surface there
//! was and quitting was the only thing to control. It was always a misclick from
//! being a data-loss gesture — one slip while reaching for a drag and the app is
//! gone. Now that settings exist, the same button opens a menu, and Quit lives
//! *inside* it: still reachable from the sprite alone (the tray-less fallback the
//! old gesture existed for), but behind one deliberate step instead of zero.
//!
//! # What belongs here and what belongs in the tray
//!
//! The split is by subject. This menu is about one dancer — its sheet, its size,
//! whether to clone or remove it — so it opens *on* that dancer, and there is no
//! "which of the five identical entries is mine" to work out. The tray is about
//! the process: add a dancer, the offset, click-through, the account. Sheet
//! choice used to be in the tray, which made sense with one dancer and none
//! with several.
//!
//! # Why it is rebuilt on every click
//!
//! The menu shows state — which sheet and scale are ticked, whether Mirror is
//! checked, whether Remove is allowed — and a menu built once would have to be
//! patched on every change from every path that can change it. Building it fresh
//! at the moment it opens makes it correct by construction, and the cost is
//! microseconds on a gesture that happens a few times an hour.
//!
//! The popup blocks the winit thread while it is open (`TrackPopupMenu` runs its
//! own message loop), so the dancers freeze mid-pose until the menu closes. That
//! is how every context menu on Windows behaves, and it is fine here for the same
//! reason: it is open for a second, by the user's own hand.

use tray_icon::menu::{
    CheckMenuItem, ContextMenu as _, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use windows::Win32::Foundation::HWND;

/// What the user picked. A request, not a mutation — same contract as the tray.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Wear the sheet at this index of the list the menu was built with.
    SelectSheet(usize),
    /// Set this dancer's scale to this factor.
    SetScale(f32),
    ToggleMirror,
    /// Add another dancer wearing the same sheet at the same size.
    Duplicate,
    /// Close this dancer. Never offered for the last one.
    Remove,
    Quit,
}

/// The scales on offer.
///
/// A fixed ladder rather than a slider because muda has no slider, and because
/// nearest-neighbour scaling looks best at simple ratios anyway. Anything else
/// remains reachable through `config.toml`, and a value set there shows up as its
/// own ticked, disabled row so the menu never lies about the current state.
pub const SCALES: &[f32] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0];

/// How close two scales must be to count as the same rung.
const EPS: f32 = 0.005;

/// What the menu needs to know about the dancer it opens on.
pub struct View<'a> {
    pub scale: f32,
    pub mirror: bool,
    /// Sheet names as the menu should show them, and which one this dancer wears.
    pub sheets: &'a [String],
    pub current_sheet: Option<usize>,
    /// False for the last dancer: removing it would leave nothing on screen, and
    /// "Quit" already exists for that.
    pub removable: bool,
}

pub struct Context {
    menu: Menu,
    sheets: Vec<MenuId>,
    scales: Vec<MenuId>,
    mirror: MenuId,
    duplicate: MenuId,
    remove: MenuId,
    quit: MenuId,
}

impl Context {
    /// Build the menu as of `view` right now.
    pub fn new(view: &View) -> anyhow::Result<Self> {
        let menu = Menu::new();

        // Radio behaviour by hand: `muda` has no radio item, and a check item per
        // sheet with exactly one ticked reads the same way.
        let sheet_menu = Submenu::new("Sheet", true);
        let mut sheets = Vec::with_capacity(view.sheets.len());
        for (i, name) in view.sheets.iter().enumerate() {
            let item = CheckMenuItem::new(name, true, Some(i) == view.current_sheet, None);
            sheets.push(item.id().clone());
            sheet_menu.append(&item)?;
        }
        if view.sheets.is_empty() {
            // Never an empty submenu: an empty one looks broken, whereas a line
            // saying what is missing points at the fix.
            sheet_menu.append(&MenuItem::new("No sheets in the artwork folder", false, None))?;
        }
        menu.append(&sheet_menu)?;

        let size_menu = Submenu::new("Size", true);
        let on_ladder = SCALES.iter().any(|s| (s - view.scale).abs() < EPS);
        if !on_ladder {
            // A hand-edited or jittered value. Shown, ticked and disabled: the
            // menu must not pretend the current size is not the current size.
            size_menu.append(&CheckMenuItem::new(
                format!("{:.0}% (current)", view.scale * 100.0),
                false,
                true,
                None,
            ))?;
        }
        let mut scales = Vec::with_capacity(SCALES.len());
        for &s in SCALES {
            let item = CheckMenuItem::new(
                format!("{:.0}%", s * 100.0),
                true,
                (s - view.scale).abs() < EPS,
                None,
            );
            scales.push(item.id().clone());
            size_menu.append(&item)?;
        }
        menu.append(&size_menu)?;

        let mirror_item = CheckMenuItem::new("Mirror", true, view.mirror, None);
        let mirror = mirror_item.id().clone();
        menu.append(&mirror_item)?;

        menu.append(&PredefinedMenuItem::separator())?;
        let duplicate_item = MenuItem::new("Duplicate", true, None);
        let duplicate = duplicate_item.id().clone();
        menu.append(&duplicate_item)?;
        let remove_item = MenuItem::new("Remove this dancer", view.removable, None);
        let remove = remove_item.id().clone();
        menu.append(&remove_item)?;

        menu.append(&PredefinedMenuItem::separator())?;
        let quit_item = MenuItem::new("Quit dancer-rs", true, None);
        let quit = quit_item.id().clone();
        menu.append(&quit_item)?;

        Ok(Self {
            menu,
            sheets,
            scales,
            mirror,
            duplicate,
            remove,
            quit,
        })
    }

    /// Pop the menu at the cursor and block until it closes.
    ///
    /// The selection, if any, arrives through `MenuEvent::receiver()` — the same
    /// channel the tray uses — and is matched by [`Context::action_for`] on the
    /// next drain.
    pub fn show(&self, hwnd: HWND) {
        // SAFETY: the hwnd is the live sprite window; `None` positions at the cursor.
        unsafe {
            self.menu.show_context_menu_for_hwnd(hwnd.0 as isize, None);
        }
    }

    pub fn action_for(&self, id: &MenuId) -> Option<Action> {
        if let Some(i) = self.sheets.iter().position(|s| s == id) {
            return Some(Action::SelectSheet(i));
        }
        if let Some(i) = self.scales.iter().position(|s| s == id) {
            return Some(Action::SetScale(SCALES[i]));
        }
        if *id == self.mirror {
            return Some(Action::ToggleMirror);
        }
        if *id == self.duplicate {
            return Some(Action::Duplicate);
        }
        if *id == self.remove {
            return Some(Action::Remove);
        }
        if *id == self.quit {
            return Some(Action::Quit);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(sheets: &[String]) -> View<'_> {
        View {
            scale: 1.0,
            mirror: false,
            sheets,
            current_sheet: Some(0),
            removable: true,
        }
    }

    #[test]
    fn every_item_maps_back_to_its_action() {
        let names = vec!["a".to_string(), "b".to_string()];
        let ctx = Context::new(&view(&names)).unwrap();
        for (i, id) in ctx.sheets.iter().enumerate() {
            assert_eq!(ctx.action_for(id), Some(Action::SelectSheet(i)));
        }
        for (i, id) in ctx.scales.iter().enumerate() {
            assert_eq!(ctx.action_for(id), Some(Action::SetScale(SCALES[i])));
        }
        assert_eq!(ctx.action_for(&ctx.mirror), Some(Action::ToggleMirror));
        assert_eq!(ctx.action_for(&ctx.duplicate), Some(Action::Duplicate));
        assert_eq!(ctx.action_for(&ctx.remove), Some(Action::Remove));
        assert_eq!(ctx.action_for(&ctx.quit), Some(Action::Quit));
    }

    #[test]
    fn an_unknown_id_is_nobodys_action() {
        // The tray and this menu share one event channel; claiming a foreign id
        // would hijack tray clicks.
        let ctx = Context::new(&view(&[])).unwrap();
        assert_eq!(ctx.action_for(&MenuId::new("not-ours")), None);
    }

    #[test]
    fn a_scale_off_the_ladder_and_an_empty_sheet_list_still_build() {
        // 0.9 is not a rung and an empty folder is not an error; the menu grows a
        // disabled row for each instead of lying or breaking.
        let v = View {
            scale: 0.9,
            mirror: true,
            sheets: &[],
            current_sheet: None,
            removable: false,
        };
        Context::new(&v).unwrap();
    }
}
