//! Accessibility tree walk: AXUIElement -> normalized `Element`s.
//!
//! Per-element failures are tolerated (elements vanish mid-walk all the
//! time) and counted; truncation is explicit, never silent.

use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
use core_foundation::base::{CFType, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use dexter_core::{Element, ElementId, ElementSource, Rect};
use dexter_world_model::normalize_ax_role;
use std::ffi::c_void;

use crate::ffi;

/// Roles that must never leak their value — the shared definition
/// lives in `dexter_core` so collection redaction, expectation
/// derivation and policy sensitivity agree.
pub(crate) use dexter_core::is_sensitive_role;

pub struct AxTree {
    pub elements: Vec<Element>,
    /// Live AXUIElement refs parallel to `elements` (same order) — needed
    /// to act on resolved elements without re-walking.
    pub nodes: Vec<AXUIElement>,
    pub truncated: bool,
    /// Elements whose attributes could not be fully read.
    pub errors: u32,
}

struct Ctx {
    max_depth: u32,
    max_elements: usize,
    elements: Vec<Element>,
    nodes: Vec<AXUIElement>,
    truncated: bool,
    errors: u32,
    next_id: u64,
}

/// Roles that can be a real window root — anything else `AXWindows`
/// returns (e.g. an `AXApplication` proxy under a degraded grant) is not.
fn is_windowish(role: Option<&str>) -> bool {
    matches!(
        role,
        Some("AXWindow")
            | Some("AXDrawer")
            | Some("AXSheet")
            | Some("AXFloatingWindow")
            | Some("AXSystemDialog")
    )
}

/// Collect the app's element tree: each AX window is a root; if the app
/// reports no *real* windows (empty list or only non-window proxies, as a
/// degraded TCC grant produces), fall back to the app's direct children —
/// that still reaches the menu bar and whatever the app does expose.
pub fn collect(
    app: &AXUIElement,
    max_depth: u32,
    max_elements: usize,
    include_menu: bool,
) -> AxTree {
    let mut ctx = Ctx {
        max_depth,
        max_elements,
        elements: Vec::new(),
        nodes: Vec::new(),
        truncated: false,
        errors: 0,
        next_id: 1,
    };

    let mut walked = false;
    if let Ok(windows) = app.windows() {
        let real: Vec<_> = windows
            .iter()
            .filter(|w| {
                let r = w.role().ok().map(|s| s.to_string());
                is_windowish(r.as_deref())
            })
            .collect();
        if !real.is_empty() {
            walked = true;
            for window in real {
                walk(&window, None, 0, &mut ctx, false);
                if ctx.truncated {
                    break;
                }
            }
        }
    }
    // The menu bar is an app child alongside windows — walk it too, or
    // agents can never reach menu items (File > Save, Format > Bold).
    // `include_menu=false` skips it: menus often outnumber window
    // elements ~10:1 and each element costs an IPC roundtrip.
    if include_menu && walked && !ctx.truncated {
        if let Ok(children) = app.children() {
            for child in children.iter() {
                let role = child.role().ok().map(|s| s.to_string());
                if role.as_deref() == Some("AXMenuBar") {
                    walk(&child, None, 0, &mut ctx, true);
                    break;
                }
            }
        }
    }
    // Menu bar extras, agents and apps under a degraded grant report no
    // AXWindows — fall back to the app element's direct children.
    if !walked {
        if let Ok(children) = app.children() {
            for child in children.iter() {
                let role = child.role().ok().map(|s| s.to_string());
                let is_menu_bar = role.as_deref() == Some("AXMenuBar");
                if is_menu_bar && !include_menu {
                    continue;
                }
                walk(&child, None, 0, &mut ctx, is_menu_bar);
                if ctx.truncated {
                    break;
                }
            }
        }
    }

    AxTree {
        elements: ctx.elements,
        nodes: ctx.nodes,
        truncated: ctx.truncated,
        errors: ctx.errors,
    }
}

/// Position/size equality within ε — CGWindowList bounds and AX bounds
/// can differ by subpixel rounding on Retina displays.
const BOUNDS_EPS: f64 = 2.0;

fn bounds_eq(a: Rect, b: Rect) -> bool {
    (a.x - b.x).abs() <= BOUNDS_EPS
        && (a.y - b.y).abs() <= BOUNDS_EPS
        && (a.w - b.w).abs() <= BOUNDS_EPS
        && (a.h - b.h).abs() <= BOUNDS_EPS
}

/// Collect only the window subtree matching `cg_bounds` — the
/// `ObservationScope.window` fast path: O(window) instead of O(app).
/// The menubar is intentionally out of scope (it doesn't live in the
/// window). `NotFound` when no AX window matches — honest miss.
///
/// Limitation: two windows with identical bounds are indistinguishable;
/// the first in `AXWindows` order wins.
pub fn collect_window(
    app: &AXUIElement,
    cg_bounds: Rect,
    max_depth: u32,
    max_elements: usize,
) -> Result<AxTree, crate::DriverError> {
    let windows = app
        .windows()
        .map_err(|e| crate::DriverError::Platform(format!("AXWindows: {e}")))?;
    let target = windows
        .iter()
        .filter(|w| {
            let r = w.role().ok().map(|s| s.to_string());
            is_windowish(r.as_deref())
        })
        .find(|w| element_bounds(w).is_some_and(|b| bounds_eq(b, cg_bounds)))
        .ok_or_else(|| {
            crate::DriverError::NotFound(format!(
                "no AX window at bounds [{},{},{}x{}]",
                cg_bounds.x, cg_bounds.y, cg_bounds.w, cg_bounds.h
            ))
        })?;

    let mut ctx = Ctx {
        max_depth,
        max_elements,
        elements: Vec::new(),
        nodes: Vec::new(),
        truncated: false,
        errors: 0,
        next_id: 1,
    };
    walk(&target, None, 0, &mut ctx, false);
    Ok(AxTree {
        elements: ctx.elements,
        nodes: ctx.nodes,
        truncated: ctx.truncated,
        errors: ctx.errors,
    })
}

/// Decode an AXValue-typed attribute ("AXPosition" / "AXSize") into a pair
/// of f64s.
fn ax_pair(el: &AXUIElement, name: &str, expected: i32) -> Option<(f64, f64)> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let v: CFType = el.attribute(&attr).ok()?;
    if unsafe { ffi::AXValueGetType(v.as_CFTypeRef()) } != expected {
        return None;
    }
    let mut out = [0.0f64; 2];
    let ok = unsafe {
        ffi::AXValueGetValue(v.as_CFTypeRef(), expected, out.as_mut_ptr() as *mut c_void)
    };
    (ok != 0).then(|| (out[0], out[1]))
}

