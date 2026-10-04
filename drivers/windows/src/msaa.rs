//! MSAA (`IAccessible`) fallback for controls UIA misses.
//!
//! The pure rules — role/state tables, the merge that drops duplicates
//! and re-parents orphans — are platform-free so they compile and test
//! on every OS. The live COM walk is `#[cfg(windows)]` below.
//!
//! When the pass runs (the honest trigger): a window's UIA subtree is
//! empty or root-only (UIA attached but saw no interior), or a
//! descendant `HWND` went unclaimed by every UIA element's
//! `NativeWindowHandle`. Covered content is never re-walked: UIA's
//! LegacyIAccessible bridge already surfaces it, and the dedupe would
//! just throw it away again. What the pass cannot see is documented in
//! `docs/sdd/windows.md`: windowless MSAA children *inside* a subtree
//! UIA already covers are not re-scanned — bridging handles them.

use dexter_core::{Element, ElementId, Rect};

/// MSAA `ROLE_SYSTEM_*` protocol ids (oleacc.h — a stable ABI), listed
/// so the tables and their tests compile on every platform.
pub const ROLE_SYSTEM_TITLEBAR: u32 = 1;
pub const ROLE_SYSTEM_MENUBAR: u32 = 2;
pub const ROLE_SYSTEM_SCROLLBAR: u32 = 3;
pub const ROLE_SYSTEM_GRIP: u32 = 4;
pub const ROLE_SYSTEM_SOUND: u32 = 5;
pub const ROLE_SYSTEM_CURSOR: u32 = 6;
pub const ROLE_SYSTEM_CARET: u32 = 7;
pub const ROLE_SYSTEM_ALERT: u32 = 8;
pub const ROLE_SYSTEM_WINDOW: u32 = 9;
pub const ROLE_SYSTEM_CLIENT: u32 = 10;
pub const ROLE_SYSTEM_MENUPOPUP: u32 = 11;
pub const ROLE_SYSTEM_MENUITEM: u32 = 12;
pub const ROLE_SYSTEM_TOOLTIP: u32 = 13;
pub const ROLE_SYSTEM_APPLICATION: u32 = 14;
pub const ROLE_SYSTEM_DOCUMENT: u32 = 15;
pub const ROLE_SYSTEM_PANE: u32 = 16;
pub const ROLE_SYSTEM_CHART: u32 = 17;
pub const ROLE_SYSTEM_DIALOG: u32 = 18;
pub const ROLE_SYSTEM_BORDER: u32 = 19;
pub const ROLE_SYSTEM_GROUPING: u32 = 20;
pub const ROLE_SYSTEM_SEPARATOR: u32 = 21;
pub const ROLE_SYSTEM_TOOLBAR: u32 = 22;
pub const ROLE_SYSTEM_STATUSBAR: u32 = 23;
pub const ROLE_SYSTEM_TABLE: u32 = 24;
pub const ROLE_SYSTEM_COLUMNHEADER: u32 = 25;
pub const ROLE_SYSTEM_ROWHEADER: u32 = 26;
pub const ROLE_SYSTEM_COLUMN: u32 = 27;
pub const ROLE_SYSTEM_ROW: u32 = 28;
pub const ROLE_SYSTEM_CELL: u32 = 29;
pub const ROLE_SYSTEM_LINK: u32 = 30;
pub const ROLE_SYSTEM_HELPBALLOON: u32 = 31;
pub const ROLE_SYSTEM_CHARACTER: u32 = 32;
pub const ROLE_SYSTEM_LIST: u32 = 33;
pub const ROLE_SYSTEM_LISTITEM: u32 = 34;
pub const ROLE_SYSTEM_OUTLINE: u32 = 35;
pub const ROLE_SYSTEM_OUTLINEITEM: u32 = 36;
pub const ROLE_SYSTEM_PAGETAB: u32 = 37;
pub const ROLE_SYSTEM_PROPERTYPAGE: u32 = 38;
pub const ROLE_SYSTEM_INDICATOR: u32 = 39;
pub const ROLE_SYSTEM_GRAPHIC: u32 = 40;
pub const ROLE_SYSTEM_STATICTEXT: u32 = 41;
pub const ROLE_SYSTEM_TEXT: u32 = 42;
pub const ROLE_SYSTEM_PUSHBUTTON: u32 = 43;
pub const ROLE_SYSTEM_CHECKBUTTON: u32 = 44;
pub const ROLE_SYSTEM_RADIOBUTTON: u32 = 45;
pub const ROLE_SYSTEM_COMBOBOX: u32 = 46;
pub const ROLE_SYSTEM_DROPLIST: u32 = 47;
pub const ROLE_SYSTEM_PROGRESSBAR: u32 = 48;
pub const ROLE_SYSTEM_DIAL: u32 = 49;
pub const ROLE_SYSTEM_HOTKEYFIELD: u32 = 50;
pub const ROLE_SYSTEM_SLIDER: u32 = 51;
pub const ROLE_SYSTEM_SPINBUTTON: u32 = 52;
pub const ROLE_SYSTEM_DIAGRAM: u32 = 53;
pub const ROLE_SYSTEM_ANIMATION: u32 = 54;
pub const ROLE_SYSTEM_EQUATION: u32 = 55;
pub const ROLE_SYSTEM_BUTTONDROPDOWN: u32 = 56;
pub const ROLE_SYSTEM_BUTTONMENU: u32 = 57;
pub const ROLE_SYSTEM_BUTTONDROPDOWNGRID: u32 = 58;
pub const ROLE_SYSTEM_WHITESPACE: u32 = 59;
pub const ROLE_SYSTEM_PAGETABLIST: u32 = 60;
pub const ROLE_SYSTEM_CLOCK: u32 = 61;
pub const ROLE_SYSTEM_SPLITBUTTON: u32 = 62;
pub const ROLE_SYSTEM_IPADDRESS: u32 = 63;
pub const ROLE_SYSTEM_OUTLINEBUTTON: u32 = 64;

/// `STATE_SYSTEM_*` bit flags the element mapping consults (oleacc.h).
pub const STATE_SYSTEM_UNAVAILABLE: u32 = 0x1;
pub const STATE_SYSTEM_SELECTED: u32 = 0x2;
pub const STATE_SYSTEM_FOCUSED: u32 = 0x4;
pub const STATE_SYSTEM_CHECKED: u32 = 0x10;
pub const STATE_SYSTEM_MIXED: u32 = 0x20;
pub const STATE_SYSTEM_READONLY: u32 = 0x40;
pub const STATE_SYSTEM_INVISIBLE: u32 = 0x8000;
pub const STATE_SYSTEM_OFFSCREEN: u32 = 0x10000;
pub const STATE_SYSTEM_FOCUSABLE: u32 = 0x100000;
pub const STATE_SYSTEM_SELECTABLE: u32 = 0x200000;
pub const STATE_SYSTEM_PROTECTED: u32 = 0x20000000;
pub const STATE_SYSTEM_EXPANDED: u32 = 0x200;
pub const STATE_SYSTEM_COLLAPSED: u32 = 0x400;
pub const STATE_SYSTEM_HASPOPUP: u32 = 0x40000000;

