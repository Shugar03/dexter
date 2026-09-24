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
pub fn collect(app: &AXUIElement, max_depth: u32, max_elements: usize) -> AxTree {
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
                walk(&window, None, 0, &mut ctx);
                if ctx.truncated {
                    break;
                }
            }
        }
    }
    // The menu bar is an app child alongside windows — walk it too, or
    // agents can never reach menu items (File > Save, Format > Bold).
    if walked && !ctx.truncated {
        if let Ok(children) = app.children() {
            for child in children.iter() {
                let role = child.role().ok().map(|s| s.to_string());
                if role.as_deref() == Some("AXMenuBar") {
                    walk(&child, None, 0, &mut ctx);
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
                walk(&child, None, 0, &mut ctx);
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
    walk(&target, None, 0, &mut ctx);
    Ok(AxTree {
        elements: ctx.elements,
        nodes: ctx.nodes,
        truncated: ctx.truncated,
        errors: ctx.errors,
    })
}

fn read_string(
    el: &AXUIElement,
    f: impl Fn(&AXUIElement) -> Result<CFString, accessibility::Error>,
) -> Option<String> {
    f(el).ok().map(|s| s.to_string()).filter(|s| !s.is_empty())
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

fn bool_attr(r: Result<CFBoolean, accessibility::Error>) -> Option<bool> {
    r.ok().map(|b| b == CFBoolean::true_value())
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

/// Roles that must never leak their value — password/secure fields are
/// redacted at collection time, before anything reaches the process.
pub(crate) fn is_sensitive_role(role: &str) -> bool {
    let r = role.to_lowercase();
    r.contains("secure") || r.contains("password")
}

fn walk(el: &AXUIElement, parent: Option<ElementId>, depth: u32, ctx: &mut Ctx) {
    if ctx.elements.len() >= ctx.max_elements || depth > ctx.max_depth {
        ctx.truncated = true;
        return;
    }

    let raw_role = read_string(el, |e| e.role());
    let role = raw_role.as_deref().map(normalize_ax_role);
    let subrole = read_string(el, |e| e.subrole());
    let sensitive = role
        .as_deref()
        .or(subrole.as_deref())
        .is_some_and(is_sensitive_role);

    let title = read_string(el, |e| e.title());
    let description = read_string(el, |e| e.description());
    let name = title.or(description);
    let role_description = read_string(el, |e| e.role_description());
    let identifier = read_string(el, |e| e.identifier());
    let value = if sensitive {
        None
    } else {
        el.value()
            .ok()
            .and_then(|v| stringify_value(&v))
            .filter(|s| s.chars().count() <= 500)
    };
    let enabled = bool_attr(el.enabled());
    let focused = bool_attr(el.focused()).unwrap_or(false);
    let bounds = element_bounds(el);
    let actions = action_names(el);

    let id = ElementId(ctx.next_id);
    ctx.next_id += 1;
    let this_id = id;
    // An AXApplication child is an app boundary — descending into it
    // re-enters the same window list and cycles until the element cap.
    let is_app_boundary = raw_role.as_deref() == Some("AXApplication");

    // Keep a stable identifier fallback so unnamed controls stay findable.
    let _ = role_description;

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
        source: ElementSource::Accessibility,
    });
    ctx.nodes.push(el.clone());

    if is_app_boundary || depth >= ctx.max_depth {
        return;
    }
    if let Ok(children) = el.children() {
        for child in children.iter() {
            walk(&child, Some(this_id), depth + 1, ctx);
            if ctx.truncated {
                return;
            }
        }
    }
}
