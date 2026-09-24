//! Desktop actions v2 for macOS: Invoke, app lifecycle, window ops,
//! clipboard, multi-click and drag, plus the semantic key route through
//! the menu bar.
//!
//! The pure pieces (shortcut matching, advertised-action lookup,
//! modifier decoding) are separated from the AX plumbing so routing
//! order and fail-closed behavior are unit-testable without a desktop.

use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use core_foundation::base::{CFType, TCFType};
use core_foundation::string::CFString;
use dexter_core::{KeyChord, MouseButton, Rect};
use dexter_driver::DriverError;
use dexter_world_model::normalize_ax_role;
use objc::{class, msg_send, sel, sel_impl};

use crate::{ffi, windows};

/// The pasteboard flavor dexter handles — UTF-8 text only.
const PB_TYPE: &str = "public.utf8-plain-text";
/// Payload bound for clipboard writes (bytes).
const CLIPBOARD_MAX: usize = 1024 * 1024;

// ---------------------------------------------------------------------------
// Pure helpers — no AX objects, unit-testable.
// ---------------------------------------------------------------------------

/// The raw AX action name an element advertises that corresponds to the
/// requested normalized name (`"show_menu"` matches advertised
/// `"AXShowMenu"`). Matching against the element's own advertised list —
/// never a static table — means custom actions work and unadvertised
/// names are refused before anything reaches the platform.
pub fn ax_action_for<'a>(requested: &str, advertised: &'a [String]) -> Option<&'a str> {
    advertised
        .iter()
        .find(|raw| normalize_ax_role(raw) == requested)
        .map(String::as_str)
}

/// `AXMenuItemCmdModifiers` bit set → chord modifier names. Bit 3 means
/// "command key absent"; command is implied otherwise.
pub fn menu_modifiers(bits: i64) -> Vec<String> {
    let mut out = Vec::new();
    if bits & 8 == 0 {
        out.push("cmd".into());
    }
    if bits & 1 != 0 {
        out.push("shift".into());
    }
    if bits & 2 != 0 {
        out.push("alt".into());
    }
    if bits & 4 != 0 {
        out.push("ctrl".into());
    }
    out
}

/// Compare an `AXMenuItemCmdChar` with a chord key. Menu chars are the
/// literal character (uppercase when shift is held); a few control
/// characters name named keys.
pub fn menu_key_matches(cmd_char: &str, chord_key: &str) -> bool {
    let named = match cmd_char {
        "\r" | "\n" => Some("return"),
        "\u{1b}" => Some("escape"),
        "\t" => Some("tab"),
        "\u{7f}" => Some("delete"),
        " " => Some("space"),
        _ => None,
    };
    if let Some(n) = named {
        return chord_key.eq_ignore_ascii_case(n)
            || (n == "return" && chord_key.eq_ignore_ascii_case("enter"))
            || (n == "escape" && chord_key.eq_ignore_ascii_case("esc"));
    }
    cmd_char.eq_ignore_ascii_case(chord_key)
}

/// Same modifier set, order-insensitive.
pub fn same_modifiers(a: &[String], b: &[String]) -> bool {
    fn canon(m: &str) -> &str {
        match m {
            "command" | "meta" => "cmd",
            "option" | "opt" => "alt",
            "control" => "ctrl",
            other => other,
        }
    }
    let mut x: Vec<&str> = a.iter().map(|s| canon(s)).collect();
    let mut y: Vec<&str> = b.iter().map(|s| canon(s)).collect();
    x.sort_unstable();
    y.sort_unstable();
    x == y
}

/// Center of a rect — drag/click coordinates for resolved elements.
pub fn center(b: &Rect) -> (f64, f64) {
    (b.x + b.w / 2.0, b.y + b.h / 2.0)
}

// ---------------------------------------------------------------------------
// Invoke — perform an advertised AX action by its normalized name.
// ---------------------------------------------------------------------------