/// MSAA `ROLE_SYSTEM_*` id → its programmatic name.
///
/// Ids are the stable oleacc.h constants; unknown ids return `None` —
/// an element keeps its numeric raw role rather than wearing an
/// invented name.
pub fn msaa_role_name(id: u32) -> Option<&'static str> {
    Some(match id {
        ROLE_SYSTEM_TITLEBAR => "ROLE_SYSTEM_TITLEBAR",
        ROLE_SYSTEM_MENUBAR => "ROLE_SYSTEM_MENUBAR",
        ROLE_SYSTEM_SCROLLBAR => "ROLE_SYSTEM_SCROLLBAR",
        ROLE_SYSTEM_GRIP => "ROLE_SYSTEM_GRIP",
        ROLE_SYSTEM_SOUND => "ROLE_SYSTEM_SOUND",
        ROLE_SYSTEM_CURSOR => "ROLE_SYSTEM_CURSOR",
        ROLE_SYSTEM_CARET => "ROLE_SYSTEM_CARET",
        ROLE_SYSTEM_ALERT => "ROLE_SYSTEM_ALERT",
        ROLE_SYSTEM_WINDOW => "ROLE_SYSTEM_WINDOW",
        ROLE_SYSTEM_CLIENT => "ROLE_SYSTEM_CLIENT",
        ROLE_SYSTEM_MENUPOPUP => "ROLE_SYSTEM_MENUPOPUP",
        ROLE_SYSTEM_MENUITEM => "ROLE_SYSTEM_MENUITEM",
        ROLE_SYSTEM_TOOLTIP => "ROLE_SYSTEM_TOOLTIP",
        ROLE_SYSTEM_APPLICATION => "ROLE_SYSTEM_APPLICATION",
        ROLE_SYSTEM_DOCUMENT => "ROLE_SYSTEM_DOCUMENT",
        ROLE_SYSTEM_PANE => "ROLE_SYSTEM_PANE",
        ROLE_SYSTEM_CHART => "ROLE_SYSTEM_CHART",
        ROLE_SYSTEM_DIALOG => "ROLE_SYSTEM_DIALOG",
        ROLE_SYSTEM_BORDER => "ROLE_SYSTEM_BORDER",
        ROLE_SYSTEM_GROUPING => "ROLE_SYSTEM_GROUPING",
        ROLE_SYSTEM_SEPARATOR => "ROLE_SYSTEM_SEPARATOR",
        ROLE_SYSTEM_TOOLBAR => "ROLE_SYSTEM_TOOLBAR",
        ROLE_SYSTEM_STATUSBAR => "ROLE_SYSTEM_STATUSBAR",
        ROLE_SYSTEM_TABLE => "ROLE_SYSTEM_TABLE",
        ROLE_SYSTEM_COLUMNHEADER => "ROLE_SYSTEM_COLUMNHEADER",
        ROLE_SYSTEM_ROWHEADER => "ROLE_SYSTEM_ROWHEADER",
        ROLE_SYSTEM_COLUMN => "ROLE_SYSTEM_COLUMN",
        ROLE_SYSTEM_ROW => "ROLE_SYSTEM_ROW",
        ROLE_SYSTEM_CELL => "ROLE_SYSTEM_CELL",
        ROLE_SYSTEM_LINK => "ROLE_SYSTEM_LINK",
        ROLE_SYSTEM_HELPBALLOON => "ROLE_SYSTEM_HELPBALLOON",
        ROLE_SYSTEM_CHARACTER => "ROLE_SYSTEM_CHARACTER",
        ROLE_SYSTEM_LIST => "ROLE_SYSTEM_LIST",
        ROLE_SYSTEM_LISTITEM => "ROLE_SYSTEM_LISTITEM",
        ROLE_SYSTEM_OUTLINE => "ROLE_SYSTEM_OUTLINE",
        ROLE_SYSTEM_OUTLINEITEM => "ROLE_SYSTEM_OUTLINEITEM",
        ROLE_SYSTEM_PAGETAB => "ROLE_SYSTEM_PAGETAB",
        ROLE_SYSTEM_PROPERTYPAGE => "ROLE_SYSTEM_PROPERTYPAGE",
        ROLE_SYSTEM_INDICATOR => "ROLE_SYSTEM_INDICATOR",
        ROLE_SYSTEM_GRAPHIC => "ROLE_SYSTEM_GRAPHIC",
        ROLE_SYSTEM_STATICTEXT => "ROLE_SYSTEM_STATICTEXT",
        ROLE_SYSTEM_TEXT => "ROLE_SYSTEM_TEXT",
        ROLE_SYSTEM_PUSHBUTTON => "ROLE_SYSTEM_PUSHBUTTON",
        ROLE_SYSTEM_CHECKBUTTON => "ROLE_SYSTEM_CHECKBUTTON",
        ROLE_SYSTEM_RADIOBUTTON => "ROLE_SYSTEM_RADIOBUTTON",
        ROLE_SYSTEM_COMBOBOX => "ROLE_SYSTEM_COMBOBOX",
        ROLE_SYSTEM_DROPLIST => "ROLE_SYSTEM_DROPLIST",
        ROLE_SYSTEM_PROGRESSBAR => "ROLE_SYSTEM_PROGRESSBAR",
        ROLE_SYSTEM_DIAL => "ROLE_SYSTEM_DIAL",
        ROLE_SYSTEM_HOTKEYFIELD => "ROLE_SYSTEM_HOTKEYFIELD",
        ROLE_SYSTEM_SLIDER => "ROLE_SYSTEM_SLIDER",
        ROLE_SYSTEM_SPINBUTTON => "ROLE_SYSTEM_SPINBUTTON",
        ROLE_SYSTEM_DIAGRAM => "ROLE_SYSTEM_DIAGRAM",
        ROLE_SYSTEM_ANIMATION => "ROLE_SYSTEM_ANIMATION",
        ROLE_SYSTEM_EQUATION => "ROLE_SYSTEM_EQUATION",
        ROLE_SYSTEM_BUTTONDROPDOWN => "ROLE_SYSTEM_BUTTONDROPDOWN",
        ROLE_SYSTEM_BUTTONMENU => "ROLE_SYSTEM_BUTTONMENU",
        ROLE_SYSTEM_BUTTONDROPDOWNGRID => "ROLE_SYSTEM_BUTTONDROPDOWNGRID",
        ROLE_SYSTEM_WHITESPACE => "ROLE_SYSTEM_WHITESPACE",
        ROLE_SYSTEM_PAGETABLIST => "ROLE_SYSTEM_PAGETABLIST",
        ROLE_SYSTEM_CLOCK => "ROLE_SYSTEM_CLOCK",
        ROLE_SYSTEM_SPLITBUTTON => "ROLE_SYSTEM_SPLITBUTTON",
        ROLE_SYSTEM_IPADDRESS => "ROLE_SYSTEM_IPADDRESS",
        ROLE_SYSTEM_OUTLINEBUTTON => "ROLE_SYSTEM_OUTLINEBUTTON",
        _ => return None,
    })
}

