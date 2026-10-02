//! `dexter-linux` — Linux driver skeleton.
//!
//! The seam, not the backend: this crate owns the Linux-side shape of
//! the `ComputerDriver` contract — capabilities that admit what it
//! can't do (everything, for now) and the AT-SPI role → normalized-role
//! tables the real AT-SPI2 backend will plug into. Every operation
//! returns `Unsupported` honestly rather than simulating anything.
//! Adding the backend later means filling in `windows`/`observe`/`act`
//! behind `#[cfg(target_os = "linux")]` and flipping capability flags.

use dexter_core::{Action, ActionResult, Observation, ObservationScope, Window};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};

fn unsupported() -> DriverError {
    DriverError::Unsupported("linux AT-SPI backend not implemented — skeleton only".into())
}

/// Linux driver skeleton. See crate docs — the type exists so engine
/// and app code can hold a driver named `linux` whose every claim is
/// honest.
#[derive(Debug, Default)]
pub struct LinuxDriver;

impl LinuxDriver {
    pub fn new() -> Self {
        Self
    }
}

impl ComputerDriver for LinuxDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "linux",
            element_tree: false,
            screenshots: false,
            background_input: false,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Err(unsupported())
    }

    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        let _ = scope;
        Err(unsupported())
    }

    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        let _ = (action, ctx);
        Err(unsupported())
    }
}

/// `AtspiRole` id → canonical AT-SPI role name.
///
/// Ids are the stable D-Bus protocol constants from `atspi-constants.h`
/// (`org.a11y.atspi.Accessible.GetRole` returns them as `u32`); names
/// are what `atspi_role_get_name` yields (the enum nick, `-` → space:
/// `ATSPI_ROLE_CHECK_BOX` → `"check box"`). `ATSPI_ROLE_LAST_DEFINED`
/// and anything past it are not roles → `None`.
pub fn atspi_role_name(id: u32) -> Option<&'static str> {
    match id {
        0 => Some("invalid"),
        1 => Some("accelerator label"),
        2 => Some("alert"),
        3 => Some("animation"),
        4 => Some("arrow"),
        5 => Some("calendar"),
        6 => Some("canvas"),
        7 => Some("check box"),
        8 => Some("check menu item"),
        9 => Some("color chooser"),
        10 => Some("column header"),
        11 => Some("combo box"),
        12 => Some("date editor"),
        13 => Some("desktop icon"),
        14 => Some("desktop frame"),
        15 => Some("dial"),
        16 => Some("dialog"),
        17 => Some("directory pane"),
        18 => Some("drawing area"),
        19 => Some("file chooser"),
        20 => Some("filler"),
        21 => Some("focus traversable"),
        22 => Some("font chooser"),
        23 => Some("frame"),
        24 => Some("glass pane"),
        25 => Some("html container"),
        26 => Some("icon"),
        27 => Some("image"),
        28 => Some("internal frame"),
        29 => Some("label"),
        30 => Some("layered pane"),
        31 => Some("list"),
        32 => Some("list item"),
        33 => Some("menu"),
        34 => Some("menu bar"),
        35 => Some("menu item"),
        36 => Some("option pane"),
        37 => Some("page tab"),
        38 => Some("page tab list"),
        39 => Some("panel"),
        40 => Some("password text"),
        41 => Some("popup menu"),
        42 => Some("progress bar"),
        43 => Some("button"),
        44 => Some("radio button"),
        45 => Some("radio menu item"),
        46 => Some("root pane"),
        47 => Some("row header"),
        48 => Some("scroll bar"),
        49 => Some("scroll pane"),
        50 => Some("separator"),
        51 => Some("slider"),
        52 => Some("spin button"),
        53 => Some("split pane"),
        54 => Some("status bar"),
        55 => Some("table"),
        56 => Some("table cell"),
        57 => Some("table column header"),
        58 => Some("table row header"),
        59 => Some("tearoff menu item"),
        60 => Some("terminal"),
        61 => Some("text"),
        62 => Some("toggle button"),
        63 => Some("tool bar"),
        64 => Some("tool tip"),
        65 => Some("tree"),
        66 => Some("tree table"),
        67 => Some("unknown"),
        68 => Some("viewport"),
        69 => Some("window"),
        70 => Some("extended"),
        71 => Some("header"),
        72 => Some("footer"),
        73 => Some("paragraph"),
        74 => Some("ruler"),
        75 => Some("application"),
        76 => Some("autocomplete"),
        77 => Some("editbar"),
        78 => Some("embedded"),
        79 => Some("entry"),
        80 => Some("chart"),
        81 => Some("caption"),
        82 => Some("document frame"),
        83 => Some("heading"),
        84 => Some("page"),
        85 => Some("section"),
        86 => Some("redundant object"),
        87 => Some("form"),
        88 => Some("link"),
        89 => Some("input method window"),
        90 => Some("table row"),
        91 => Some("tree item"),
        92 => Some("document spreadsheet"),
        93 => Some("document presentation"),
        94 => Some("document text"),
        95 => Some("document web"),
        96 => Some("document email"),
        97 => Some("comment"),
        98 => Some("list box"),
        99 => Some("grouping"),
        100 => Some("image map"),
        101 => Some("notification"),
        102 => Some("info bar"),
        103 => Some("level bar"),
        104 => Some("title bar"),
        105 => Some("block quote"),
        106 => Some("audio"),
        107 => Some("video"),
        108 => Some("definition"),
        109 => Some("article"),
        110 => Some("landmark"),
        111 => Some("log"),
        112 => Some("marquee"),
        113 => Some("math"),
        114 => Some("rating"),
        115 => Some("timer"),
        116 => Some("static"),
        117 => Some("math fraction"),
        118 => Some("math root"),
        119 => Some("subscript"),
        120 => Some("superscript"),
        121 => Some("description list"),
        122 => Some("description term"),
        123 => Some("description value"),
        124 => Some("footnote"),
        125 => Some("content deletion"),
        126 => Some("content insertion"),
        127 => Some("mark"),
        128 => Some("suggestion"),
        129 => Some("push button menu"),
        130 => Some("switch"),
        _ => None,
    }
}

