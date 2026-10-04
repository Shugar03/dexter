//! `dexter-windows` — Windows `ComputerDriver`.
//!
//! The UIA backend is real on `windows`: `windows()` lists top-level
//! windows (Win32 `EnumWindows`), `observe()` walks UI Automation
//! ControlView trees anchored on those HWNDs into normalized `Element`s,
//! and `act()` resolves targets through UIA patterns first — `SendInput`
//! physical input only behind `ctx.allow_coordinates`. Off Windows the
//! crate keeps the skeleton contract: no capability is claimed and
//! every operation declines honestly.

#[cfg(windows)]
mod actions;
#[cfg(windows)]
mod apps;
pub mod keymap;
pub mod resolve;
#[cfg(windows)]
mod uia;
#[cfg(windows)]
mod win;

use dexter_core::{Action, ActionResult, Observation, ObservationScope, Window};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
#[cfg(windows)]
use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(windows))]
fn unsupported() -> DriverError {
    DriverError::Unsupported("not implemented on dexter-windows yet".into())
}

/// Windows driver. See crate docs — on Windows the element tree and
/// pattern-driven actions are real (UIA needs no permission grant);
/// capture arrives in a later slice.
#[derive(Debug, Default)]
pub struct WindowsDriver {
    #[cfg(windows)]
    next_observation: AtomicU64,
    #[cfg(windows)]
    obs_cache: actions::ObsCache,
}

impl WindowsDriver {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ComputerDriver for WindowsDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "windows",
            // UIA is a read path with no grant — the flag is a fact on
            // Windows, an honest zero anywhere else.
            element_tree: cfg!(windows),
            screenshots: false,
            background_input: false,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        #[cfg(windows)]
        let result = win::list_windows();
        #[cfg(not(windows))]
        let result = Err(unsupported());
        result
    }

    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        #[cfg(windows)]
        let result = {
            let id =
                dexter_core::ObservationId(self.next_observation.fetch_add(1, Ordering::SeqCst));
            let obs = self::uia::observe(id, scope);
            // Only app-scoped observations bind element ids worth acting
            // on; cache plain data, never COM handles.
            if let Ok(o) = &obs {
                if let Some(pid) = o.pid {
                    self.obs_cache.store(
                        o.id,
                        pid,
                        scope.window,
                        scope.max_depth,
                        scope.max_elements,
                        o.elements.clone(),
                    );
                }
            }
            obs
        };
        #[cfg(not(windows))]
        let result = {
            let _ = scope;
            Err(unsupported())
        };
        result
    }

    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        #[cfg(windows)]
        let result = actions::act(action, ctx, &self.obs_cache);
        #[cfg(not(windows))]
        let result = {
            let _ = (action, ctx);
            Err(unsupported())
        };
        result
    }
}

/// UIA ControlType id → programmatic name.
///
/// Ids are the stable `UIA_<Name>ControlTypeId` protocol constants
/// (50000–50040). windows-rs reports the same values through
/// `CurrentControlType`; taking a bare `i32` keeps the table decidable
/// off Windows. Unknown ids return `None` — an element keeps its raw
/// control type rather than wearing an invented name.
pub fn control_type_name(id: i32) -> Option<&'static str> {
    match id {
        50000 => Some("Button"),
        50001 => Some("Calendar"),
        50002 => Some("CheckBox"),
        50003 => Some("ComboBox"),
        50004 => Some("Edit"),
        50005 => Some("Hyperlink"),
        50006 => Some("Image"),
        50007 => Some("ListItem"),
        50008 => Some("List"),
        50009 => Some("Menu"),
        50010 => Some("MenuBar"),
        50011 => Some("MenuItem"),
        50012 => Some("ProgressBar"),
        50013 => Some("RadioButton"),
        50014 => Some("ScrollBar"),
        50015 => Some("Slider"),
        50016 => Some("Spinner"),
        50017 => Some("StatusBar"),
        50018 => Some("Tab"),
        50019 => Some("TabItem"),
        50020 => Some("Text"),
        50021 => Some("ToolBar"),
        50022 => Some("ToolTip"),
        50023 => Some("Tree"),
        50024 => Some("TreeItem"),
        50025 => Some("Custom"),
        50026 => Some("Group"),
        50027 => Some("Thumb"),
        50028 => Some("DataGrid"),
        50029 => Some("DataItem"),
        50030 => Some("Document"),
        50031 => Some("SplitButton"),
        50032 => Some("Window"),
        50033 => Some("Pane"),
        50034 => Some("Header"),
        50035 => Some("HeaderItem"),
        50036 => Some("Table"),
        50037 => Some("TitleBar"),
        50038 => Some("Separator"),
        50039 => Some("SemanticZoom"),
        50040 => Some("AppBar"),
        _ => None,
    }
}

/// UIA ControlType name → dexter normalized role.
///
/// Keys are the `IUIAutomationElement::CurrentControlType`
/// programmatic names (`ControlType.Button` → `"Button"`). Values are
/// the same role vocabulary the AX and DOM walkers emit, so
/// `SemanticTarget` matching stays platform-agnostic. Unknown control
/// types return `None` — an unmapped element keeps its `raw_role`
/// instead of wearing a guessed role.
pub fn uia_role(control_type: &str) -> Option<&'static str> {
    match control_type {
        "Button" => Some("button"),
        "Calendar" => Some("calendar"),
        "CheckBox" => Some("check_box"),
        "ComboBox" => Some("combo_box"),
        "DataGrid" => Some("table"),
        "DataItem" => Some("cell"),
        "Document" => Some("text_area"),
        "Edit" => Some("text_field"),
        "Group" => Some("group"),
        "Header" => Some("row_header"),
        "HeaderItem" => Some("column_header"),
        "Hyperlink" => Some("link"),
        "Image" => Some("image"),
        "List" => Some("list"),
        "ListItem" => Some("list_item"),
        "Menu" => Some("menu"),
        "MenuBar" => Some("menu_bar"),
        "MenuItem" => Some("menu_item"),
        "Pane" => Some("group"),
        "ProgressBar" => Some("progress_indicator"),
        "RadioButton" => Some("radio_button"),
        "ScrollBar" => Some("scroll_bar"),
        "Slider" => Some("slider"),
        "Spinner" => Some("stepper"),
        "SplitButton" => Some("button"),
        "StatusBar" => Some("status_bar"),
        "Tab" => Some("tab_group"),
        "TabItem" => Some("tab"),
        "Table" => Some("table"),
        "Text" => Some("static_text"),
        "Thumb" => Some("slider_thumb"),
        "TitleBar" => Some("title_bar"),
        "ToolBar" => Some("toolbar"),
        "ToolTip" => Some("tooltip"),
        "Tree" => Some("outline"),
        "TreeItem" => Some("row"),
        "Window" => Some("window"),
        _ => None,
    }
}