/// MSAA `ROLE_SYSTEM_*` name → dexter normalized role — the same
/// vocabulary `uia_role` emits so `SemanticTarget` matching stays
/// platform-agnostic. Roles with no honest counterpart stay `None`
/// (the element keeps its `msaa:` raw role instead of a guess).
pub fn msaa_role(role_name: &str) -> Option<&'static str> {
    Some(match role_name {
        "ROLE_SYSTEM_PUSHBUTTON"
        | "ROLE_SYSTEM_SPLITBUTTON"
        | "ROLE_SYSTEM_BUTTONDROPDOWN"
        | "ROLE_SYSTEM_BUTTONDROPDOWNGRID"
        | "ROLE_SYSTEM_OUTLINEBUTTON"
        | "ROLE_SYSTEM_BUTTONMENU" => "button",
        "ROLE_SYSTEM_CHECKBUTTON" => "check_box",
        "ROLE_SYSTEM_RADIOBUTTON" => "radio_button",
        "ROLE_SYSTEM_TEXT" | "ROLE_SYSTEM_HOTKEYFIELD" | "ROLE_SYSTEM_IPADDRESS" => "text_field",
        "ROLE_SYSTEM_STATICTEXT" => "static_text",
        "ROLE_SYSTEM_COMBOBOX" | "ROLE_SYSTEM_DROPLIST" => "combo_box",
        "ROLE_SYSTEM_LIST" => "list",
        "ROLE_SYSTEM_LISTITEM" => "list_item",
        "ROLE_SYSTEM_TABLE" => "table",
        "ROLE_SYSTEM_CELL" => "cell",
        "ROLE_SYSTEM_ROW" => "row",
        "ROLE_SYSTEM_ROWHEADER" => "row_header",
        "ROLE_SYSTEM_COLUMNHEADER" => "column_header",
        "ROLE_SYSTEM_OUTLINE" => "outline",
        "ROLE_SYSTEM_OUTLINEITEM" => "row",
        "ROLE_SYSTEM_MENUBAR" => "menu_bar",
        "ROLE_SYSTEM_MENUPOPUP" => "menu",
        "ROLE_SYSTEM_MENUITEM" => "menu_item",
        "ROLE_SYSTEM_SCROLLBAR" => "scroll_bar",
        "ROLE_SYSTEM_SLIDER" | "ROLE_SYSTEM_DIAL" => "slider",
        "ROLE_SYSTEM_SPINBUTTON" => "stepper",
        "ROLE_SYSTEM_PROGRESSBAR" | "ROLE_SYSTEM_INDICATOR" => "progress_indicator",
        "ROLE_SYSTEM_PAGETAB" => "tab",
        "ROLE_SYSTEM_PAGETABLIST" => "tab_group",
        "ROLE_SYSTEM_PANE"
        | "ROLE_SYSTEM_GROUPING"
        | "ROLE_SYSTEM_CLIENT"
        | "ROLE_SYSTEM_PROPERTYPAGE" => "group",
        "ROLE_SYSTEM_WINDOW" | "ROLE_SYSTEM_APPLICATION" => "window",
        "ROLE_SYSTEM_DIALOG" => "dialog",
        "ROLE_SYSTEM_TITLEBAR" => "title_bar",
        "ROLE_SYSTEM_TOOLBAR" => "toolbar",
        "ROLE_SYSTEM_STATUSBAR" => "status_bar",
        "ROLE_SYSTEM_TOOLTIP" | "ROLE_SYSTEM_HELPBALLOON" => "tooltip",
        "ROLE_SYSTEM_LINK" => "link",
        "ROLE_SYSTEM_GRAPHIC" => "image",
        "ROLE_SYSTEM_SEPARATOR" => "separator",
        "ROLE_SYSTEM_DOCUMENT" => "text_area",
        _ => return None,
    })
}

/// One node in an MSAA forest: the element payload (ids/parent/depth
/// are meaningless until the merge assigns them) plus the forest-parent
/// index — `None` at roots. Platform-free so merge tests run anywhere.
#[derive(Debug, Clone)]
pub struct MsaaNode {
    pub role: Option<String>,
    /// Raw role carrying the honest origin, e.g. `msaa:ROLE_SYSTEM_PUSHBUTTON`.
    pub raw_role: Option<String>,
    pub name: Option<String>,
    pub value: Option<String>,
    pub bounds: Option<Rect>,
    pub enabled: Option<bool>,
    pub focused: bool,
    pub actions: Vec<String>,
    /// Forest parent: index into the same `Vec<MsaaNode>` — `None` at roots.
    pub parent: Option<usize>,
}

/// Whether a window's UIA partition warrants an MSAA second pass on
/// that window's own `OBJID_CLIENT` root: the tree is empty (UIA never
/// attached — closed/ungrippable window) or it holds only the window
/// root (UIA attached and saw no interior). Unclaimed descendant HWNDs
/// are decided separately by the live walk.
pub fn warranted(partition: &[Element]) -> bool {
    partition.len() <= 1
}

/// Whether an MSAA node is the same control a UIA element already
/// surfaced — same name, same bounds (within the ±2px drift the
/// identity check tolerates), same role when both mapped. A bit-
/// identical rect + name is the same control even across role
/// disagreement: two distinct controls never share both.
fn same_control(node: &MsaaNode, el: &Element) -> bool {
    if node.name.as_deref() != el.name.as_deref() {
        return false;
    }
    let same_bounds = crate::resolve::bounds_close(node.bounds, el.bounds);
    let identical = matches!(
        (node.bounds, el.bounds),
        (Some(a), Some(b))
            if a.x == b.x && a.y == b.y && a.w == b.w && a.h == b.h
    );
    same_bounds && (identical || node.role.is_none() || el.role.is_none() || node.role == el.role)
}