/// Perform `action` on the resolved element, but only if the element
/// itself advertises it. Returns the raw AX name performed.
pub fn invoke(el: &AXUIElement, action: &str) -> Result<String, DriverError> {
    let advertised: Vec<String> = el
        .action_names()
        .map(|names| names.iter().map(|n| n.to_string()).collect())
        .unwrap_or_default();
    let raw = ax_action_for(action, &advertised).ok_or_else(|| {
        DriverError::Unsupported(format!(
            "element does not advertise '{action}' (has: {})",
            advertised
                .iter()
                .map(|r| normalize_ax_role(r))
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    let raw = raw.to_string();
    el.perform_action(&CFString::new(&raw))
        .map_err(|e| DriverError::Platform(format!("{raw} failed: {e:?}")))?;
    Ok(raw)
}

// ---------------------------------------------------------------------------
// Clipboard — NSPasteboard, text only.
// ---------------------------------------------------------------------------

fn pasteboard(name: Option<&str>) -> id {
    unsafe {
        match name {
            Some(n) => {
                let ns = NSString::alloc(nil).init_str(n);
                msg_send![class!(NSPasteboard), pasteboardWithName: ns]
            }
            None => msg_send![class!(NSPasteboard), generalPasteboard],
        }
    }
}

/// Read pasteboard text. `name` selects a named pasteboard (tests);
/// `None` is the general pasteboard. Empty/unset returns `None`.
pub fn clipboard_read(name: Option<&str>) -> Result<Option<String>, DriverError> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let pb = pasteboard(name);
        if pb == nil {
            pool.drain();
            return Err(DriverError::Platform("NSPasteboard unavailable".into()));
        }
        let ty = NSString::alloc(nil).init_str(PB_TYPE);
        let s: id = msg_send![pb, stringForType: ty];
        let out = if s == nil {
            None
        } else {
            let utf8 = s.UTF8String();
            if utf8.is_null() {
                None
            } else {
                Some(
                    std::ffi::CStr::from_ptr(utf8)
                        .to_string_lossy()
                        .into_owned(),
                )
            }
        };
        pool.drain();
        Ok(out)
    }
}

/// Replace pasteboard text (UTF-8, bounded).
pub fn clipboard_write(name: Option<&str>, text: &str) -> Result<(), DriverError> {
    if text.len() > CLIPBOARD_MAX {
        return Err(DriverError::Unsupported(format!(
            "clipboard payload {} bytes exceeds {} byte bound",
            text.len(),
            CLIPBOARD_MAX
        )));
    }
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let pb = pasteboard(name);
        if pb == nil {
            pool.drain();
            return Err(DriverError::Platform("NSPasteboard unavailable".into()));
        }
        let _: i64 = msg_send![pb, clearContents];
        let ty = NSString::alloc(nil).init_str(PB_TYPE);
        let ns = NSString::alloc(nil).init_str(text);
        let ok: bool = msg_send![pb, setString: ns forType: ty];
        pool.drain();
        if ok {
            Ok(())
        } else {
            Err(DriverError::Platform(
                "NSPasteboard setString failed".into(),
            ))
        }
    }
}

// ---------------------------------------------------------------------------
// Windows — CGWindowID → AX window revalidated by bounds.
// ---------------------------------------------------------------------------

/// How far apart CG and AX bounds may be and still be the same window.
const WINDOW_BOUNDS_TOLERANCE: f64 = 2.0;

/// On-screen bounds of an AX element (AXPosition + AXSize).
pub fn ax_bounds(el: &AXUIElement) -> Option<Rect> {
    let pos = ax_pair(el, "AXPosition", ffi::K_AX_VALUE_CG_POINT_TYPE)?;
    let (w, h) = ax_pair(el, "AXSize", ffi::K_AX_VALUE_CG_SIZE_TYPE)?;
    Some(Rect {
        x: pos.0,
        y: pos.1,
        w,
        h,
    })
}

fn ax_pair(el: &AXUIElement, name: &str, expected: i32) -> Option<(f64, f64)> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let v: CFType = el.attribute(&attr).ok()?;
    if unsafe { ffi::AXValueGetType(v.as_CFTypeRef()) } != expected {
        return None;
    }
    let mut out = [0.0f64; 2];
    let ok = unsafe {
        ffi::AXValueGetValue(
            v.as_CFTypeRef(),
            expected,
            out.as_mut_ptr() as *mut std::ffi::c_void,
        )
    };
    (ok != 0).then(|| (out[0], out[1]))
}

