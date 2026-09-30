//! `dexter-windows` — Windows driver skeleton.
//!
//! The seam, not the backend: this crate owns the Windows-side shape
//! of the `ComputerDriver` contract — capabilities that admit what it
//! can't do (everything, for now) and the UIA control-type →
//! normalized-role table the real backend will plug into. Every
//! operation returns `Unsupported` honestly rather than simulating
//! anything. Adding the UI Automation backend later means filling in
//! `observe`/`act` behind `#[cfg(target_os = "windows")]` and
//! flipping capability flags — the crate shape and role mapping are
//! already load-bearing.

use dexter_core::{Action, ActionResult, Observation, ObservationScope, Window};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};

fn unsupported() -> DriverError {
    DriverError::Unsupported("windows UIA backend not implemented — skeleton only".into())
}

/// Windows driver skeleton. See crate docs — the type exists so the
/// engine/app code can hold a driver named `windows` whose every
/// claim is honest.
#[derive(Debug, Default)]
pub struct WindowsDriver;

impl WindowsDriver {
    pub fn new() -> Self {
        Self
    }
}

impl ComputerDriver for WindowsDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "windows",
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