/// Whether `b` lies inside `r` (±4px edges, strictly smaller) — an
/// element contained in a child window's rect is interior content,
/// while the window root and the frame element spanning `r` are not.
fn inside(b: &Rect, r: &Rect) -> bool {
    let t = 4.0;
    b.x >= r.x - t
        && b.y >= r.y - t
        && b.x + b.w <= r.x + r.w + t
        && b.y + b.h <= r.y + r.h + t
        && (b.w < r.w - 2.0 * t || b.h < r.h - 2.0 * t)
}

/// Does the UIA partition already have interior content inside the
/// child window `rect`? Elements merely containing or exactly filling
/// the rect (window root, the frame element itself) don't count.
pub fn interior_covered(partition: &[Element], rect: &Rect) -> bool {
    partition
        .iter()
        .any(|e| e.bounds.as_ref().is_some_and(|b| inside(b, rect)))
}

/// Merge one window's MSAA forest into its UIA `partition`.
///
/// Returns `(forest_index, element)` pairs for the nodes that survive
/// dedupe — new ids come from `next_id` (1-based positions continue
/// across the merged list, so `Target::Element` index math holds).
/// Dropped nodes map their children onto the UIA element they
/// duplicated, so kept grandchildren still get honest parents.
/// `partition[0]` is the window's UIA root; MSAA roots attach under it
/// (or stay depth-0 orphans when the partition is empty).
pub fn merge_msaa(
    partition: &[Element],
    forest: &[MsaaNode],
    next_id: &mut u64,
) -> Vec<(usize, Element)> {
    let mut out: Vec<(usize, Element)> = Vec::new();
    // emitted[i] = the element id forest node i maps to — kept nodes get
    // a fresh id, dropped nodes adopt the duplicate's id so descendants
    // re-parent to the UIA twin instead of a phantom.
    let mut emitted: Vec<ElementId> = Vec::with_capacity(forest.len());
    for (i, node) in forest.iter().enumerate() {
        let dup = partition
            .iter()
            .find(|e| same_control(node, e))
            .or_else(|| out.iter().map(|(_, e)| e).find(|e| same_control(node, e)));
        if let Some(e) = dup {
            emitted.push(e.id);
            continue;
        }
        let parent = node
            .parent
            .map(|p| emitted[p])
            .or_else(|| partition.first().map(|r| r.id));
        // Parent depth from the partition or an already-emitted element —
        // looked up before the push, so `out` isn't double-borrowed.
        let depth = parent
            .map(|p| {
                partition
                    .iter()
                    .chain(out.iter().map(|(_, e)| e))
                    .find(|e| e.id == p)
                    .map(|e| e.depth)
                    .unwrap_or(0)
                    + 1
            })
            .unwrap_or(0);
        let id = ElementId(*next_id);
        *next_id += 1;
        emitted.push(id);
        out.push((
            i,
            Element {
                id,
                parent,
                depth,
                role: node.role.clone(),
                raw_role: node.raw_role.clone(),
                subrole: None,
                name: node.name.clone(),
                value: node.value.clone(),
                bounds: node.bounds,
                enabled: node.enabled,
                focused: node.focused,
                actions: node.actions.clone(),
                identifier: None,
                source: dexter_core::ElementSource::Accessibility,
            },
        ));
    }
    out
}

// ---------------- live walk (real MSAA, Windows only) ----------------

#[cfg(windows)]
pub use sys::*;

#[cfg(windows)]
mod sys {
    use super::*;
    use dexter_driver::DriverError;
    use std::sync::Mutex;
    use windows::core::{Interface, BOOL, BSTR};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::System::Variant::{
        VariantClear, VARIANT, VT_BSTR, VT_DISPATCH, VT_I2, VT_I4, VT_UI4,
    };
    use windows::Win32::UI::Accessibility::{
        AccessibleChildren, AccessibleObjectFromWindow, IAccessible, WindowFromAccessibleObject,
        SELFLAG_TAKEFOCUS,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetWindowRect, GetWindowThreadProcessId, IsChild, IsWindowVisible,
        CHILDID_SELF, OBJID_CLIENT,
    };

    /// A live MSAA handle: the `IAccessible` object plus the child id the
    /// call targets (`CHILDID_SELF` when the object is the element itself,
    /// a 1-based id for simple elements inside it). One value carries both
    /// full-object and windowless children — MSAA's own addressing scheme.
    #[derive(Clone)]
    pub struct MsaaRef {
        pub acc: IAccessible,
        pub child: i32,
    }

    /// One MSAA window-subtree walk: the forest plus the live handle of
    /// every node (aligned by index — only kept nodes' handles reach the
    /// merged element list).
    pub struct MsaaWalk {
        pub forest: Vec<MsaaNode>,
        pub handles: Vec<MsaaRef>,
        pub truncated: bool,
        pub errors: u32,
    }

    /// Value length cap — same bound the UIA walk applies.
    const MAX_VALUE_CHARS: usize = 500;
    /// `accChildCount` can lie (providers report stale counts); read
    /// children in chunks so a huge count can't pin a single call.
    const CHILD_CHUNK: i32 = 64;

    /// MSAA serializes with the UIA walk — both are COM probes of the
    /// same trees and `OBSERVE_LOCK` already guarantees one reader.
    fn oleacc_lock() -> std::sync::MutexGuard<'static, ()> {
        static MSAA_LOCK: Mutex<()> = Mutex::new(());
        MSAA_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `VARIANT` holding a VT_I4 child id — MSAA's addressing currency.
    pub fn var_child(id: i32) -> VARIANT {
        let mut v = VARIANT::default();
        unsafe {
            let inner = &mut *v.Anonymous.Anonymous;
            inner.vt = VT_I4;
            inner.Anonymous.lVal = id;
        }
        v
    }

    /// Read the i32 payload of a numeric VARIANT (`VT_I4`/`VT_UI4`/`VT_I2`
    /// — providers differ); anything else is not a number we can use.
    fn var_i32(v: &VARIANT) -> Option<i32> {
        unsafe {
            let inner = &*v.Anonymous.Anonymous;
            match inner.vt {
                t if t == VT_I4 => Some(inner.Anonymous.lVal),
                t if t == VT_UI4 => Some(inner.Anonymous.ulVal as i32),
                t if t == VT_I2 => Some(inner.Anonymous.iVal as i32),
                _ => None,
            }
        }
    }