fn bounds_close(a: &Rect, b: &Rect) -> bool {
    (a.x - b.x).abs() <= WINDOW_BOUNDS_TOLERANCE
        && (a.y - b.y).abs() <= WINDOW_BOUNDS_TOLERANCE
        && (a.w - b.w).abs() <= WINDOW_BOUNDS_TOLERANCE
        && (a.h - b.h).abs() <= WINDOW_BOUNDS_TOLERANCE
}

fn ax_windows_of(pid: i32) -> Vec<AXUIElement> {
    let app = AXUIElement::application(pid);
    let _ = app.set_messaging_timeout(1.5);
    let attr = AXAttribute::<CFType>::new(&CFString::new("AXWindows"));
    let arr = match app
        .attribute(&attr)
        .ok()
        .and_then(|v| v.downcast::<core_foundation::array::CFArray>())
    {
        Some(a) => a,
        None => return Vec::new(),
    };
    arr.iter()
        .filter_map(|item| {
            // Untyped CFArray items are raw AXUIElementRefs — the cast
            // target is inferred from wrap_under_get_rule's signature.
            let el = unsafe { AXUIElement::wrap_under_get_rule(*item as _) };
            (el.role().ok().map(|r| r.to_string()).as_deref() == Some("AXWindow")).then_some(el)
        })
        .collect()
}

/// Resolve a `window_id` (CGWindowID) to a live AX window, revalidated
/// by bounds — the CG list and AX tree can disagree, so an id alone is
/// never trusted. `None` resolves the scoped app's frontmost AX window.
pub fn resolve_window(
    window_id: Option<u32>,
    app_pid: Option<i32>,
) -> Result<(i32, AXUIElement), DriverError> {
    match window_id {
        Some(id) => {
            let wins = windows::list_windows()?;
            let w = wins
                .iter()
                .find(|w| w.id == id)
                .ok_or_else(|| DriverError::NotFound(format!("window {id}")))?;
            let candidates = ax_windows_of(w.pid);
            let mut matches: Vec<AXUIElement> = candidates
                .iter()
                .filter(|el| ax_bounds(el).is_some_and(|b| bounds_close(&b, &w.bounds)))
                .cloned()
                .collect();
            match matches.len() {
                0 => Err(DriverError::StaleReference(format!(
                    "window {id} has no AX counterpart at its bounds — it may have closed or moved"
                ))),
                1 => Ok((w.pid, matches.remove(0))),
                _ => Err(DriverError::Ambiguous(format!(
                    "window {id} matches {} AX windows at the same bounds",
                    matches.len()
                ))),
            }
        }
        None => {
            let pid = app_pid.ok_or_else(|| {
                DriverError::NotFound("window op needs a window_id or an app scope".into())
            })?;
            let app = AXUIElement::application(pid);
            let _ = app.set_messaging_timeout(1.5);
            // Prefer the app's own notion of the focused window.
            let focused = app
                .attribute(&AXAttribute::<CFType>::new(&CFString::new(
                    "AXFocusedWindow",
                )))
                .ok()
                .and_then(|v| v.downcast::<AXUIElement>());
            if let Some(el) = focused {
                return Ok((pid, el.clone()));
            }
            ax_windows_of(pid)
                .into_iter()
                .next()
                .map(|el| (pid, el))
                .ok_or_else(|| DriverError::NotFound(format!("app pid {pid} has no AX windows")))
        }
    }
}

fn set_ax_value(el: &AXUIElement, name: &str, ty: i32, data: &[f64; 2]) -> Result<(), DriverError> {
    let v = unsafe { ffi::AXValueCreate(ty, data.as_ptr() as *const std::ffi::c_void) };
    if v.is_null() {
        return Err(DriverError::Platform(format!("AXValueCreate {name} null")));
    }
    let cf = unsafe { CFType::wrap_under_create_rule(v) };
    el.set_attribute(&AXAttribute::<CFType>::new(&CFString::new(name)), cf)
        .map_err(|e| DriverError::Platform(format!("{name} set failed: {e:?}")))
}

