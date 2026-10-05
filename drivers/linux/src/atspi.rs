//! AT-SPI2 protocol data the observe backend decodes — decidable on
//! every host, so the contracts are testable without a bus.
//!
//! `org.a11y.atspi.Accessible.GetState` returns `au`: two `u32` words
//! holding an `AtspiStateType` bitfield (bit `n` of word `n / 32`).
//! `org.a11y.atspi.Component.GetExtents` returns `(iiii)` in the
//! requested coordinate space. Both come straight from
//! `atspi-constants.h`; the numbers are the protocol, not the toolkit.

use dexter_core::Rect;

/// Decoded `AtspiStateType` set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct States(u64);

impl States {
    pub const ACTIVE: u32 = 1;
    pub const CHECKED: u32 = 4;
    pub const DEFUNCT: u32 = 6;
    pub const EDITABLE: u32 = 7;
    pub const ENABLED: u32 = 8;
    pub const EXPANDABLE: u32 = 9;
    pub const FOCUSABLE: u32 = 11;
    pub const FOCUSED: u32 = 12;
    pub const ICONIFIED: u32 = 15;
    pub const MODAL: u32 = 16;
    pub const PRESSED: u32 = 20;
    pub const SELECTABLE: u32 = 22;
    pub const SELECTED: u32 = 23;
    pub const SENSITIVE: u32 = 24;
    pub const SHOWING: u32 = 25;
    pub const VISIBLE: u32 = 30;
    pub const INDETERMINATE: u32 = 32;
    pub const CHECKABLE: u32 = 41;
    pub const READ_ONLY: u32 = 43;

    /// Decode the `au` words `GetState` returns. Missing words are
    /// clear bits — a toolkit sending one word (or none) reports no
    /// state, never panics the walk.
    pub fn from_words(words: &[u32]) -> Self {
        let lo = words.first().copied().unwrap_or(0) as u64;
        let hi = words.get(1).copied().unwrap_or(0) as u64;
        Self(lo | (hi << 32))
    }

    pub fn has(self, state: u32) -> bool {
        state < 64 && (self.0 >> state) & 1 == 1
    }

    /// `enabled` for the normalized element: both `ENABLED` and
    /// `SENSITIVE` — GTK clears `SENSITIVE` on insensitive widgets and
    /// leaves `ENABLED` alone, so either missing means no input.
    pub fn enabled(self) -> Option<bool> {
        Some(self.has(Self::ENABLED) && self.has(Self::SENSITIVE))
    }

    /// Rendered somewhere right now: `SHOWING` (every ancestor shown)
    /// and `VISIBLE`. Widgets on hidden notebook pages or in unmapped
    /// dialogs are `VISIBLE` but not `SHOWING`.
    pub fn showing(self) -> bool {
        self.has(Self::SHOWING) && self.has(Self::VISIBLE)
    }

    /// A frame the user can see: showing and not iconified — the same
    /// rule as a visible, non-minimized HWND.
    pub fn on_screen(self) -> bool {
        self.showing() && !self.has(Self::ICONIFIED)
    }
}

/// `GetExtents(SCREEN)` → screen `Rect`. GTK reports unrealized
/// widgets at `(G_MININT, G_MININT, 1, 1)`; that sentinel and any
/// non-positive size are "no extent" — `None`, not a fake box.
pub fn extents_rect((x, y, w, h): (i32, i32, i32, i32)) -> Option<Rect> {
    if x == i32::MIN || y == i32::MIN || w <= 0 || h <= 0 {
        return None;
    }
    Some(Rect {
        x: x as f64,
        y: y as f64,
        w: w as f64,
        h: h as f64,
    })
}

/// `org.a11y.atspi.Action.GetName(i)` verb → the action vocabulary the
/// engine and the AX/DOM/UIA walkers share. ATK verbs are toolkit
/// strings (`click`, `press`, `activate`, `toggle`, `jump` for links,
/// `expand or contract` on tree rows, `menu` on menu buttons); anything
/// unrecognized is `None` — never guessed into `press`.
pub fn action_name(raw: &str) -> Option<&'static str> {
    let lower = raw.trim().to_ascii_lowercase();
    match lower.as_str() {
        "click" | "press" | "release" | "activate" | "toggle" | "jump" => Some("press"),
        "expand or contract" | "expand" | "collapse" => Some("expand_collapse"),
        "menu" | "show menu" | "showmenu" => Some("show_menu"),
        _ => None,
    }
}

/// Toggle state as a value for roles that *are* toggles (check boxes,
/// radio buttons, toggle buttons, switches, check/radio menu items):
/// `on` when `CHECKED` or `PRESSED`, `indeterminate`, else `off`. Any
/// other role → `None` — a plain button mid-press is not a toggle.
pub fn toggle_value(raw_role: &str, states: States) -> Option<String> {
    match raw_role {
        "check box" | "radio button" | "toggle button" | "switch" | "check menu item"
        | "radio menu item" => {}
        _ => return None,
    }
    let v = if states.has(States::INDETERMINATE) {
        "indeterminate"
    } else if states.has(States::CHECKED) || states.has(States::PRESSED) {
        "on"
    } else {
        "off"
    };
    Some(v.to_string())
}

/// `Window::id` for an AT-SPI top-level: FNV-1a over the owning
/// connection's unique bus name plus the object path. Both are stable
/// for the application's lifetime (and across our own processes), so
/// the id is reproducible; zero is reserved for "no window".
pub fn window_id(bus_name: &str, path: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in bus_name.bytes().chain([0u8]).chain(path.bytes()) {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    if h == 0 {
        1
    } else {
        h
    }
}

/// `AppSelector::Name` match: the AT-SPI application name (what the
/// toolkit registered — `gedit`, `gtk3-widget-factory`) or the
/// process `comm`, case-insensitively and whole. Nothing known about
/// the app → no match; an empty selector matches nothing.
pub fn app_name_matches(wanted: &str, app_name: Option<&str>, comm: Option<&str>) -> bool {
    if wanted.is_empty() {
        return false;
    }
    [app_name, comm]
        .into_iter()
        .flatten()
        .any(|n| n.eq_ignore_ascii_case(wanted))
}
