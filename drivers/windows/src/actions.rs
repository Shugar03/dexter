//! Action execution on Windows: semantic-first UIA patterns, `SendInput`
//! only where no pattern reaches — and physical input is always gated by
//! `ctx.allow_coordinates` plus a foreground check where keystrokes would
//! otherwise land in another app.
//!
//! `Target::Element` is never acted on through a stored pointer: the cache
//! keeps only the *data* of past observations; `act` re-walks the live UIA
//! tree for the same app/window scope and verifies the element at the same
//! index still matches (role, name, parent, depth, bounds ±2px). Anything
//! else is a stale reference — fail closed.

use dexter_core::{
    Action, ActionResult, ActionStatus, Element, ElementSource, KeyChord, Mechanism, MouseButton,
    Observation, ObservationId, ScrollDelta, Target,
};
use dexter_driver::{utf16_chunks, ActContext, DriverError};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;
use windows::core::{BSTR, HSTRING};
use windows::Win32::UI::Accessibility::{
    ExpandCollapseState_Expanded, IUIAutomationElement, IUIAutomationExpandCollapsePattern,
    IUIAutomationInvokePattern, IUIAutomationLegacyIAccessiblePattern,
    IUIAutomationScrollItemPattern, IUIAutomationScrollPattern, IUIAutomationSelectionItemPattern,
    IUIAutomationTogglePattern, IUIAutomationValuePattern, ScrollAmount,
    ScrollAmount_LargeDecrement, ScrollAmount_LargeIncrement, ScrollAmount_NoAmount,
    ScrollAmount_SmallDecrement, ScrollAmount_SmallIncrement, UIA_ExpandCollapsePatternId,
    UIA_InvokePatternId, UIA_LegacyIAccessiblePatternId, UIA_ScrollItemPatternId,
    UIA_ScrollPatternId, UIA_SelectionItemPatternId, UIA_TogglePatternId, UIA_ValuePatternId,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowThreadProcessId, IsWindow, SetCursorPos, SetForegroundWindow,
    SW_SHOWNORMAL,
};

use crate::msaa;
use crate::resolve::{self, PressPattern};
use crate::uia::{self, has_pattern, LiveNode, Uia};

/// Re-walk bounds when an action resolves a target that was never
/// bound by an observation (semantic, focused). `Target::Element`
/// re-walks at the bounds of the observation that bound it instead.
const ACTION_WALK_DEPTH: u32 = 40;
const ACTION_WALK_MAX: usize = 4_000;

/// A live element resolved for an action, with a display detail. The
/// node is whatever produced it — a UIA element or an MSAA
/// `(IAccessible, child)` pair — and each act arm handles both.
struct Resolved {
    node: LiveNode,
    detail: String,
}

#[derive(Debug)]
struct ObsEntry {
    pid: i32,
    window: Option<u32>,
    max_depth: u32,
    max_elements: usize,
    elements: Vec<Element>,
}

/// Cache of past observation *data* (never `IUIAutomationElement` COM
/// pointers — `WindowsDriver` stays `Send + Sync` and COM is initialized
/// per call). Bounded to the last few observations.
#[derive(Debug)]
pub struct ObsCache {
    inner: Mutex<VecDeque<(ObservationId, ObsEntry)>>,
}

impl ObsCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
        }
    }

    pub fn store(
        &self,
        obs: ObservationId,
        pid: i32,
        window: Option<u32>,
        max_depth: u32,
        max_elements: usize,
        elements: Vec<Element>,
    ) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|(id, _)| *id != obs);
        g.push_front((
            obs,
            ObsEntry {
                pid,
                window,
                max_depth,
                max_elements,
                elements,
            },
        ));
        g.truncate(4);
    }

    fn get(&self, obs: ObservationId) -> Option<ObsEntry> {
        let g = self.inner.lock().unwrap();
        g.iter().find(|(id, _)| *id == obs).map(|(_, e)| ObsEntry {
            pid: e.pid,
            window: e.window,
            max_depth: e.max_depth,
            max_elements: e.max_elements,
            elements: e.elements.clone(),
        })
    }
}

impl Default for ObsCache {
    fn default() -> Self {
        Self::new()
    }
}