    /// Owned BSTR → String; empty is `None` — an absent name/value is
    /// data, not an empty string.
    fn bstr(b: BSTR) -> Option<String> {
        let s = String::from_utf16_lossy(&b);
        (!s.is_empty()).then_some(s)
    }

    /// `IAccessible` for a window's `OBJID_CLIENT` — the content root
    /// (the frame objects UIA anchors on live under other OBJIDs).
    fn client_acc(hwnd: HWND) -> Result<IAccessible, DriverError> {
        let mut raw: *mut core::ffi::c_void = core::ptr::null_mut();
        unsafe {
            AccessibleObjectFromWindow(hwnd, OBJID_CLIENT.0 as u32, &IAccessible::IID, &mut raw)
        }
        .map_err(|e| DriverError::Platform(format!("AccessibleObjectFromWindow: {e}")))?;
        if raw.is_null() {
            return Err(DriverError::Platform(
                "AccessibleObjectFromWindow returned a null object".into(),
            ));
        }
        // SAFETY: the function returned a valid IAccessible reference.
        Ok(unsafe { IAccessible::from_raw(raw) })
    }

    /// Screen rect of an HWND (`GetWindowRect`) — the coverage test
    /// needs the rect, not the handle.
    unsafe fn window_rect(hwnd: HWND) -> Option<Rect> {
        let mut rc = windows::Win32::Foundation::RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rc) }.ok().and_then(|_| {
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            (w > 0 && h > 0).then_some(Rect {
                x: rc.left as f64,
                y: rc.top as f64,
                w: w as f64,
                h: h as f64,
            })
        })
    }

    /// Every descendant HWND of `hwnd` (`EnumChildWindows` recurses).
    /// Claimed HWNDs are filtered by the caller.
    fn descendant_hwnds(hwnd: HWND) -> Vec<HWND> {
        unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let out = &mut *(lparam.0 as *mut Vec<HWND>);
            out.push(hwnd);
            BOOL(1)
        }
        let mut out = Vec::new();
        let _ =
            unsafe { EnumChildWindows(Some(hwnd), Some(cb), LPARAM(&mut out as *mut _ as isize)) };
        out
    }

    /// One `get_acc*` read: `Ok` yields the value, `Err` flags the node
    /// — same per-element failure rule as the UIA walker (a vanished
    /// object contributes what was readable and counts once).
    fn rd<T>(r: windows::core::Result<T>, failed: &mut bool) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(_) => {
                *failed = true;
                None
            }
        }
    }

    /// Reads where `Err` is the provider's normal "property absent"
    /// (no value, no default action) — absence, not a read failure.
    fn opt<T>(r: windows::core::Result<T>) -> Option<T> {
        r.ok()
    }

    /// `(IAccessible, child id)` → the element payload. Returns `None`
    /// for `INVISIBLE`/`OFFSCREEN` nodes — they aren't reachable UI
    /// (UIA's ControlView applies the same filter).
    fn read_node(acc: &IAccessible, child: i32, failed: &mut bool) -> Option<MsaaNode> {
        let var = var_child(child);
        // State first: it gates value redaction and visibility.
        let state = rd(unsafe { acc.get_accState(&var) }, failed).and_then(|mut v| {
            let n = var_i32(&v);
            unsafe { VariantClear(&mut v) }.ok();
            n
        });
        let st = state.unwrap_or(0) as u32;
        if st & (STATE_SYSTEM_INVISIBLE | STATE_SYSTEM_OFFSCREEN) != 0 {
            return None;
        }
        let protected = st & STATE_SYSTEM_PROTECTED != 0;
        let role = rd(unsafe { acc.get_accRole(&var) }, failed).and_then(|mut v| {
            let out = unsafe {
                let inner = &*v.Anonymous.Anonymous;
                if inner.vt == VT_I4 {
                    let id = inner.Anonymous.lVal as u32;
                    msaa_role_name(id)
                        .map(|n| (format!("msaa:{n}"), msaa_role(n).map(String::from)))
                } else if inner.vt == VT_BSTR {
                    // Legacy providers may hand back a role string instead
                    // of the id — keep it as raw_role, map nothing.
                    let s = String::from_utf16_lossy(&inner.Anonymous.bstrVal);
                    (!s.is_empty()).then(|| (format!("msaa:{s}"), None))
                } else {
                    None
                }
            };
            unsafe { VariantClear(&mut v) }.ok();
            out
        });
        let (raw_role, role) = role
            .map(|(raw, norm)| (Some(raw), norm))
            .unwrap_or((None, None));
        let name = rd(unsafe { acc.get_accName(&var) }, failed).and_then(bstr);
        // PROTECTED (password) fields never expose a value — redacted
        // before it exists, same rule as UIA `CurrentIsPassword`.
        let value = if protected {
            None
        } else {
            opt(unsafe { acc.get_accValue(&var) })
                .and_then(bstr)
                .filter(|s| s.chars().count() <= MAX_VALUE_CHARS)
        };
        let bounds = {
            let (mut l, mut t, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
            rd(
                unsafe { acc.accLocation(&mut l, &mut t, &mut w, &mut h, &var) },
                failed,
            )
            .and_then(|_| {
                (w > 0 && h > 0).then_some(Rect {
                    x: l as f64,
                    y: t as f64,
                    w: w as f64,
                    h: h as f64,
                })
            })
        };
        let mut actions: Vec<String> = Vec::new();
        let readonly = st & STATE_SYSTEM_READONLY != 0;
        let writable_value = value.is_some() || {
            // Many value-capable controls report an empty current value
            // yet still accept put_accValue — probe the role instead.
            matches!(raw_role.as_deref(), Some("msaa:ROLE_SYSTEM_TEXT"))
        };
        if opt(unsafe { acc.get_accDefaultAction(&var) })
            .and_then(bstr)
            .is_some()
        {
            actions.push("press".into());
        }
        // PROTECTED masks reads, not writes — a password edit is a
        // real value sink, so `set_value` stays advertised.
        if writable_value && !readonly {
            actions.push("set_value".into());
        }
        if st & STATE_SYSTEM_FOCUSABLE != 0 {
            actions.push("focus".into());
        }
        Some(MsaaNode {
            role,
            raw_role,
            name,
            value,
            bounds,
            enabled: state.map(|_| st & STATE_SYSTEM_UNAVAILABLE == 0),
            focused: st & STATE_SYSTEM_FOCUSED != 0,
            actions,
            parent: None, // the walk links it
        })
    }

    /// Walk one root `(IAccessible, CHILDID_SELF)` depth-first. Object
    /// children recurse; simple elements are leaves by definition.
    fn visit(
        acc: &IAccessible,
        child: i32,
        parent: Option<usize>,
        depth: u32,
        w: &mut MsaaWalk,
        budget: usize,
        max_depth: u32,
    ) {
        if w.forest.len() >= budget || depth > max_depth {
            w.truncated = true;
            return;
        }
        let mut failed = false;
        let Some(mut node) = read_node(acc, child, &mut failed) else {
            // Invisible/offscreen node: it isn't UI and neither is its
            // subtree — skip both, don't even count a read error.
            return;
        };
        if failed {
            w.errors += 1;
        }
        node.parent = parent;
        let idx = w.forest.len();
        w.forest.push(node);
        w.handles.push(MsaaRef {
            acc: acc.clone(),
            child,
        });
        if depth >= max_depth || child != CHILDID_SELF as i32 {
            return; // leaves (simple elements) have no children by spec
        }

        // A node whose child count can't be read is an incomplete walk —
        // count the failure and move on (its siblings still surface).
        let count = match unsafe { acc.accChildCount() } {
            Ok(c) => c,
            Err(_) => {
                w.errors += 1;
                return;
            }
        };
        if count <= 0 {
            return;
        }
        let mut start = 0;
        while start < count && w.forest.len() < budget {
            let take = CHILD_CHUNK.min(count - start);
            let mut buf = vec![VARIANT::default(); take as usize];
            let mut got = 0i32;
            if unsafe { AccessibleChildren(acc, start, &mut buf, &mut got) }.is_err() || got <= 0 {
                break;
            }
            for v in buf.iter_mut().take(got as usize) {
                if w.forest.len() >= budget {
                    w.truncated = true;
                    break;
                }
                unsafe {
                    let inner = &*v.Anonymous.Anonymous;
                    match inner.vt {
                        t if t == VT_DISPATCH => {
                            // A full child object — clone the dispatch
                            // (addref), clear the VARIANT's own ref.
                            let disp = (*inner.Anonymous.pdispVal).clone();
                            VariantClear(v).ok();
                            if let Some(d) = disp {
                                if let Ok(child_acc) = d.cast::<IAccessible>() {
                                    visit(
                                        &child_acc,
                                        CHILDID_SELF as i32,
                                        Some(idx),
                                        depth + 1,
                                        w,
                                        budget,
                                        max_depth,
                                    );
                                }
                            }
                        }
                        t if t == VT_I4 => {
                            let cid = inner.Anonymous.lVal;
                            VariantClear(v).ok();
                            // A simple element — a leaf addressed as
                            // (parent acc, child id).
                            visit(acc, cid, Some(idx), depth + 1, w, budget, max_depth);
                        }
                        _ => {
                            VariantClear(v).ok();
                        }
                    }
                }
            }
            start += got;
            if w.truncated {
                return;
            }
        }
        if start < count {
            w.truncated = true;
        }
    }

    /// `put_accValue` probe for the TypeText arm: `Ok` means the
    /// provider accepted a semantic write; `Err` means there is no
    /// value channel here (or it refused) — the caller falls back to
    /// the keyboard path, same as a missing UIA Value pattern.
    pub fn try_put_value(r: &MsaaRef, value: &str) -> Result<(), DriverError> {
        let var = var_child(r.child);
        unsafe { r.acc.put_accValue(&var, &windows::core::BSTR::from(value)) }
            .map_err(|e| DriverError::Platform(format!("put_accValue: {e}")))
    }

    /// The HWND an `IAccessible` sits behind — for the foreground-pid
    /// check the `SendInput` fallback requires.
    pub fn hwnd_of(acc: &IAccessible) -> Option<HWND> {
        let mut hwnd = HWND::default();
        unsafe { WindowFromAccessibleObject(acc, Some(&mut hwnd)) }
            .ok()
            .filter(|_| !hwnd.0.is_null())
            .map(|_| hwnd)
    }

    /// Pid owning the window behind `acc` — `None` when the object has
    /// no HWND or the window is gone.
    pub fn pid_of(acc: &IAccessible) -> Option<i32> {
        let hwnd = hwnd_of(acc)?;
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        (pid != 0).then_some(pid as i32)
    }

    /// `Focus` on an MSAA node: `accSelect(SELFLAG_TAKEFOCUS)` — for a
    /// simple element this focuses the parent and selects the leaf,
    /// which is the only focus MSAA has.
    pub fn take_focus(r: &MsaaRef) -> Result<(), DriverError> {
        let var = var_child(r.child);
        unsafe { r.acc.accSelect(SELFLAG_TAKEFOCUS as i32, &var) }
            .map_err(|e| DriverError::Platform(format!("accSelect(TAKEFOCUS): {e}")))
    }

    /// Whether the node currently holds keyboard focus
    /// (`STATE_SYSTEM_FOCUSED`) — `false` also when the read fails.
    pub fn focused_of(r: &MsaaRef) -> bool {
        state_of(r)
            .map(|st| st & STATE_SYSTEM_FOCUSED != 0)
            .unwrap_or(false)
    }

    /// `IAccessible`-level element reads an act() arm needs after
    /// re-resolution: enabled/disabled and readonly from accState.
    pub fn state_of(r: &MsaaRef) -> Option<u32> {
        let var = var_child(r.child);
        let mut v = unsafe { r.acc.get_accState(&var) }.ok()?;
        let n = var_i32(&v);
        unsafe { VariantClear(&mut v) }.ok();
        n.map(|n| n as u32)
    }

    pub fn bounds_of(r: &MsaaRef) -> Option<Rect> {
        let var = var_child(r.child);
        let (mut l, mut t, mut w, mut h) = (0i32, 0i32, 0i32, 0i32);
        unsafe { r.acc.accLocation(&mut l, &mut t, &mut w, &mut h, &var) }
            .ok()
            .and_then(|_| {
                (w > 0 && h > 0).then_some(Rect {
                    x: l as f64,
                    y: t as f64,
                    w: w as f64,
                    h: h as f64,
                })
            })
    }

    /// Live `name`/`role` for the detail line — mirrors `describe()`.
    pub fn describe(r: &MsaaRef) -> String {
        let var = var_child(r.child);
        let role = unsafe { r.acc.get_accRole(&var) }.ok().and_then(|mut v| {
            let out = unsafe {
                let inner = &*v.Anonymous.Anonymous;
                if inner.vt == VT_I4 {
                    let id = inner.Anonymous.lVal as u32;
                    msaa_role_name(id)
                        .and_then(msaa_role)
                        .or_else(|| msaa_role_name(id))
                        .map(str::to_string)
                } else {
                    None
                }
            };
            unsafe { VariantClear(&mut v) }.ok();
            out
        });
        let name = unsafe { r.acc.get_accName(&var) }.ok().and_then(bstr);
        match (role, name) {
            (Some(r), Some(n)) => format!("{r} \"{n}\""),
            (Some(r), None) => r,
            _ => "msaa element".into(),
        }
    }

    /// Run the MSAA pass over `hwnd`'s `OBJID_CLIENT` tree, bounded by
    /// `budget` (remaining `max_elements`) and `max_depth` — the same
    /// caps the UIA walk reports truncation against.
    pub fn walk_hwnd(hwnd: HWND, max_depth: u32, budget: usize) -> MsaaWalk {
        let mut w = MsaaWalk {
            forest: Vec::new(),
            handles: Vec::new(),
            truncated: false,
            errors: 0,
        };
        if budget == 0 {
            return w;
        }
        match client_acc(hwnd) {
            Ok(acc) => {
                visit(
                    &acc,
                    CHILDID_SELF as i32,
                    None,
                    0,
                    &mut w,
                    budget,
                    max_depth,
                );
            }
            // No MSAA root for this window — an empty walk, not an error:
            // the pass declines to add anything and the observation keeps
            // whatever UIA produced.
            Err(_) => w.errors += 1,
        }
        w
    }

    /// Decide where the MSAA pass walks inside `hwnd`'s subtree and run
    /// it. The trigger is the honest coverage test: UIA attached and
    /// produced an interior → only descendant HWNDs no UIA element claims
    /// (OLE-hosted panes, skipped providers); UIA saw nothing usable →
    /// the whole `OBJID_CLIENT` tree is fair game.
    ///
    /// `partition` is this window's UIA elements (slice of the shared
    /// list); `claimed` is every NativeWindowHandle the UIA walk reported.
    /// Returns `(kept forest indices → merged elements, live handles)`.
    pub fn augment(
        partition: &[Element],
        claimed: &std::collections::HashSet<usize>,
        hwnd: HWND,
        max_depth: u32,
        budget: usize,
        next_id: &mut u64,
    ) -> (Vec<Element>, Vec<MsaaRef>, bool, u32) {
        let _lock = oleacc_lock();
        let mut roots: Vec<HWND> = Vec::new();
        if warranted(partition) {
            roots.push(hwnd);
        }
        for child in descendant_hwnds(hwnd) {
            if claimed.contains(&(child.0 as usize)) {
                continue; // UIA owns this HWND — trusted as covered.
            }
            // Hidden subtrees (inactive tab pages, collapsed panes)
            // aren't visible UI — MSAA cheerfully reports their
            // controls with live bounds, which would lie about what's
            // on screen. Same rule the ControlView walk applies.
            if !unsafe { IsWindowVisible(child) }.as_bool() {
                continue;
            }
            // A descendant of an already-walked root is covered by that
            // root's own MSAA tree — skip it to avoid double-walking.
            let nested = roots
                .iter()
                .any(|r| unsafe { IsChild(*r, child) }.as_bool());
            if nested {
                continue;
            }
            // The honest coverage test (measured on msconfig/odbcad32):
            // an unclaimed child HWND still doesn't earn a walk when
            // UIA's windowless children already fill its interior —
            // prop-page hosts are exactly that shape, and re-walking
            // them only duplicates every bridged control.
            let rect = unsafe { window_rect(child) };
            let covered = rect
                .as_ref()
                .map(|r| interior_covered(partition, r))
                .unwrap_or(false);
            if !covered {
                roots.push(child);
            }
        }
        let mut out: Vec<Element> = Vec::new();
        let mut out_handles: Vec<MsaaRef> = Vec::new();
        let mut truncated = false;
        let mut errors = 0u32;
        for root in roots {
            if budget <= out.len() {
                truncated = true;
                break;
            }
            let w = walk_hwnd(root, max_depth, budget - out.len());
            errors += w.errors;
            truncated |= w.truncated;
            let kept = merge_msaa(&partition_ext(partition, &out), &w.forest, next_id);
            for (fi, e) in kept {
                out_handles.push(w.handles[fi].clone());
                out.push(e);
            }
        }
        (out, out_handles, truncated, errors)
    }

    /// The merge dedupes against UIA partition + already-kept MSAA
    /// elements — chained slices let the second root see the first's
    /// survivors.
    fn partition_ext<'a>(partition: &'a [Element], kept: &'a [Element]) -> Vec<Element> {
        partition.iter().chain(kept.iter()).cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dexter_core::ElementSource;

    fn el(id: u64, name: Option<&str>, role: Option<&str>, bounds: Option<Rect>) -> Element {
        Element {
            id: ElementId(id),
            parent: None,
            depth: 0,
            role: role.map(String::from),
            raw_role: None,
            subrole: None,
            name: name.map(String::from),
            value: None,
            bounds,
            enabled: Some(true),
            focused: false,
            actions: vec![],
            identifier: None,
            source: ElementSource::Accessibility,
        }
    }

    fn node(
        name: Option<&str>,
        raw: Option<&str>,
        role: Option<&str>,
        b: Option<Rect>,
    ) -> MsaaNode {
        MsaaNode {
            role: role.map(String::from),
            raw_role: raw.map(String::from),
            name: name.map(String::from),
            value: None,
            bounds: b,
            enabled: Some(true),
            focused: false,
            actions: vec![],
            parent: None,
        }
    }

    fn r(x: f64, y: f64, w: f64, h: f64) -> Option<Rect> {
        Some(Rect { x, y, w, h })
    }

    #[test]
    fn msaa_role_name_covers_the_oleacc_range() {
        assert_eq!(
            msaa_role_name(ROLE_SYSTEM_PUSHBUTTON),
            Some("ROLE_SYSTEM_PUSHBUTTON")
        );
        assert_eq!(
            msaa_role_name(ROLE_SYSTEM_CLIENT),
            Some("ROLE_SYSTEM_CLIENT")
        );
        assert_eq!(
            msaa_role_name(ROLE_SYSTEM_OUTLINEBUTTON),
            Some("ROLE_SYSTEM_OUTLINEBUTTON")
        );
        assert_eq!(msaa_role_name(65), None); // past the defined range
        assert_eq!(msaa_role_name(0), None);
    }

    #[test]
    fn msaa_role_maps_to_the_uia_vocabulary() {
        assert_eq!(msaa_role("ROLE_SYSTEM_PUSHBUTTON"), Some("button"));
        assert_eq!(msaa_role("ROLE_SYSTEM_CHECKBUTTON"), Some("check_box"));
        assert_eq!(msaa_role("ROLE_SYSTEM_TEXT"), Some("text_field"));
        assert_eq!(msaa_role("ROLE_SYSTEM_PAGETAB"), Some("tab"));
        // No honest counterpart — keeps the raw role, wears no guess.
        assert_eq!(msaa_role("ROLE_SYSTEM_CLOCK"), None);
        assert_eq!(msaa_role("ROLE_SYSTEM_SOUND"), None);
        assert_eq!(msaa_role("ROLE_SYSTEM_NOPE"), None);
    }

    #[test]
    fn warranted_only_for_empty_or_root_only_partitions() {
        assert!(warranted(&[]));
        assert!(warranted(&[el(1, Some("w"), Some("window"), None)]));
        assert!(!warranted(&[
            el(1, Some("w"), Some("window"), None),
            el(2, Some("b"), Some("button"), None),
        ]));
    }

    #[test]
    fn merge_drops_name_and_bounds_duplicates() {
        let uia = vec![el(1, Some("OK"), Some("button"), r(10.0, 10.0, 40.0, 20.0))];
        // Same name + same rect (within drift) + same mapped role = dup.
        let forest = vec![node(
            Some("OK"),
            Some("msaa:ROLE_SYSTEM_PUSHBUTTON"),
            Some("button"),
            r(11.0, 11.0, 40.0, 20.0),
        )];
        let mut next_id = 10;
        assert!(merge_msaa(&uia, &forest, &mut next_id).is_empty());
        assert_eq!(next_id, 10); // nothing consumed an id
    }

    #[test]
    fn merge_drops_identical_rects_across_role_disagreement() {
        // The provider's role can't beat an exact rect+name match —
        // a UIA button and an MSAA "window" at the same identical rect
        // are the same control (measured on odbcad32's Add/Remove).
        let uia = vec![el(
            1,
            Some("Add..."),
            Some("button"),
            r(933.0, 475.0, 143.0, 29.0),
        )];
        let forest = vec![node(
            Some("Add..."),
            Some("msaa:ROLE_SYSTEM_WINDOW"),
            Some("window"),
            r(933.0, 475.0, 143.0, 29.0),
        )];
        let mut next_id = 10;
        assert!(merge_msaa(&uia, &forest, &mut next_id).is_empty());
    }

    #[test]
    fn merge_keeps_new_nodes_and_reparents_orphans() {
        let uia = vec![el(
            1,
            Some("win"),
            Some("window"),
            r(0.0, 0.0, 100.0, 100.0),
        )];
        let forest = vec![
            // dup root (the MSAA OBJID_CLIENT root mirrors the UIA root)
            node(
                Some("win"),
                Some("msaa:ROLE_SYSTEM_WINDOW"),
                Some("window"),
                r(0.0, 0.0, 100.0, 100.0),
            ),
            // its child — a control UIA missed
            node(
                Some("inner"),
                Some("msaa:ROLE_SYSTEM_PUSHBUTTON"),
                Some("button"),
                r(5.0, 5.0, 10.0, 10.0),
            ),
        ];
        let mut forest = forest;
        forest[1].parent = Some(0);
        let mut next_id = 2;
        let kept = merge_msaa(&uia, &forest, &mut next_id);
        assert_eq!(kept.len(), 1);
        let (fi, e) = &kept[0];
        assert_eq!(*fi, 1);
        assert_eq!(e.parent, Some(ElementId(1))); // reparented to the UIA twin
        assert_eq!(e.depth, 1);
        assert_eq!(e.id, ElementId(2));
        assert_eq!(e.raw_role.as_deref(), Some("msaa:ROLE_SYSTEM_PUSHBUTTON"));
    }

    #[test]
    fn merge_roots_attach_under_the_uia_root() {
        let uia = vec![el(
            1,
            Some("win"),
            Some("window"),
            r(0.0, 0.0, 100.0, 100.0),
        )];
        let forest = vec![node(
            Some("pane"),
            Some("msaa:ROLE_SYSTEM_CLIENT"),
            Some("group"),
            r(0.0, 0.0, 50.0, 50.0),
        )];
        let mut next_id = 5;
        let kept = merge_msaa(&uia, &forest, &mut next_id);
        assert_eq!(kept[0].1.parent, Some(ElementId(1)));
        assert_eq!(kept[0].1.depth, 1);
    }

    #[test]
    fn merge_with_empty_partition_leaves_orphan_roots() {
        let forest = vec![node(
            Some("thing"),
            Some("msaa:ROLE_SYSTEM_WINDOW"),
            Some("window"),
            None,
        )];
        let mut next_id = 1;
        let kept = merge_msaa(&[], &forest, &mut next_id);
        assert_eq!(kept[0].1.parent, None);
        assert_eq!(kept[0].1.depth, 0);
        assert_eq!(kept[0].1.id, ElementId(1));
    }

    #[test]
    fn merge_dedupes_within_the_forest_itself() {
        let uia = vec![el(
            1,
            Some("win"),
            Some("window"),
            r(0.0, 0.0, 100.0, 100.0),
        )];
        let dup_a = node(
            Some("X"),
            Some("msaa:ROLE_SYSTEM_STATICTEXT"),
            Some("static_text"),
            r(1.0, 1.0, 5.0, 5.0),
        );
        let dup_b = node(
            Some("X"),
            Some("msaa:ROLE_SYSTEM_STATICTEXT"),
            Some("static_text"),
            r(1.0, 1.0, 5.0, 5.0),
        );
        let mut next_id = 2;
        let kept = merge_msaa(&uia, &[dup_a, dup_b], &mut next_id);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn interior_coverage_counts_only_contained_content() {
        let page = Rect {
            x: 100.0,
            y: 100.0,
            w: 300.0,
            h: 200.0,
        };
        // The window root contains the page but is not inside it.
        let win = el(1, Some("w"), Some("window"), r(0.0, 0.0, 500.0, 500.0));
        // A frame element spanning the page itself is not interior.
        let frame = el(2, None, Some("group"), r(100.0, 100.0, 300.0, 200.0));
        assert!(!interior_covered(&[win.clone(), frame], &page));
        // A control inside the page is interior content.
        let button = el(3, Some("b"), Some("button"), r(120.0, 120.0, 50.0, 20.0));
        assert!(interior_covered(&[win, button], &page));
        // No bounds at all means nothing claims the interior.
        let vague = el(4, Some("x"), Some("group"), None);
        assert!(!interior_covered(&[vague], &page));
    }
}