/// Perform a window operation on a resolved AX window.
pub fn window_op(
    pid: i32,
    win: &AXUIElement,
    operation: &dexter_core::WindowOperation,
) -> Result<String, DriverError> {
    use dexter_core::WindowOperation as Op;
    match operation {
        Op::New => Err(DriverError::Unsupported(
            "no generic new-window verb on macOS — use the app's own control".into(),
        )),
        Op::Focus | Op::Raise => {
            win.perform_action(&CFString::new("AXRaise"))
                .map_err(|e| DriverError::Platform(format!("AXRaise failed: {e:?}")))?;
            if *operation == Op::Focus {
                let _ = crate::apps::activate_pid(pid);
            }
            Ok("raised".into())
        }
        Op::Close => {
            // The close button is the honest path — it honors app-level
            // "keep running with no windows" semantics.
            let btn = win
                .attribute(&AXAttribute::<CFType>::new(&CFString::new("AXCloseButton")))
                .ok()
                .and_then(|v| v.downcast::<AXUIElement>())
                .ok_or_else(|| DriverError::Unsupported("window has no AXCloseButton".into()))?;
            btn.perform_action(&CFString::new("AXPress"))
                .map_err(|e| DriverError::Platform(format!("close press failed: {e:?}")))?;
            Ok("closed".into())
        }
        Op::Minimize | Op::Restore => {
            let target = matches!(operation, Op::Minimize);
            win.set_attribute(
                &AXAttribute::<CFType>::new(&CFString::new("AXMinimized")),
                core_foundation::boolean::CFBoolean::from(target).as_CFType(),
            )
            .map_err(|e| DriverError::Platform(format!("AXMinimized set failed: {e:?}")))?;
            Ok(if target { "minimized" } else { "restored" }.into())
        }
        Op::Move { x, y } => {
            set_ax_value(win, "AXPosition", ffi::K_AX_VALUE_CG_POINT_TYPE, &[*x, *y])?;
            Ok(format!("moved to ({x},{y})"))
        }
        Op::Resize { width, height } => {
            set_ax_value(
                win,
                "AXSize",
                ffi::K_AX_VALUE_CG_SIZE_TYPE,
                &[*width, *height],
            )?;
            Ok(format!("resized to ({width}x{height})"))
        }
    }
}

// ---------------------------------------------------------------------------
// Menu shortcuts — the semantic route for Key chords.
// ---------------------------------------------------------------------------

/// One menu item's shortcut claim: the element plus the chord it
/// advertises and whether it's enabled.
pub struct MenuShortcut {
    pub el: AXUIElement,
    pub chord: KeyChord,
    pub enabled: bool,
}

fn read_attr_string(el: &AXUIElement, name: &str) -> Option<String> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let v: CFType = el.attribute(&attr).ok()?;
    v.downcast::<CFString>().map(|s| s.to_string())
}

fn read_attr_i64(el: &AXUIElement, name: &str) -> Option<i64> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let v: CFType = el.attribute(&attr).ok()?;
    v.downcast::<core_foundation::number::CFNumber>()
        .and_then(|n| n.to_i64())
}

fn chord_advertised(el: &AXUIElement) -> Option<KeyChord> {
    // CmdVirtualKey handles F-keys/arrows where no char exists — v2
    // handles char-based shortcuts; virtual-key items return None.
    let cmd_char = read_attr_string(el, "AXMenuItemCmdChar")?;
    if cmd_char.is_empty() {
        return None;
    }
    let mods = read_attr_i64(el, "AXMenuItemCmdModifiers").unwrap_or(0);
    Some(KeyChord {
        key: cmd_char.to_lowercase(),
        modifiers: menu_modifiers(mods),
    })
}