/// AT-SPI role name → dexter normalized role.
///
/// Keys are canonical AT-SPI role names (see [`atspi_role_name`]) plus
/// `"push button"`, the ATK name toolkits still report through
/// `GetRoleName` for `ATSPI_ROLE_BUTTON`. Values are the same role
/// vocabulary the AX, DOM and UIA walkers emit, so `SemanticTarget`
/// matching stays platform-agnostic. Roles without a semantic
/// affordance (`invalid`, `unknown`, `canvas`, `separator`, …) return
/// `None` — an unmapped element keeps its `raw_role` instead of wearing
/// a guessed role.
pub fn atspi_role(name: &str) -> Option<&'static str> {
    match name {
        "button" | "push button" => Some("button"),
        "push button menu" => Some("menu_button"),
        // AX convention: toggles and switches are check boxes.
        "toggle button" | "check box" | "switch" => Some("check_box"),
        "radio button" => Some("radio_button"),
        "combo box" | "autocomplete" => Some("combo_box"),
        "entry" => Some("text_field"),
        "password text" => Some("secure_text_field"),
        "text" | "document text" => Some("text_area"),
        "label" | "static" | "caption" => Some("static_text"),
        "link" => Some("link"),
        "image" | "icon" | "image map" => Some("image"),
        "list" | "list box" => Some("list"),
        "list item" => Some("list_item"),
        "menu" | "popup menu" => Some("menu"),
        "menu bar" => Some("menu_bar"),
        "menu item" | "check menu item" | "radio menu item" | "tearoff menu item" => {
            Some("menu_item")
        }
        "page tab" => Some("tab"),
        "page tab list" => Some("tab_group"),
        "progress bar" => Some("progress_indicator"),
        "level bar" => Some("level_indicator"),
        "slider" => Some("slider"),
        "spin button" => Some("stepper"),
        "scroll bar" => Some("scroll_bar"),
        "scroll pane" => Some("scroll_area"),
        "split pane" => Some("split_group"),
        "table" => Some("table"),
        "table cell" => Some("cell"),
        "table row" => Some("row"),
        "column header" | "table column header" => Some("column_header"),
        "row header" | "table row header" => Some("row_header"),
        "tree" | "tree table" => Some("outline"),
        "tree item" => Some("row"),
        "tool bar" => Some("toolbar"),
        "tool tip" => Some("tooltip"),
        "status bar" => Some("status_bar"),
        "title bar" => Some("title_bar"),
        "calendar" => Some("calendar"),
        "dialog" | "alert" | "file chooser" | "color chooser" | "font chooser" => Some("dialog"),
        "frame" | "window" => Some("window"),
        "panel" | "filler" | "grouping" | "section" | "form" => Some("group"),
        "heading" => Some("heading"),
        "document web" => Some("web_area"),
        "application" => Some("application"),
        _ => None,
    }
}