fn describe(el: &IUIAutomationElement) -> String {
    let control = unsafe { el.CurrentControlType() }.ok().map(|c| c.0);
    // Prefer the normalized role for a cross-platform detail; the raw
    // ControlType name is the fallback so unknown types still describe.
    let role = control
        .and_then(crate::control_type_name)
        .and_then(crate::uia_role)
        .or_else(|| control.and_then(crate::control_type_name));
    let name = unsafe { el.CurrentName() }
        .ok()
        .map(|b| String::from_utf16_lossy(&b))
        .filter(|s| !s.is_empty());
    match (role, name) {
        (Some(r), Some(n)) => format!("{r} \"{n}\""),
        (Some(r), None) => r.to_string(),
        _ => "element".into(),
    }
}

fn resolve_app(ctx: &ActContext) -> Result<i32, DriverError> {
    let sel = ctx
        .app
        .clone()
        .ok_or_else(|| DriverError::NotFound("this target requires an app scope".into()))?;
    crate::apps::resolve_pid(&sel)
}

/// Fresh walk of `pid`'s windows (or one window's subtree) into
/// normalized elements + live handles. Same scope rules as
/// `uia::observe`: `window` must still be a top-level window of `pid` —
/// a window that vanished is staleness, not an empty result.
fn walk_live(
    uia: &Uia,
    pid: i32,
    window: Option<u32>,
    max_depth: u32,
    max_elements: usize,
) -> Result<uia::LiveWalk, DriverError> {
    let hwnds = crate::win::hwnds();
    // Roots are `(HWND, element)` pairs — the HWND travels with its
    // root so `walk_roots` can run the same per-window MSAA pass
    // observe ran, keeping merged element order (and ids) identical.
    let mut roots: Vec<(
        windows::Win32::Foundation::HWND,
        Option<IUIAutomationElement>,
    )> = Vec::new();
    match window {
        Some(wid) => {
            let hwnd = crate::win::hwnd_of(wid);
            if !hwnds.iter().any(|(h, p)| *h == hwnd && *p == pid) {
                return Err(DriverError::StaleReference(format!(
                    "window {wid} is no longer a top-level window of pid {pid} — re-observe"
                )));
            }
            // A window UIA can't anchor still enters the walk — its
            // MSAA pass may be the only tree, exactly as in observe.
            let root = uia.element_from(hwnd).ok().map(|e| uia.normalize(e));
            roots.push((hwnd, root));
        }
        None => {
            for (hwnd, _) in hwnds.into_iter().filter(|(_, p)| *p == pid) {
                // A window closed mid-resolution yields no root — the
                // identity check downstream treats it as staleness.
                let root = uia.element_from(hwnd).ok().map(|e| uia.normalize(e));
                roots.push((hwnd, root));
            }
        }
    }
    Ok(uia::walk_roots(uia, &roots, max_depth, max_elements))
}

/// Resolve a target to a live element. `Semantic` and `Element` targets
/// both trigger a fresh walk — never trust a stored snapshot for actions.
fn resolve_element(
    target: &Target,
    ctx: &ActContext,
    cache: &ObsCache,
    uia: &Uia,
) -> Result<Resolved, DriverError> {
    match target {
        Target::Element {
            observation,
            element,
        } => {
            let entry = cache.get(*observation).ok_or_else(|| {
                DriverError::StaleReference(format!(
                    "observation {} is no longer held — re-observe",
                    observation.0
                ))
            })?;
            let idx = element.0 as usize - 1;
            let stored = entry.elements.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference(format!(
                    "element {} out of range in observation {}",
                    element.0, observation.0
                ))
            })?;
            // OCR elements are evidence, not live handles — there is no
            // UIA node to re-resolve (same rule as the AX slice).
            if stored.source == ElementSource::Ocr {
                return Err(DriverError::StaleReference(format!(
                    "element {} is OCR-derived — target its bounds center \
                     as a Point instead",
                    element.0
                )));
            }
            let tree = walk_live(
                uia,
                entry.pid,
                entry.window,
                entry.max_depth,
                entry.max_elements,
            )?;
            resolve::fresh_at_stored_index(&stored, &tree.elements).ok_or_else(|| {
                DriverError::StaleReference(format!(
                    "element {} changed since observation {} — re-observe",
                    element.0, observation.0
                ))
            })?;
            let node = tree.nodes.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference("resolved element vanished mid-walk".into())
            })?;
            Ok(Resolved {
                detail: describe_node(&node),
                node,
            })
        }
        Target::Semantic(_) => {
            let pid = resolve_app(ctx)?;
            let tree = walk_live(uia, pid, None, ACTION_WALK_DEPTH, ACTION_WALK_MAX)?;
            let obs = Observation {
                elements: tree.elements,
                elements_truncated: tree.truncated,
                ..Default::default()
            };
            let found = dexter_world_model::resolve_element(&obs, target)
                .map_err(|e| resolve::resolve_error(e, obs.elements_truncated))?;
            let idx = found.id.0 as usize - 1;
            let node = tree.nodes.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference("resolved element vanished mid-walk".into())
            })?;
            Ok(Resolved {
                detail: describe_node(&node),
                node,
            })
        }
        Target::Focused => {
            let pid = resolve_app(ctx)?;
            let el = uia.focused()?;
            let el_pid = unsafe { el.CurrentProcessId() }.unwrap_or(0);
            if el_pid != pid {
                return Err(DriverError::NotFound(format!(
                    "focused element belongs to pid {el_pid}, not {pid}"
                )));
            }
            Ok(Resolved {
                detail: describe(&el),
                node: LiveNode::Uia(el),
            })
        }
        Target::Point { .. } | Target::Window { .. } => {
            Err(DriverError::Unsupported("target is not an element".into()))
        }
    }
}