fn collect_menu_items(el: &AXUIElement, depth: u32, out: &mut Vec<MenuShortcut>) {
    if depth > 8 || out.len() > 512 {
        return;
    }
    let role = el.role().ok().map(|r| r.to_string());
    if role.as_deref() == Some("AXMenuItem") {
        if let Some(chord) = chord_advertised(el) {
            let enabled = el
                .enabled()
                .ok()
                .map(|b| b == core_foundation::boolean::CFBoolean::true_value())
                .unwrap_or(true);
            out.push(MenuShortcut {
                el: el.clone(),
                chord,
                enabled,
            });
        }
    }
    if let Ok(children) = el.children() {
        for c in children.iter() {
            collect_menu_items(&c, depth + 1, out);
        }
    }
}

/// Walk the app's menu bar for items advertising a keyboard shortcut.
fn menu_shortcuts(pid: i32) -> Vec<MenuShortcut> {
    let app = AXUIElement::application(pid);
    let _ = app.set_messaging_timeout(1.5);
    let bar = app
        .attribute(&AXAttribute::<CFType>::new(&CFString::new("AXMenuBar")))
        .ok()
        .and_then(|v| v.downcast::<AXUIElement>());
    let mut out = Vec::new();
    if let Some(bar) = bar {
        collect_menu_items(&bar, 0, &mut out);
    }
    out
}