fn element_bounds(el: &AXUIElement) -> Option<Rect> {
    let (x, y) = ax_pair(el, "AXPosition", ffi::K_AX_VALUE_CG_POINT_TYPE)?;
    let (w, h) = ax_pair(el, "AXSize", ffi::K_AX_VALUE_CG_SIZE_TYPE)?;
    Some(Rect { x, y, w, h })
}

/// CFType -> display string for scalar values only. Elements and containers
/// are skipped to keep values small and safe.
fn stringify_value(v: &CFType) -> Option<String> {
    if v.instance_of::<CFString>() {
        let s = unsafe { CFString::wrap_under_get_rule(v.as_CFTypeRef() as _) };
        return Some(s.to_string());
    }
    if v.instance_of::<CFBoolean>() {
        let b = unsafe { CFBoolean::wrap_under_get_rule(v.as_CFTypeRef() as _) };
        return Some(
            if b == CFBoolean::true_value() {
                "true"
            } else {
                "false"
            }
            .to_string(),
        );
    }
    if v.instance_of::<CFNumber>() {
        let n = unsafe { CFNumber::wrap_under_get_rule(v.as_CFTypeRef() as _) };
        return n
            .to_i64()
            .map(|i| i.to_string())
            .or_else(|| n.to_f64().map(|f| f.to_string()));
    }
    None
}