fn describe_node(node: &LiveNode) -> String {
    match node {
        LiveNode::Uia(el) => describe(el),
        LiveNode::Msaa(m) => msaa::describe(m),
    }
}

/// A resolved element must be enabled for a mutation to mean anything —
/// UIA patterns on disabled controls are no-ops, and success on a no-op
/// is simulated success. Unreadable state is not invented as disabled:
/// the pattern call itself reports the failure. (The browser driver's
/// disabled guard, same rule.) The MSAA arm reads `accState` for
/// `STATE_SYSTEM_UNAVAILABLE` instead of `CurrentIsEnabled`.
fn disabled_verdict(r: &Resolved) -> Option<ActionResult> {
    let enabled = match &r.node {
        LiveNode::Uia(el) => unsafe { el.CurrentIsEnabled() }
            .map(|b| b.as_bool())
            .unwrap_or(true),
        LiveNode::Msaa(m) => msaa::state_of(m)
            .map(|st| st & msaa::STATE_SYSTEM_UNAVAILABLE == 0)
            .unwrap_or(true),
    };
    if enabled {
        return None;
    }
    Some(ActionResult::failure(
        ActionStatus::Failed,
        Mechanism::Accessibility,
        format!("{} is disabled", r.detail),
    ))
}

fn has(el: &IUIAutomationElement, pat: PressPattern) -> bool {
    match pat {
        PressPattern::Invoke => {
            has_pattern::<IUIAutomationInvokePattern>(el, UIA_InvokePatternId).is_some()
        }
        PressPattern::Toggle => {
            has_pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId).is_some()
        }
        PressPattern::SelectItem => {
            has_pattern::<IUIAutomationSelectionItemPattern>(el, UIA_SelectionItemPatternId)
                .is_some()
        }
        PressPattern::ExpandCollapse => {
            has_pattern::<IUIAutomationExpandCollapsePattern>(el, UIA_ExpandCollapsePatternId)
                .is_some()
        }
        PressPattern::LegacyIAccessible => {
            has_pattern::<IUIAutomationLegacyIAccessiblePattern>(el, UIA_LegacyIAccessiblePatternId)
                .is_some()
        }
    }
}

/// MSAA left click: `accDoDefaultAction` is the only activation MSAA
/// has — a control with no default action is honestly UNSUPPORTED,
/// and a failed call is a platform error, never a pretend press.
fn press_msaa(m: &msaa::MsaaRef, detail: &str) -> Result<ActionResult, DriverError> {
    let var = msaa::var_child(m.child);
    let has_default = unsafe { m.acc.get_accDefaultAction(&var) }
        .ok()
        .map(|b| !String::from_utf16_lossy(&b).is_empty())
        .unwrap_or(false);
    if !has_default {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            format!("no default action on {detail}"),
        ));
    }
    unsafe { m.acc.accDoDefaultAction(&var) }
        .map_err(|e| DriverError::Platform(format!("accDoDefaultAction: {e}")))?;
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("default action on {detail}")),
    ))
}

