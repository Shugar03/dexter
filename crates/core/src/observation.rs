use crate::element::{Element, ElementId};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Unique id of an observation (monotonic per process).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObservationId(pub u64);

/// Selects which application a scoped observation targets.
/// Serialized adjacently tagged (`{"by":"name","value":"TextEdit"}`) so a
/// bundle id can never be confused with a display name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "by", content = "value", rename_all = "snake_case")]
pub enum AppSelector {
    Pid(i32),
    /// Bundle identifier, e.g. `com.apple.TextEdit`.
    BundleId(String),
    /// Process/window-owner name, e.g. `TextEdit`.
    Name(String),
}

impl AppSelector {
    /// Parse a CLI `--app` value: digits -> pid, contains `.` -> bundle id,
    /// otherwise a name.
    pub fn parse(s: &str) -> Self {
        if let Ok(pid) = s.parse::<i32>() {
            Self::Pid(pid)
        } else if s.contains('.') {
            Self::BundleId(s.to_string())
        } else {
            Self::Name(s.to_string())
        }
    }
}

/// What an [`crate::Observation`] should capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservationScope {
    /// Restrict to one application. `None` = whole screen, windows only
    /// (accessibility trees are only walked for a scoped app).
    pub app: Option<AppSelector>,
    /// Restrict the element walk to one window subtree (a `Window::id`
    /// from a previous observation). Drivers SHOULD honor this natively;
    /// callers post-filter with `world_model::within_window` when the
    /// driver did not (detected by `windows` not already narrowed).
    #[serde(default)]
    pub window: Option<u32>,
    /// Max accessibility tree depth to walk.
    pub max_depth: u32,
    /// Cap on flattened elements.
    pub max_elements: usize,
    /// Whether to capture a screenshot alongside the structured data.
    pub screenshot: bool,
    /// Opt-in OCR fallback: when the AX tree is limited/empty or the
    /// observation is window-scoped, capture the target window and append
    /// recognized text as inert `source: ocr` elements. Costs a screen
    /// capture plus an on-device recognition pass — never implicit.
    #[serde(default)]
    pub vision: bool,
    /// Optional output path for the screenshot (driver picks a temp file
    /// when absent).
    #[serde(default)]
    pub screenshot_path: Option<String>,
    /// Whether to walk the app's menu-bar subtree. Menu items often
    /// outnumber window elements ~10:1 and each costs an IPC roundtrip;
    /// callers that only need window controls can opt out. Menu chords
    /// (`Action::Key` semantic routing) resolve the menu live and are
    /// unaffected.
    #[serde(default = "default_true")]
    pub include_menu: bool,
}

fn default_true() -> bool {
    true
}

impl Default for ObservationScope {
    fn default() -> Self {
        Self {
            app: None,
            window: None,
            max_depth: 40,
            max_elements: 4_000,
            screenshot: false,
            vision: false,
            screenshot_path: None,
            include_menu: true,
        }
    }
}

impl ObservationScope {
    pub fn for_app(sel: AppSelector) -> Self {
        Self {
            app: Some(sel),
            ..Self::default()
        }
    }
}

/// A window on screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    /// Platform window id (CGWindowID on macOS).
    pub id: u32,
    pub pid: i32,
    /// Owning application name (`kCGWindowOwnerName`).
    pub app: String,
    /// Window title; may be `None` without screen-recording permission.
    pub title: Option<String>,
    pub bounds: crate::Rect,
    pub on_screen: bool,
    /// Layer: 0 = normal application window.
    pub layer: i32,
}

/// One normalized snapshot of the world.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    pub id: ObservationId,
    pub timestamp: SystemTime,
    /// The app this observation was scoped to, if any.
    pub app: Option<AppSelector>,
    /// Resolved pid when `app` was given.
    pub pid: Option<i32>,
    pub windows: Vec<Window>,
    /// Flattened element list (tree order, `parent` links back).
    pub elements: Vec<Element>,
    /// True when the element walk hit a depth/count cap — callers must not
    /// treat "not found" as definitive when this is set.
    #[serde(default)]
    pub elements_truncated: bool,
    /// Elements that failed mid-read (vanishing nodes etc.). Partial data
    /// is explicit, never silently dropped.
    #[serde(default)]
    pub collection_errors: u32,
    /// The platform reports windows for this app (CGWindowList) but the
    /// accessibility tree shows none — the signature of a degraded AX
    /// grant (e.g. a re-signed binary the permission no longer applies to)
    /// or an app that simply doesn't expose its content. `not found`
    /// results against such a tree are not definitive.
    #[serde(default)]
    pub ax_limited: bool,
    /// Path of the captured screenshot, if requested.
    pub screenshot: Option<String>,
    /// Compact text rendering of this observation — this is what decision
    /// engines (e.g. Laya) consume as `state`.
    pub digest: String,
}

impl Default for Observation {
    /// Empty observation — used by tests and sim drivers; real drivers
    /// fill every field explicitly.
    fn default() -> Self {
        Self {
            id: ObservationId(0),
            timestamp: SystemTime::UNIX_EPOCH,
            app: None,
            pid: None,
            windows: Vec::new(),
            elements: Vec::new(),
            elements_truncated: false,
            collection_errors: 0,
            ax_limited: false,
            screenshot: None,
            digest: String::new(),
        }
    }
}

impl Observation {
    pub fn element(&self, id: ElementId) -> Option<&Element> {
        self.elements.iter().find(|e| e.id == id)
    }
}