/// Semantic actions, normalized: "AXPress" -> "press".
fn action_names(el: &AXUIElement) -> Vec<String> {
    el.action_names()
        .map(|names| {
            names
                .iter()
                .map(|n| {
                    let s = n.to_string();
                    normalize_ax_role(&s)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Batched attribute read: every field `walk` needs in ONE IPC
/// roundtrip (`AXUIElementCopyMultipleAttributeValues`) instead of a
/// dozen. Failed slots arrive as AXValue-wrapped AXError and decode to
/// `None` — same per-attribute tolerance as individual reads.
const BATCH_ATTRS: [&str; 14] = [
    "AXRole",
    "AXSubrole",
    "AXTitle",
    "AXDescription",
    "AXRoleDescription",
    "AXIdentifier",
    "AXValue",
    "AXEnabled",
    "AXFocused",
    "AXPosition",
    "AXSize",
    "AXChildren",
    "AXMenuItemCmdChar",
    "AXMenuItemCmdModifiers",
];
const I_ROLE: usize = 0;
const I_SUBROLE: usize = 1;
const I_TITLE: usize = 2;
const I_DESC: usize = 3;
const I_IDENT: usize = 5;
const I_VALUE: usize = 6;
const I_ENABLED: usize = 7;
const I_FOCUSED: usize = 8;
const I_POS: usize = 9;
const I_SIZE: usize = 10;
const I_CHILDREN: usize = 11;
const I_CMDCHAR: usize = 12;
const I_CMDMODS: usize = 13;

/// Slim batch for menu-bar descendants — menus only need role, title,
/// enabled, shortcut and children. Position/size/value are absent on
/// menu items anyway, so requesting them just wastes server time.
const MENU_ATTRS: [&str; 6] = [
    "AXRole",
    "AXTitle",
    "AXEnabled",
    "AXMenuItemCmdChar",
    "AXMenuItemCmdModifiers",
    "AXChildren",
];
const M_ROLE: usize = 0;
const M_TITLE: usize = 1;
const M_ENABLED: usize = 2;
const M_CMDCHAR: usize = 3;
const M_CMDMODS: usize = 4;
const M_CHILDREN: usize = 5;

fn batch_values(el: &AXUIElement, attrs: &[&str]) -> Vec<Option<CFType>> {
    let names: Vec<CFString> = attrs.iter().map(|s| CFString::new(s)).collect();
    let arr = core_foundation::array::CFArray::from_CFTypes(&names);
    let mut out = std::ptr::null();
    let err = unsafe {
        ffi::AXUIElementCopyMultipleAttributeValues(
            el.as_CFTypeRef(),
            arr.as_concrete_TypeRef(),
            0,
            &mut out,
        )
    };
    if err != 0 || out.is_null() {
        return Vec::new();
    }
    let values = unsafe { core_foundation::array::CFArray::<CFType>::wrap_under_create_rule(out) };
    (0..attrs.len())
        .map(|i| {
            let item = values.get(i as _)?;
            // ItemRef is a borrow — retain it into an owned CFType.
            let v = unsafe { CFType::wrap_under_get_rule(item.as_CFTypeRef()) };
            // Failed slots arrive as AXValue-wrapped AXError — decode
            // to `None`. `AXValueGetType` is only defined on AXValue
            // instances, so check the CFTypeID first.
            if is_ax_error(&v) {
                None
            } else {
                Some(v)
            }
        })
        .collect()
}

fn str_slot(v: Option<&CFType>) -> Option<String> {
    v.and_then(|v| v.downcast::<CFString>())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

fn bool_slot(v: Option<&CFType>) -> Option<bool> {
    v.and_then(|v| v.downcast::<CFBoolean>())
        .map(|b| b == CFBoolean::true_value())
}

fn num_slot(v: Option<&CFType>) -> Option<i64> {
    v.and_then(|v| v.downcast::<CFNumber>())
        .and_then(|n| n.to_i64())
}

/// Whether this CFType is an AXValue wrapping an `AXError` — the
/// failure slot of a batched attribute read.
fn is_ax_error(v: &CFType) -> bool {
    use core_foundation::base::CFGetTypeID;
    unsafe {
        CFGetTypeID(v.as_CFTypeRef()) == ffi::AXValueGetTypeID()
            && ffi::AXValueGetType(v.as_CFTypeRef()) == ffi::K_AX_VALUE_AX_ERROR_TYPE
    }
}

fn pair_slot(v: Option<&CFType>, expected: i32) -> Option<(f64, f64)> {
    let v = v?;
    use core_foundation::base::CFGetTypeID;
    let is_expected = unsafe {
        CFGetTypeID(v.as_CFTypeRef()) == ffi::AXValueGetTypeID()
            && ffi::AXValueGetType(v.as_CFTypeRef()) == expected
    };
    if !is_expected {
        return None;
    }
    let mut out = [0.0f64; 2];
    let ok = unsafe {
        ffi::AXValueGetValue(v.as_CFTypeRef(), expected, out.as_mut_ptr() as *mut c_void)
    };
    (ok != 0).then(|| (out[0], out[1]))
}

fn children_slot(v: Option<&CFType>) -> Vec<AXUIElement> {
    v.and_then(|v| v.downcast::<core_foundation::array::CFArray>())
        .map(|a| {
            a.iter()
                .map(|item| unsafe { AXUIElement::wrap_under_get_rule(*item as *mut _) })
                .collect()
        })
        .unwrap_or_default()
}

fn walk(el: &AXUIElement, parent: Option<ElementId>, depth: u32, ctx: &mut Ctx, in_menu: bool) {
    if ctx.elements.len() >= ctx.max_elements || depth > ctx.max_depth {
        ctx.truncated = true;
        return;
    }
    if in_menu {
        walk_menu(el, parent, depth, ctx);
        return;
    }

    // One IPC roundtrip fetches every attribute this element needs.
    // An empty result means the whole batch call failed — the element
    // still gets recorded (degraded) rather than skipped silently.
    let vals = batch_values(el, &BATCH_ATTRS);
    if vals.is_empty() {
        ctx.errors += 1;
    }
    let slot = |i: usize| vals.get(i).and_then(|v| v.as_ref());

    let raw_role = str_slot(slot(I_ROLE));
    let role = raw_role.as_deref().map(normalize_ax_role);
    let subrole = str_slot(slot(I_SUBROLE));
    let sensitive = role
        .as_deref()
        .or(subrole.as_deref())
        .is_some_and(is_sensitive_role);

    let name = str_slot(slot(I_TITLE)).or_else(|| str_slot(slot(I_DESC)));
    let identifier = str_slot(slot(I_IDENT));
    // Sensitive values are never materialized — the slot stays undecoded.
    let value = if sensitive {
        None
    } else {
        slot(I_VALUE)
            .and_then(stringify_value)
            .filter(|s| s.chars().count() <= 500)
    };
    let enabled = bool_slot(slot(I_ENABLED));
    let focused = bool_slot(slot(I_FOCUSED)).unwrap_or(false);
    let bounds = pair_slot(slot(I_POS), ffi::K_AX_VALUE_CG_POINT_TYPE)
        .zip(pair_slot(slot(I_SIZE), ffi::K_AX_VALUE_CG_SIZE_TYPE))
        .map(|((x, y), (w, h))| Rect { x, y, w, h });
    // Menu roles exist to be pressed — AXPress is their documented
    // contract, so the per-element AXUIElementCopyActionNames IPC is
    // skipped for them (menu bars contribute ~90% of a typical tree).
    let actions = match raw_role.as_deref() {
        Some("AXMenu" | "AXMenuBar" | "AXMenuBarItem" | "AXMenuItem") => {
            vec!["press".into()]
        }
        _ => action_names(el),
    };
    // Menu items advertise their shortcut — lets `Key` plan a semantic
    // press instead of a physical chord.
    let shortcut = if raw_role.as_deref() == Some("AXMenuItem") {
        str_slot(slot(I_CMDCHAR)).map(|key| dexter_core::KeyChord {
            key: key.to_lowercase(),
            modifiers: crate::v2::menu_modifiers(num_slot(slot(I_CMDMODS)).unwrap_or(0)),
        })
    } else {
        None
    };

    let id = ElementId(ctx.next_id);
    ctx.next_id += 1;
    let this_id = id;
    // An AXApplication child is an app boundary — descending into it
    // re-enters the same window list and cycles until the element cap.
    let is_app_boundary = raw_role.as_deref() == Some("AXApplication");

    ctx.elements.push(Element {
        id,
        parent,
        depth,
        role,
        raw_role,
        subrole,
        name,
        value,
        bounds,
        enabled,
        focused,
        actions,
        identifier,
        shortcut,
        source: ElementSource::Accessibility,
    });
    ctx.nodes.push(el.clone());

    if is_app_boundary {
        return;
    }
    let children = children_slot(slot(I_CHILDREN));
    // The depth boundary is a truncation point too: unvisited children
    // mean the tree is partial, and absence-dependent verification
    // must know that (`walk_menu` flags the same case at entry).
    if depth >= ctx.max_depth {
        ctx.truncated |= !children.is_empty();
        return;
    }
    for child in children {
        walk(&child, Some(this_id), depth + 1, ctx, false);
        if ctx.truncated {
            return;
        }
    }
}

/// Menu-bar subtree: same element shape, but a 6-attribute batch —
/// menu items have no position/size/value worth fetching.
fn walk_menu(el: &AXUIElement, parent: Option<ElementId>, depth: u32, ctx: &mut Ctx) {
    if ctx.elements.len() >= ctx.max_elements || depth > ctx.max_depth {
        ctx.truncated = true;
        return;
    }
    let vals = batch_values(el, &MENU_ATTRS);
    if vals.is_empty() {
        ctx.errors += 1;
    }
    let slot = |i: usize| vals.get(i).and_then(|v| v.as_ref());

    let raw_role = str_slot(slot(M_ROLE));
    let role = raw_role.as_deref().map(normalize_ax_role);
    let shortcut = if raw_role.as_deref() == Some("AXMenuItem") {
        str_slot(slot(M_CMDCHAR)).map(|key| dexter_core::KeyChord {
            key: key.to_lowercase(),
            modifiers: crate::v2::menu_modifiers(num_slot(slot(M_CMDMODS)).unwrap_or(0)),
        })
    } else {
        None
    };
    let id = ElementId(ctx.next_id);
    ctx.next_id += 1;
    let this_id = id;
    ctx.elements.push(Element {
        id,
        parent,
        depth,
        role,
        raw_role,
        subrole: None,
        name: str_slot(slot(M_TITLE)),
        value: None,
        bounds: None,
        enabled: bool_slot(slot(M_ENABLED)),
        focused: false,
        actions: vec!["press".into()],
        identifier: None,
        shortcut,
        source: ElementSource::Accessibility,
    });
    ctx.nodes.push(el.clone());
    for child in children_slot(slot(M_CHILDREN)) {
        walk_menu(&child, Some(this_id), depth + 1, ctx);
        if ctx.truncated {
            return;
        }
    }
}