/// Left click: the press-pattern ladder, first present wins. The ladder
/// is the `PRESS_ORDER` contract — probed per element, never assumed.
/// An MSAA node skips the ladder entirely: `accDoDefaultAction` is its
/// one verb.
fn press(r: &Resolved) -> Result<ActionResult, DriverError> {
    let el = match &r.node {
        LiveNode::Uia(el) => el,
        LiveNode::Msaa(m) => return press_msaa(m, &r.detail),
    };
    let Some(pat) = resolve::first_press_pattern(|p| has(el, p)) else {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            format!("no activation pattern on {}", r.detail),
        ));
    };
    let detail = match pat {
        PressPattern::Invoke => {
            let p = has_pattern::<IUIAutomationInvokePattern>(el, UIA_InvokePatternId)
                .ok_or_else(|| DriverError::Platform("Invoke pattern raced away".into()))?;
            unsafe { p.Invoke() }
                .map_err(|e| DriverError::Platform(format!("InvokePattern.Invoke: {e}")))?;
            format!("pressed {}", r.detail)
        }
        PressPattern::Toggle => {
            let p = has_pattern::<IUIAutomationTogglePattern>(el, UIA_TogglePatternId)
                .ok_or_else(|| DriverError::Platform("Toggle pattern raced away".into()))?;
            unsafe { p.Toggle() }
                .map_err(|e| DriverError::Platform(format!("TogglePattern.Toggle: {e}")))?;
            format!("toggled {}", r.detail)
        }
        PressPattern::SelectItem => {
            let p =
                has_pattern::<IUIAutomationSelectionItemPattern>(el, UIA_SelectionItemPatternId)
                    .ok_or_else(|| {
                        DriverError::Platform("SelectionItem pattern raced away".into())
                    })?;
            unsafe { p.Select() }
                .map_err(|e| DriverError::Platform(format!("SelectionItemPattern.Select: {e}")))?;
            format!("selected {}", r.detail)
        }
        PressPattern::ExpandCollapse => {
            let p =
                has_pattern::<IUIAutomationExpandCollapsePattern>(el, UIA_ExpandCollapsePatternId)
                    .ok_or_else(|| {
                        DriverError::Platform("ExpandCollapse pattern raced away".into())
                    })?;
            let expanded = unsafe { p.CurrentExpandCollapseState() }
                .map(|s| s == ExpandCollapseState_Expanded)
                .unwrap_or(false);
            let call = if expanded {
                unsafe { p.Collapse() }
            } else {
                unsafe { p.Expand() }
            };
            call.map_err(|e| DriverError::Platform(format!("ExpandCollapsePattern: {e}")))?;
            let verb = if expanded { "collapsed" } else { "expanded" };
            format!("{verb} {}", r.detail)
        }
        PressPattern::LegacyIAccessible => {
            let p = has_pattern::<IUIAutomationLegacyIAccessiblePattern>(
                el,
                UIA_LegacyIAccessiblePatternId,
            )
            .ok_or_else(|| DriverError::Platform("LegacyIAccessible pattern raced away".into()))?;
            unsafe { p.DoDefaultAction() }.map_err(|e| {
                DriverError::Platform(format!("LegacyIAccessible.DoDefaultAction: {e}"))
            })?;
            format!("default action on {}", r.detail)
        }
    };
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(detail),
    ))
}

/// The element's `ValuePattern` plus its read-only flag, when present.
fn value_pattern(el: &IUIAutomationElement) -> Option<(IUIAutomationValuePattern, bool)> {
    let v = has_pattern::<IUIAutomationValuePattern>(el, UIA_ValuePatternId)?;
    let read_only = unsafe { v.CurrentIsReadOnly() }
        .map(|b| b.as_bool())
        .unwrap_or(true);
    Some((v, read_only))
}

/// MSAA `put_accValue` — the semantic write. `READONLY` state gates
/// it (PROTECTED masks reads, not writes — a password edit accepts a
/// value set); an `Err` from the call itself is a platform failure.
fn set_value_msaa(
    m: &msaa::MsaaRef,
    detail: &str,
    value: &str,
) -> Result<ActionResult, DriverError> {
    let st = msaa::state_of(m).unwrap_or(0);
    if st & msaa::STATE_SYSTEM_READONLY != 0 {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            format!("{detail} is read-only"),
        ));
    }
    msaa::try_put_value(m, value)?;
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("set value on {detail}")),
    ))
}

fn set_value(r: &Resolved, value: &str) -> Result<ActionResult, DriverError> {
    match &r.node {
        LiveNode::Uia(el) => match value_pattern(el) {
            Some((v, false)) => {
                unsafe { v.SetValue(&BSTR::from(value)) }
                    .map_err(|e| DriverError::Platform(format!("ValuePattern.SetValue: {e}")))?;
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("set value on {}", r.detail)),
                ))
            }
            Some((_, true)) => Ok(ActionResult::failure(
                ActionStatus::Failed,
                Mechanism::Accessibility,
                format!("{} is read-only", r.detail),
            )),
            None => Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                format!("no writable Value pattern on {}", r.detail),
            )),
        },
        LiveNode::Msaa(m) => set_value_msaa(m, &r.detail, value),
    }
}

