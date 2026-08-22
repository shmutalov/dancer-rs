//! Right-click context menu on the sprite itself.
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
//! # Why it is rebuilt on every click
//!
//! The menu shows state — which scale is ticked, whether Mirror is checked — and a
//! menu built once would have to be patched on every change from every path that
//! can change it (tray, hot reload, this menu itself). Building it fresh at the
//! moment it opens makes it correct by construction, and the cost is microseconds
//! on a gesture that happens a few times an hour.
//!
//! The popup blocks the winit thread while it is open (`TrackPopupMenu` runs its
//! own message loop), so the dancer freezes mid-pose until the menu closes. That
//! is how every context menu on Windows behaves, and it is fine here for the same
//! reason: it is open for a second, by the user's own hand.

use tray_icon::menu::{CheckMenuItem, ContextMenu as _, Menu, MenuId, MenuItem, PredefinedMenuItem};
use windows::Win32::Foundation::HWND;

/// What the user picked. A request, not a mutation — same contract as the tray.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Set the sprite scale to this factor.
    SetScale(f32),
    ToggleMirror,
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

pub struct Context {
    menu: Menu,
    scales: Vec<MenuId>,
    mirror: MenuId,
    quit: MenuId,
}

impl Context {
    /// Build the menu as of `scale` and `mirror` right now.
    pub fn new(scale: f32, mirror: bool) -> anyhow::Result<Self> {
        let menu = Menu::new();

        let on_ladder = SCALES.iter().any(|s| (s - scale).abs() < EPS);
        if !on_ladder {
            // A hand-edited value. Shown, ticked and disabled: the menu must not
            // pretend the current size is not the current size.
            menu.append(&CheckMenuItem::new(
                format!("Size: {:.0}% (from config.toml)", scale * 100.0),
                false,
                true,
                None,
            ))?;
        }

        let mut scales = Vec::with_capacity(SCALES.len());
        for &s in SCALES {
            let item = CheckMenuItem::new(
                format!("Size {:.0}%", s * 100.0),
                true,
                (s - scale).abs() < EPS,
                None,
            );
            scales.push(item.id().clone());
            menu.append(&item)?;
        }

        menu.append(&PredefinedMenuItem::separator())?;
        let mirror_item = CheckMenuItem::new("Mirror", true, mirror, None);
        let mirror = mirror_item.id().clone();
        menu.append(&mirror_item)?;

        menu.append(&PredefinedMenuItem::separator())?;
        let quit_item = MenuItem::new("Quit dancer-rs", true, None);
        let quit = quit_item.id().clone();
        menu.append(&quit_item)?;

        Ok(Self { menu, scales, mirror, quit })
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
        if let Some(i) = self.scales.iter().position(|s| s == id) {
            return Some(Action::SetScale(SCALES[i]));
        }
        if *id == self.mirror {
            return Some(Action::ToggleMirror);
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

    #[test]
    fn every_ladder_rung_maps_back_to_its_scale() {
        let ctx = Context::new(1.0, false).unwrap();
        for (i, id) in ctx.scales.iter().enumerate() {
            assert_eq!(ctx.action_for(id), Some(Action::SetScale(SCALES[i])));
        }
        assert_eq!(ctx.action_for(&ctx.mirror), Some(Action::ToggleMirror));
        assert_eq!(ctx.action_for(&ctx.quit), Some(Action::Quit));
    }

    #[test]
    fn an_unknown_id_is_nobodys_action() {
        // The tray and this menu share one event channel; claiming a foreign id
        // would hijack tray clicks.
        let ctx = Context::new(1.0, false).unwrap();
        assert_eq!(ctx.action_for(&MenuId::new("not-ours")), None);
    }

    #[test]
    fn a_config_scale_off_the_ladder_still_builds() {
        // 0.9 is not a rung; the menu grows a disabled row instead of lying.
        Context::new(0.9, true).unwrap();
    }
}