/// The menu item uniquely claiming `chord` — `Ok(None)` when no item
/// matches, `Err(Ambiguous)` when more than one enabled item does.
/// Disabled items don't count toward ambiguity but can't be pressed.
pub fn menu_item_for_chord(pid: i32, chord: &KeyChord) -> Result<Option<AXUIElement>, DriverError> {
    let matches: Vec<MenuShortcut> = menu_shortcuts(pid)
        .into_iter()
        .filter(|m| {
            menu_key_matches(&m.chord.key, &chord.key)
                && same_modifiers(&m.chord.modifiers, &chord.modifiers)
        })
        .collect();
    let enabled: Vec<&MenuShortcut> = matches.iter().filter(|m| m.enabled).collect();
    match enabled.len() {
        0 => Ok(None),
        1 => Ok(Some(enabled[0].el.clone())),
        n => Err(DriverError::Ambiguous(format!(
            "{n} enabled menu items advertise the same shortcut — refusing to pick one"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Physical input helpers — multi-click and drag.
// ---------------------------------------------------------------------------

/// Post `count` clicks at (x,y) with `kCGMouseEventClickState` set —
/// real double/triple clicks, not repeated singles.
pub fn cg_multi_click(x: f64, y: f64, button: MouseButton, count: u8) -> Result<(), DriverError> {
    let (down, up, btn) = match button {
        MouseButton::Left => (
            ffi::K_CG_EVENT_LEFT_DOWN,
            ffi::K_CG_EVENT_LEFT_UP,
            ffi::K_CG_MOUSE_LEFT,
        ),
        MouseButton::Right => (
            ffi::K_CG_EVENT_RIGHT_DOWN,
            ffi::K_CG_EVENT_RIGHT_UP,
            ffi::K_CG_MOUSE_RIGHT,
        ),
        MouseButton::Middle => (
            ffi::K_CG_EVENT_MIDDLE_DOWN,
            ffi::K_CG_EVENT_MIDDLE_UP,
            ffi::K_CG_MOUSE_MIDDLE,
        ),
    };
    let p = ffi::CGPoint { x, y };
    for i in 1..=count as i64 {
        unsafe {
            let d = ffi::CGEventCreateMouseEvent(std::ptr::null(), down, p, btn);
            let u = ffi::CGEventCreateMouseEvent(std::ptr::null(), up, p, btn);
            if d.is_null() || u.is_null() {
                return Err(DriverError::Platform("CGEventCreateMouseEvent null".into()));
            }
            ffi::CGEventSetIntegerValueField(d, ffi::K_CG_MOUSE_EVENT_CLICK_STATE, i);
            ffi::CGEventSetIntegerValueField(u, ffi::K_CG_MOUSE_EVENT_CLICK_STATE, i);
            ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, d);
            ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, u);
            ffi::CFRelease(d);
            ffi::CFRelease(u);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(())
}

/// Drag from (x1,y1) to (x2,y2) over `duration_ms`. The button is always
/// released — an error mid-drag still posts the mouse-up so the pointer
/// is never left held.
pub fn cg_drag(x1: f64, y1: f64, x2: f64, y2: f64, duration_ms: u64) -> Result<(), DriverError> {
    fn post(ty: u32, x: f64, y: f64) -> Result<(), DriverError> {
        unsafe {
            let ev = ffi::CGEventCreateMouseEvent(
                std::ptr::null(),
                ty,
                ffi::CGPoint { x, y },
                ffi::K_CG_MOUSE_LEFT,
            );
            if ev.is_null() {
                return Err(DriverError::Platform("CGEventCreateMouseEvent null".into()));
            }
            ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, ev);
            ffi::CFRelease(ev);
        }
        Ok(())
    }
    post(ffi::K_CG_EVENT_LEFT_DOWN, x1, y1)?;
    // Interpolate the path — apps that hit-test during drag need
    // intermediate positions, not a teleport.
    const STEPS: u64 = 8;
    let step_ms = (duration_ms / STEPS).max(10);
    let mut result = Ok(());
    for i in 1..STEPS {
        let t = i as f64 / STEPS as f64;
        if let Err(e) = post(
            ffi::K_CG_EVENT_LEFT_DRAGGED,
            x1 + (x2 - x1) * t,
            y1 + (y2 - y1) * t,
        ) {
            result = Err(e);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(step_ms));
    }
    // Always release — even on error.
    let up = post(ffi::K_CG_EVENT_LEFT_UP, x2, y2);
    result.and(up)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ax_action_for_matches_normalized_names() {
        let advertised = vec![
            "AXPress".to_string(),
            "AXShowMenu".to_string(),
            "AXOpen".to_string(),
        ];
        assert_eq!(ax_action_for("press", &advertised), Some("AXPress"));
        assert_eq!(ax_action_for("show_menu", &advertised), Some("AXShowMenu"));
        assert_eq!(ax_action_for("open", &advertised), Some("AXOpen"));
        // Unadvertised names are refused — never passed to the platform.
        assert_eq!(ax_action_for("delete", &advertised), None);
        assert_eq!(ax_action_for("open", &[]), None);
    }

    #[test]
    fn menu_modifiers_decodes_the_bitmask() {
        // 0 = cmd only; bit3 = no cmd.
        assert_eq!(menu_modifiers(0), vec!["cmd"]);
        assert_eq!(menu_modifiers(1), vec!["cmd", "shift"]);
        assert_eq!(menu_modifiers(3), vec!["cmd", "shift", "alt"]);
        assert_eq!(menu_modifiers(8), Vec::<String>::new());
        assert_eq!(menu_modifiers(9), vec!["shift"]);
    }

    #[test]
    fn menu_key_matches_named_and_literal_keys() {
        assert!(menu_key_matches("s", "s"));
        assert!(menu_key_matches("S", "s")); // cmd char uppercases under shift
        assert!(menu_key_matches("\r", "return"));
        assert!(menu_key_matches("\r", "enter"));
        assert!(menu_key_matches("\u{1b}", "escape"));
        assert!(menu_key_matches("\u{1b}", "esc"));
        assert!(!menu_key_matches("s", "t"));
    }

    #[test]
    fn same_modifiers_is_order_and_alias_insensitive() {
        let a = vec!["cmd".to_string(), "shift".to_string()];
        let b = vec!["shift".to_string(), "command".to_string()];
        assert!(same_modifiers(&a, &b));
        let c = vec!["cmd".to_string()];
        assert!(!same_modifiers(&a, &c));
    }

    #[test]
    fn center_of_bounds() {
        let b = Rect {
            x: 10.0,
            y: 20.0,
            w: 100.0,
            h: 50.0,
        };
        assert_eq!(center(&b), (60.0, 45.0));
    }
}