/// Pid owning the foreground window — keystrokes go wherever that is.
fn foreground_pid() -> Option<i32> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid as i32)
}

fn pid_is_foreground(pid: i32) -> bool {
    foreground_pid() == Some(pid)
}

/// One `SendInput` batch; fewer events inserted than given is the honest
/// platform failure.
fn send_input(inputs: &[INPUT]) -> Result<(), DriverError> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize != inputs.len() {
        return Err(DriverError::Platform(format!(
            "SendInput inserted {sent}/{} events",
            inputs.len()
        )));
    }
    Ok(())
}

fn key_input(vk: u16, up: bool) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(vk),
                wScan: 0,
                dwFlags: if up {
                    KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Physical click: `SetCursorPos` takes real pixels (SendInput's own
/// absolute-move space is normalized virtual-screen), then button events.
fn mouse_click(x: f64, y: f64, button: MouseButton) -> Result<(), DriverError> {
    unsafe { SetCursorPos(x as i32, y as i32) }
        .map_err(|e| DriverError::Platform(format!("SetCursorPos refused the move: {e}")))?;
    let (down, up) = match button {
        MouseButton::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
        MouseButton::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
        MouseButton::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
    };
    send_input(&[mouse_input(down), mouse_input(up)])
}

/// Element center in screen coordinates — the only point a pointer click
/// on an element can honestly land at. `None` when UIA reports no bounds.
fn element_center(el: &IUIAutomationElement) -> Option<(f64, f64)> {
    let mut failed = false;
    let c = uia::bounds_of(el, &mut failed)?.center();
    Some((c.x, c.y))
}

/// Center of a resolved node — `accLocation` for MSAA nodes, UIA
/// bounds for UIA nodes. Same `None`-means-no-bounds rule.
fn resolved_center(r: &Resolved) -> Option<(f64, f64)> {
    match &r.node {
        LiveNode::Uia(el) => element_center(el),
        LiveNode::Msaa(m) => msaa::bounds_of(m).map(|b| {
            let c = b.center();
            (c.x, c.y)
        }),
    }
}

/// `SendInput` unicode keystrokes — one down/up pair per UTF-16 unit so
/// surrogate pairs post whole chars. Long text batches so a single call
/// never overruns the input queue.
fn send_text(text: &str) -> Result<usize, DriverError> {
    let mut count = 0usize;
    for chunk in utf16_chunks(text, 200) {
        let mut inputs = Vec::with_capacity(chunk.len() * 2);
        for unit in chunk {
            for up in [false, true] {
                inputs.push(INPUT {
                    r#type: INPUT_KEYBOARD,
                    Anonymous: INPUT_0 {
                        ki: KEYBDINPUT {
                            wVk: VIRTUAL_KEY(0),
                            wScan: unit,
                            dwFlags: if up {
                                KEYEVENTF_UNICODE | KEYEVENTF_KEYUP
                            } else {
                                KEYEVENTF_UNICODE
                            },
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                });
            }
            count += 1;
        }
        send_input(&inputs)?;
        std::thread::sleep(Duration::from_millis(15));
    }
    Ok(count)
}

fn send_chord(chord: &KeyChord) -> Result<ActionResult, DriverError> {
    let key = crate::keymap::vk_of(&chord.key)
        .ok_or_else(|| DriverError::Unsupported(format!("unknown key '{}'", chord.key)))?;
    let mut mods = Vec::new();
    for m in &chord.modifiers {
        mods.push(
            crate::keymap::modifier_vk(m)
                .ok_or_else(|| DriverError::Unsupported(format!("unknown modifier '{m}'")))?,
        );
    }
    let mut inputs = Vec::with_capacity((mods.len() + 1) * 2);
    for &m in &mods {
        inputs.push(key_input(m, false));
    }
    inputs.push(key_input(key, false));
    inputs.push(key_input(key, true));
    for &m in mods.iter().rev() {
        inputs.push(key_input(m, true));
    }
    send_input(&inputs)?;
    let mut parts = chord.modifiers.clone();
    parts.push(chord.key.clone());
    Ok(ActionResult::success(
        Mechanism::Coordinates,
        Some(format!("posted chord {}", parts.join("+"))),
    ))
}

/// Wheel scroll with no element target. `dy > 0` scrolls content down;
/// one wheel notch is `WHEEL_DELTA` (120) of `mouseData`, negative means
/// down — 40px per line keeps the same scale the CGEvent path uses.
fn send_wheel(dx: f64, dy: f64) -> Result<(), DriverError> {
    let mut inputs = Vec::new();
    let vnotches = (dy / 40.0).round() as i32;
    if vnotches != 0 {
        let mut mi = mouse_input(MOUSEEVENTF_WHEEL);
        mi.Anonymous.mi.mouseData = (-vnotches * 120) as u32;
        inputs.push(mi);
    }
    let hnotches = (dx / 40.0).round() as i32;
    if hnotches != 0 {
        let mut mi = mouse_input(MOUSEEVENTF_HWHEEL);
        mi.Anonymous.mi.mouseData = (hnotches * 120) as u32;
        inputs.push(mi);
    }
    // A ~zero delta produces no events — nothing happened, nothing to
    // report beyond an honest success of doing nothing.
    if inputs.is_empty() {
        return Ok(());
    }
    send_input(&inputs)
}

/// Scroll deltas → `ScrollPattern` increments: positive dy scrolls
/// content down (toward the end), large deltas move a page at a time.
fn scroll_amounts(d: &ScrollDelta) -> (ScrollAmount, ScrollAmount) {
    fn amount(v: f64) -> ScrollAmount {
        if v.abs() < 0.5 {
            ScrollAmount_NoAmount
        } else if v > 0.0 {
            if v >= 80.0 {
                ScrollAmount_LargeIncrement
            } else {
                ScrollAmount_SmallIncrement
            }
        } else if v <= -80.0 {
            ScrollAmount_LargeDecrement
        } else {
            ScrollAmount_SmallDecrement
        }
    }
    (amount(d.dx), amount(d.dy))
}

fn scroll_element(r: &Resolved, delta: &ScrollDelta) -> Result<ActionResult, DriverError> {
    let el = match &r.node {
        LiveNode::Uia(el) => el,
        // MSAA has no scroll contract — unsupported, not simulated.
        LiveNode::Msaa(_) => {
            return Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                format!("no scroll semantics on {}", r.detail),
            ))
        }
    };
    // Bringing the target into view is the semantic scroll — mirrors
    // AXScrollToVisible; the delta is irrelevant to it.
    if let Some(p) = has_pattern::<IUIAutomationScrollItemPattern>(el, UIA_ScrollItemPatternId) {
        unsafe { p.ScrollIntoView() }
            .map_err(|e| DriverError::Platform(format!("ScrollItemPattern.ScrollIntoView: {e}")))?;
        return Ok(ActionResult::success(
            Mechanism::Accessibility,
            Some(format!("scrolled {} into view", r.detail)),
        ));
    }
    if let Some(p) = has_pattern::<IUIAutomationScrollPattern>(el, UIA_ScrollPatternId) {
        let (h, v) = scroll_amounts(delta);
        unsafe { p.Scroll(h, v) }
            .map_err(|e| DriverError::Platform(format!("ScrollPattern.Scroll: {e}")))?;
        return Ok(ActionResult::success(
            Mechanism::Accessibility,
            Some(format!("scrolled {} ({},{})", r.detail, delta.dx, delta.dy)),
        ));
    }
    Ok(ActionResult::failure(
        ActionStatus::Unsupported,
        Mechanism::Accessibility,
        format!("no scroll pattern on {}", r.detail),
    ))
}

/// `Target::Window` focus = `SetForegroundWindow`. Scoped to ctx.app when
/// given — raising a foreign window is not what the caller asked for.
fn focus_window(window_id: u32, ctx: &ActContext) -> Result<ActionResult, DriverError> {
    let hwnd = crate::win::hwnd_of(window_id);
    if !unsafe { IsWindow(Some(hwnd)) }.as_bool() {
        return Err(DriverError::NotFound(format!(
            "window {window_id} is not a window"
        )));
    }
    if let Some(sel) = &ctx.app {
        let pid = crate::apps::resolve_pid(sel)?;
        let mut wpid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut wpid)) };
        if wpid as i32 != pid {
            return Err(DriverError::NotFound(format!(
                "window {window_id} belongs to pid {}, not {pid}",
                wpid as i32
            )));
        }
    }
    // A FALSE return is Windows refusing the raise (locked session,
    // another app holding the foreground) — the verdict is a failed
    // result, not an exception.
    if !unsafe { SetForegroundWindow(hwnd) }.as_bool() {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::NativeAutomation,
            format!("SetForegroundWindow refused window {window_id}"),
        ));
    }
    Ok(ActionResult::success(
        Mechanism::NativeAutomation,
        Some(format!("raised window {window_id}")),
    ))
}

/// `ShellExecuteW("open", url)` restricted to document schemes — `open`
/// on a path or exotic scheme executes programs, so `navigable_url` is a
/// closed allowlist, not a heuristic.
fn navigate(url: &str) -> Result<ActionResult, DriverError> {
    if !resolve::navigable_url(url) {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::NativeAutomation,
            "refusing to open non-URL input — only http/https/mailto reach ShellExecuteW"
                .to_string(),
        ));
    }
    // The return is the instance handle on success (> 32), else an error
    // code — that integer is the honest verdict.
    let hinst = unsafe {
        ShellExecuteW(
            None,
            &HSTRING::from("open"),
            &HSTRING::from(url),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if hinst.0 as usize > 32 {
        Ok(ActionResult::success(
            Mechanism::NativeAutomation,
            Some(format!("opened {url}")),
        ))
    } else {
        Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::NativeAutomation,
            format!(
                "ShellExecuteW open {url} failed (code {})",
                hinst.0 as usize
            ),
        ))
    }
}

pub fn act(
    action: &Action,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    // Wait/Observe/Navigate never touch UIA — resolve them before paying
    // for a COM session.
    match action {
        Action::Wait { millis } => {
            std::thread::sleep(Duration::from_millis(*millis));
            return Ok(ActionResult::success(
                Mechanism::NativeAutomation,
                Some(format!("waited {millis}ms")),
            ));
        }
        Action::Observe => {
            return Err(DriverError::Unsupported(
                "Action::Observe is an engine directive, not a driver action".into(),
            ));
        }
        Action::Navigate { url } => return navigate(url),
        _ => {}
    }

    let _serialize = uia::OBSERVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_guard, uia) = uia::connect()?;

    match action {
        Action::Click { target, button } => match target {
            Target::Point { x, y } => {
                if !ctx.allow_coordinates {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Coordinates,
                        "coordinate input disabled — pass the explicit coords flag",
                    ));
                }
                mouse_click(*x, *y, *button)?;
                Ok(ActionResult::success(
                    Mechanism::Coordinates,
                    Some(format!("clicked at ({x},{y})")),
                ))
            }
            Target::Window { .. } => Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "window targets not implemented — use semantic or element targets",
            )),
            _ => {
                let r = resolve_element(target, ctx, cache, &uia)?;
                if let Some(fail) = disabled_verdict(&r) {
                    return Ok(fail);
                }
                match button {
                    MouseButton::Left => press(&r),
                    _ => {
                        if !ctx.allow_coordinates {
                            // No pattern reaches a context menu —
                            // Unsupported, not a fake left-click.
                            return Ok(ActionResult::failure(
                                ActionStatus::Unsupported,
                                Mechanism::Accessibility,
                                "no UIA pattern reaches a context menu — enable coords for a pointer click",
                            ));
                        }
                        let Some((x, y)) = resolved_center(&r) else {
                            return Ok(ActionResult::failure(
                                ActionStatus::Unsupported,
                                Mechanism::Coordinates,
                                format!("{} reports no bounds to click", r.detail),
                            ));
                        };
                        mouse_click(x, y, *button)?;
                        Ok(ActionResult::success(
                            Mechanism::Coordinates,
                            Some(format!("clicked {button:?} on {}", r.detail)),
                        ))
                    }
                }
            }
        },
        Action::TypeText { text, target } => {
            let t = target.clone().unwrap_or(Target::Focused);
            let r = resolve_element(&t, ctx, cache, &uia)?;
            if let Some(fail) = disabled_verdict(&r) {
                return Ok(fail);
            }
            // Semantic first: a writable Value channel is a real set —
            // UIA's ValuePattern, or MSAA's put_accValue.
            match &r.node {
                LiveNode::Uia(el) => {
                    if let Some((v, read_only)) = value_pattern(el) {
                        if !read_only {
                            unsafe { v.SetValue(&BSTR::from(text)) }.map_err(|e| {
                                DriverError::Platform(format!("ValuePattern.SetValue: {e}"))
                            })?;
                            return Ok(ActionResult::success(
                                Mechanism::Accessibility,
                                Some(format!("set value on {}", r.detail)),
                            ));
                        }
                    }
                }
                LiveNode::Msaa(m) => {
                    let readonly = msaa::state_of(m)
                        .map(|st| st & msaa::STATE_SYSTEM_READONLY != 0)
                        .unwrap_or(false);
                    if readonly {
                        return Ok(ActionResult::failure(
                            ActionStatus::Failed,
                            Mechanism::Accessibility,
                            format!("{} is read-only", r.detail),
                        ));
                    }
                    // A rejected write means no value channel here —
                    // fall through to the keyboard path, same as a
                    // missing pattern.
                    if msaa::try_put_value(m, text).is_ok() {
                        return Ok(ActionResult::success(
                            Mechanism::Accessibility,
                            Some(format!("set value on {}", r.detail)),
                        ));
                    }
                }
            }
            // Physical typing is the coordinates path — opt-in only,
            // and only into the foreground app it would actually hit.
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Accessibility,
                    format!(
                        "no writable Value pattern on {} — enable coords for keyboard fallback",
                        r.detail
                    ),
                ));
            }
            let pid = match &r.node {
                LiveNode::Uia(el) => {
                    unsafe { el.SetFocus() }
                        .map_err(|e| DriverError::Platform(format!("SetFocus: {e}")))?;
                    let focused = unsafe { el.CurrentHasKeyboardFocus() }
                        .map(|b| b.as_bool())
                        .unwrap_or(false);
                    if !focused {
                        return Ok(ActionResult::failure(
                            ActionStatus::Failed,
                            Mechanism::Accessibility,
                            format!("{} would not take keyboard focus", r.detail),
                        ));
                    }
                    unsafe { el.CurrentProcessId() }.unwrap_or(0)
                }
                LiveNode::Msaa(m) => {
                    msaa::take_focus(m)?;
                    if !msaa::focused_of(m) {
                        return Ok(ActionResult::failure(
                            ActionStatus::Failed,
                            Mechanism::Accessibility,
                            format!("{} would not take keyboard focus", r.detail),
                        ));
                    }
                    msaa::pid_of(&m.acc).unwrap_or(0)
                }
            };
            if pid == 0 || !pid_is_foreground(pid) {
                return Ok(ActionResult::failure(
                    ActionStatus::ForegroundRequired,
                    Mechanism::Coordinates,
                    "keyboard input goes to the foreground window — target app is not foreground",
                ));
            }
            let n = send_text(text)?;
            Ok(ActionResult::success(
                Mechanism::Coordinates,
                Some(format!("typed {n} chars via SendInput")),
            ))
        }
        Action::Key { chord } => {
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Coordinates,
                    "key chords require physical input — pass the explicit coords flag",
                ));
            }
            if let Some(sel) = &ctx.app {
                let pid = crate::apps::resolve_pid(sel)?;
                if !pid_is_foreground(pid) {
                    return Ok(ActionResult::failure(
                        ActionStatus::ForegroundRequired,
                        Mechanism::Coordinates,
                        "key chords go to the foreground window — target app is not foreground",
                    ));
                }
            }
            send_chord(chord)
        }
        Action::Scroll { delta, target } => {
            if let Some(t) = target {
                let r = resolve_element(t, ctx, cache, &uia)?;
                return scroll_element(&r, delta);
            }
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Coordinates,
                    "scroll without a target requires physical input",
                ));
            }
            send_wheel(delta.dx, delta.dy)?;
            Ok(ActionResult::success(
                Mechanism::Coordinates,
                Some(format!("scrolled ({},{})", delta.dx, delta.dy)),
            ))
        }
        Action::Focus { target } => match target {
            Target::Window { window_id } => focus_window(*window_id, ctx),
            Target::Point { .. } => Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "cannot focus a point",
            )),
            _ => {
                let r = resolve_element(target, ctx, cache, &uia)?;
                match &r.node {
                    LiveNode::Uia(el) => unsafe { el.SetFocus() }
                        .map_err(|e| DriverError::Platform(format!("SetFocus: {e}")))?,
                    LiveNode::Msaa(m) => msaa::take_focus(m)?,
                }
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("focused {}", r.detail)),
                ))
            }
        },
        Action::SetValue { target, value } => {
            let r = resolve_element(target, ctx, cache, &uia)?;
            if let Some(fail) = disabled_verdict(&r) {
                return Ok(fail);
            }
            set_value(&r, value)
        }
        // Wait/Observe/Navigate return before the COM session — this arm
        // only exists to keep the match exhaustive.
        Action::Wait { .. } | Action::Observe | Action::Navigate { .. } => {
            unreachable!("handled before COM init")
        }
    }
}
