//! Action execution: semantic first (AX actions), CGEvent only when the
//! caller enabled physical input — and even then, key events go to the
//! *frontmost* app, so a scoped action refuses unless the app is frontmost.
//!
//! `Target::Element` is never acted on through a stored pointer: the cache
//! keeps only the *data* of past observations; `act` re-walks the tree fresh
//! and verifies the element at the same index still matches (role, name,
//! parent, bounds). Anything else is a stale reference — fail closed.

use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use dexter_core::{
    Action, ActionResult, ActionStatus, Element, ExecutionPlan, ExecutionRoute, Intrusiveness,
    Mechanism, MouseButton, Observation, ObservationId, Sensitivity, Target, TargetDescriptor,
};
use dexter_driver::{ActContext, DriverError};
use dexter_world_model::normalize_ax_role;
use std::collections::VecDeque;
use std::sync::Mutex;

use crate::{apps, ax, ffi, keymap, permissions};

const ACTION_WALK_DEPTH: u32 = 40;
const ACTION_WALK_MAX: usize = 4_000;

/// A live element resolved for an action, with a display detail.
struct Resolved {
    el: AXUIElement,
    detail: String,
}

struct ObsEntry {
    pid: Option<i32>,
    elements: Vec<Element>,
}

/// Cache of past observation *data* (never AXUIElement pointers — those are
/// !Send and can go stale silently). Bounded to the last few observations.
pub struct ObsCache {
    inner: Mutex<VecDeque<(ObservationId, ObsEntry)>>,
}

impl ObsCache {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(VecDeque::new()),
        }
    }

    pub fn store(&self, obs: ObservationId, pid: Option<i32>, elements: Vec<Element>) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|(id, _)| *id != obs);
        g.push_front((obs, ObsEntry { pid, elements }));
        g.truncate(4);
    }

    fn get(&self, obs: ObservationId) -> Option<(Option<i32>, Vec<Element>)> {
        let g = self.inner.lock().unwrap();
        g.iter()
            .find(|(id, _)| *id == obs)
            .map(|(_, e)| (e.pid, e.elements.clone()))
    }
}

impl Default for ObsCache {
    fn default() -> Self {
        Self::new()
    }
}

fn describe(el: &AXUIElement) -> String {
    let role = el.role().ok().map(|r| r.to_string());
    let name = el
        .title()
        .ok()
        .map(|t| t.to_string())
        .or_else(|| el.description().ok().map(|d| d.to_string()));
    match (role, name) {
        (Some(r), Some(n)) => format!("{r} \"{n}\""),
        (Some(r), None) => r,
        _ => "element".into(),
    }
}

fn resolve_app(ctx: &ActContext) -> Result<(i32, AXUIElement), DriverError> {
    let sel = ctx
        .app
        .clone()
        .ok_or_else(|| DriverError::NotFound("this target requires an app scope".into()))?;
    let pid = apps::resolve_pid(&sel)?;
    let app = AXUIElement::application(pid);
    let _ = app.set_messaging_timeout(1.5);
    Ok((pid, app))
}

/// Resolve a target to a live element. `Semantic` and `Element` targets both
/// trigger a fresh walk — never trust a stale snapshot for actions.
/// Identity rules live in `dexter_driver::resolve`; this keeps only the
/// AX mechanics (pid, walk, node handle).
fn resolve_element(
    target: &Target,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<Resolved, DriverError> {
    match target {
        Target::Element {
            observation,
            element,
        } => {
            let entry = cache.get(*observation);
            let stored = dexter_driver::resolve::stored_element(
                entry.as_ref().map(|(_, els)| els.as_slice()),
                *observation,
                *element,
            )?
            .clone();
            let pid = entry.and_then(|(pid, _)| pid).ok_or_else(|| {
                DriverError::NotFound(
                    "observation was not app-scoped — cannot re-resolve element".into(),
                )
            })?;
            let app = AXUIElement::application(pid);
            let _ = app.set_messaging_timeout(1.5);
            let tree = ax::collect(&app, ACTION_WALK_DEPTH, ACTION_WALK_MAX);
            let fresh_idx = tree.elements.iter().position(|e| e.id == *element);
            dexter_driver::resolve::verify_identity(
                &stored,
                fresh_idx.map(|i| &tree.elements[i]),
                *observation,
                *element,
            )?;
            let idx = fresh_idx.expect("position was Some — verify_identity refuses None");
            let el = tree.nodes.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference("resolved element vanished mid-walk".into())
            })?;
            Ok(Resolved {
                detail: describe(&el),
                el,
            })
        }
        Target::Semantic(_) => {
            let (_pid, app) = resolve_app(ctx)?;
            let tree = ax::collect(&app, ACTION_WALK_DEPTH, ACTION_WALK_MAX);
            let obs = Observation {
                elements: tree.elements,
                elements_truncated: tree.truncated,
                ..Default::default()
            };
            let found = dexter_driver::resolve::resolve_semantic(&obs, target)?;
            let idx = found.id.0 as usize - 1;
            let el = tree.nodes.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference("resolved element vanished mid-walk".into())
            })?;
            Ok(Resolved {
                detail: describe(&el),
                el,
            })
        }
        Target::Focused => {
            let (_pid, app) = resolve_app(ctx)?;
            let attr = AXAttribute::<core_foundation::base::CFType>::new(&CFString::new(
                "AXFocusedUIElement",
            ));
            let v: core_foundation::base::CFType = app
                .attribute(&attr)
                .map_err(|_| DriverError::NotFound("no focused element".into()))?;
            let el = v.downcast::<AXUIElement>().ok_or_else(|| {
                DriverError::NotFound("focused attribute was not an element".into())
            })?;
            Ok(Resolved {
                detail: describe(&el),
                el,
            })
        }
        Target::Point { .. } | Target::Window { .. } => {
            Err(DriverError::Unsupported("target is not an element".into()))
        }
    }
}

fn ax_press(r: &Resolved) -> Result<ActionResult, DriverError> {
    r.el.perform_action(&CFString::new("AXPress"))
        .map_err(|e| DriverError::Platform(format!("AXPress failed: {e:?}")))?;
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("pressed {}", r.detail)),
    ))
}

fn ax_show_menu(r: &Resolved) -> Result<ActionResult, DriverError> {
    r.el.perform_action(&CFString::new("AXShowMenu"))
        .map_err(|e| DriverError::Platform(format!("AXShowMenu failed: {e:?}")))?;
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("opened menu on {}", r.detail)),
    ))
}

fn ax_focus(r: &Resolved) -> Result<(), DriverError> {
    r.el.set_attribute(&AXAttribute::<()>::focused(), CFBoolean::from(true))
        .map_err(|e| DriverError::Platform(format!("AXFocused set failed: {e:?}")))
}

fn ax_set_value(r: &Resolved, value: &str) -> Result<(), DriverError> {
    r.el.set_attribute(
        &AXAttribute::<()>::value(),
        CFString::new(value).as_CFType(),
    )
    .map_err(|e| DriverError::Platform(format!("AXValue set failed: {e:?}")))
}

fn ax_value_settable(r: &Resolved) -> bool {
    r.el.is_settable(&AXAttribute::<()>::value())
        .unwrap_or(false)
}

fn cg_mouse_click(x: f64, y: f64, button: MouseButton) -> Result<(), DriverError> {
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
    unsafe {
        let d = ffi::CGEventCreateMouseEvent(std::ptr::null(), down, p, btn);
        let u = ffi::CGEventCreateMouseEvent(std::ptr::null(), up, p, btn);
        if d.is_null() || u.is_null() {
            return Err(DriverError::Platform("CGEventCreateMouseEvent null".into()));
        }
        ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, d);
        ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, u);
        ffi::CFRelease(d);
        ffi::CFRelease(u);
    }
    Ok(())
}

fn cg_scroll(dx: i32, dy: i32) -> Result<(), DriverError> {
    unsafe {
        let ev = ffi::CGEventCreateScrollWheelEvent(
            std::ptr::null(),
            ffi::K_CG_SCROLL_UNIT_LINE,
            2,
            dy,
            dx,
        );
        if ev.is_null() {
            return Err(DriverError::Platform(
                "CGEventCreateScrollWheelEvent null".into(),
            ));
        }
        ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, ev);
        ffi::CFRelease(ev);
    }
    Ok(())
}

fn cg_type_text(text: &str) -> Result<(), DriverError> {
    // Post text in small chunks — CGEventKeyboardSetUnicodeString has a
    // limited buffer per event.
    let utf16: Vec<u16> = text.encode_utf16().collect();
    for chunk in utf16.chunks(20) {
        unsafe {
            let down = ffi::CGEventCreateKeyboardEvent(std::ptr::null(), 0, true);
            let up = ffi::CGEventCreateKeyboardEvent(std::ptr::null(), 0, false);
            if down.is_null() || up.is_null() {
                return Err(DriverError::Platform(
                    "CGEventCreateKeyboardEvent null".into(),
                ));
            }
            ffi::CGEventKeyboardSetUnicodeString(down, chunk.len(), chunk.as_ptr());
            ffi::CGEventKeyboardSetUnicodeString(up, chunk.len(), chunk.as_ptr());
            ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, down);
            ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, up);
            ffi::CFRelease(down);
            ffi::CFRelease(up);
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    Ok(())
}

fn cg_key_chord(chord: &dexter_core::KeyChord) -> Result<ActionResult, DriverError> {
    let keycode = keymap::keycode(&chord.key)
        .ok_or_else(|| DriverError::Unsupported(format!("unknown key '{}'", chord.key)))?;
    let mut flags = 0u64;
    for m in &chord.modifiers {
        flags |= keymap::modifier_flag(m)
            .ok_or_else(|| DriverError::Unsupported(format!("unknown modifier '{m}'")))?;
    }
    unsafe {
        let down = ffi::CGEventCreateKeyboardEvent(std::ptr::null(), keycode, true);
        let up = ffi::CGEventCreateKeyboardEvent(std::ptr::null(), keycode, false);
        if down.is_null() || up.is_null() {
            return Err(DriverError::Platform(
                "CGEventCreateKeyboardEvent null".into(),
            ));
        }
        ffi::CGEventSetFlags(down, flags);
        ffi::CGEventSetFlags(up, flags);
        ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, down);
        ffi::CGEventPost(ffi::K_CG_HID_EVENT_TAP, up);
        ffi::CFRelease(down);
        ffi::CFRelease(up);
    }
    Ok(ActionResult::success(
        Mechanism::Coordinates,
        Some(format!("posted chord {}", describe_chord(chord))),
    ))
}

fn describe_chord(chord: &dexter_core::KeyChord) -> String {
    let mut parts = chord.modifiers.clone();
    parts.push(chord.key.clone());
    parts.join("+")
}

/// Whether `pid` is the frontmost app — CGEvent key/type events go to the
/// frontmost app, so posting to a background app would type elsewhere.
fn is_frontmost(pid: i32) -> bool {
    apps::frontmost_pid() == Some(pid)
}

pub fn act(
    action: &Action,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    if !permissions::accessibility_trusted() {
        return Ok(ActionResult::failure(
            ActionStatus::PermissionDenied,
            Mechanism::Accessibility,
            "accessibility permission not granted",
        ));
    }
    match action {
        Action::Wait { millis } => {
            std::thread::sleep(std::time::Duration::from_millis(*millis));
            Ok(ActionResult::success(
                Mechanism::NativeAutomation,
                Some(format!("waited {millis}ms")),
            ))
        }
        Action::Observe => Err(DriverError::Unsupported(
            "Action::Observe is an engine directive, not a driver action".into(),
        )),
        Action::Navigate { url } => {
            let status = std::process::Command::new("open")
                .arg(url)
                .status()
                .map_err(|e| DriverError::Platform(format!("open: {e}")))?;
            if status.success() {
                Ok(ActionResult::success(
                    Mechanism::NativeAutomation,
                    Some(format!("opened {url}")),
                ))
            } else {
                Ok(ActionResult::failure(
                    ActionStatus::Failed,
                    Mechanism::NativeAutomation,
                    format!("open {url} exited {status}"),
                ))
            }
        }
        Action::Click { target, button } => match target {
            Target::Point { x, y } => {
                if !ctx.allow_coordinates {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Coordinates,
                        "coordinate input disabled — pass the explicit coords flag",
                    ));
                }
                cg_mouse_click(*x, *y, *button)?;
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
                let r = resolve_element(target, ctx, cache)?;
                match button {
                    MouseButton::Left => ax_press(&r),
                    _ => ax_show_menu(&r),
                }
            }
        },
        Action::TypeText { text, target } => {
            let t = target.clone().unwrap_or(Target::Focused);
            let r = resolve_element(&t, ctx, cache)?;
            // Semantic-first: prefer AXValue set; physical typing only when
            // enabled AND the app is frontmost.
            if ax_value_settable(&r) {
                ax_set_value(&r, text)?;
                return Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("set value on {}", r.detail)),
                ));
            }
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Accessibility,
                    format!(
                        "AXValue not settable on {} — enable coords for keyboard fallback",
                        r.detail
                    ),
                ));
            }
            let (pid, _) = resolve_app(ctx)?;
            if !is_frontmost(pid) {
                return Ok(ActionResult::failure(
                    ActionStatus::ForegroundRequired,
                    Mechanism::Coordinates,
                    "keyboard input goes to the frontmost app — target app is not frontmost",
                ));
            }
            cg_type_text(text)?;
            Ok(ActionResult::success(
                Mechanism::Coordinates,
                Some(format!("typed {} chars via CGEvent", text.chars().count())),
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
                let pid = apps::resolve_pid(sel)?;
                if !is_frontmost(pid) {
                    return Ok(ActionResult::failure(
                        ActionStatus::ForegroundRequired,
                        Mechanism::Coordinates,
                        "key chords go to the frontmost app — target app is not frontmost",
                    ));
                }
            }
            cg_key_chord(chord)
        }
        Action::Scroll { delta, target } => {
            if let Some(t) = target {
                let r = resolve_element(t, ctx, cache)?;
                r.el.perform_action(&CFString::new("AXScrollToVisible"))
                    .map_err(|e| {
                        DriverError::Platform(format!("AXScrollToVisible failed: {e:?}"))
                    })?;
                return Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("scrolled {} into view", r.detail)),
                ));
            }
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Coordinates,
                    "scroll without a target requires physical input",
                ));
            }
            cg_scroll(delta.dx as i32, (delta.dy / 40.0) as i32)?;
            Ok(ActionResult::success(
                Mechanism::Coordinates,
                Some(format!("scrolled ({},{})", delta.dx, delta.dy)),
            ))
        }
        Action::Focus { target } => match target {
            Target::Window { .. } => Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "window focus not implemented",
            )),
            Target::Point { .. } => Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "cannot focus a point",
            )),
            _ => {
                let r = resolve_element(target, ctx, cache)?;
                ax_focus(&r)?;
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("focused {}", r.detail)),
                ))
            }
        },
        Action::SetValue { target, value } => {
            let r = resolve_element(target, ctx, cache)?;
            ax_set_value(&r, value)?;
            Ok(ActionResult::success(
                Mechanism::Accessibility,
                Some(format!("set value on {}", r.detail)),
            ))
        }
    }
}

/// An honest "no route exists" — the engine still gates the action's
/// declared tier on the legacy fallback, and execute->act reports the
/// refusal with its real mechanism.
fn empty_plan(action: &Action) -> ExecutionPlan {
    ExecutionPlan {
        requested: action.clone(),
        routes: Vec::new(),
    }
}

/// The route `act` takes through the accessibility tree on a resolved
/// element: semantic, background-safe, never steals focus.
fn ax_route(action: &Action, target: Option<&Target>, r: &Resolved) -> ExecutionRoute {
    ExecutionRoute {
        action: action.clone(),
        target: resolved_target(target, r),
        mechanism: Some(Mechanism::Accessibility),
        intrusiveness: Intrusiveness::Background,
        sensitivity: sensitivity_of(r),
        requires_foreground: false,
    }
}

/// The route `act` takes through CGEvent: real input events aimed at
/// whatever is frontmost — physical tier, needs the foreground.
fn cg_route(action: &Action, target: TargetDescriptor, sensitivity: Sensitivity) -> ExecutionRoute {
    ExecutionRoute {
        action: action.clone(),
        target,
        mechanism: Some(Mechanism::Coordinates),
        intrusiveness: Intrusiveness::Physical,
        sensitivity,
        requires_foreground: true,
    }
}

/// Secrets when the resolved element is a secure/password field — the
/// approval fingerprint binds sensitivity, so a standard grant never
/// silently covers a secrets field.
fn sensitivity_of(r: &Resolved) -> Sensitivity {
    let role = r.el.role().ok().map(|s| s.to_string());
    if role.as_deref().is_some_and(ax::is_sensitive_role) {
        Sensitivity::Secrets
    } else {
        Sensitivity::Standard
    }
}

/// `TargetDescriptor::from_target` enriched with whatever the freshly
/// resolved element attests — normalized role, name, identifier — so
/// policy matches and the approval fingerprint binds the real control,
/// not just the query. Best-effort: unreadable attributes keep the
/// declared descriptor.
fn resolved_target(target: Option<&Target>, r: &Resolved) -> TargetDescriptor {
    let mut d = TargetDescriptor::from_target(target);
    if let Ok(role) = r.el.role() {
        d.role = Some(normalize_ax_role(&role.to_string()));
    }
    if let Some(name) =
        r.el.title()
            .ok()
            .or_else(|| r.el.description().ok())
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    {
        d.name = Some(name);
    }
    if let Some(id) =
        r.el.identifier()
            .ok()
            .map(|s| s.to_string())
            .filter(|s| !s.is_empty())
    {
        d.identifier = Some(id);
    }
    d
}

/// Resolve `target` exactly as `act` would and declare the AX route.
/// Resolution failures (NotFound, Ambiguous, StaleReference) surface
/// honestly rather than becoming a plan-time guess.
fn element_plan(
    action: &Action,
    target: &Target,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ExecutionPlan, DriverError> {
    let r = resolve_element(target, ctx, cache)?;
    Ok(ExecutionPlan::single(
        action,
        ax_route(action, Some(target), &r),
    ))
}

/// Read-only planning: declare the route `act` would actually take, with
/// the mechanism `ActionResult` will report. Policy sees the real tier
/// before anything runs — a `type_text` that would fall back to CGEvent
/// typing is `Physical`/`requires_foreground` here, not `Background`.
pub fn plan(
    action: &Action,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ExecutionPlan, DriverError> {
    // Without the AX grant act() refuses every action the same way — no
    // honest route exists, so keep that verdict on the legacy route.
    if !permissions::accessibility_trusted() {
        return Ok(ExecutionPlan::legacy(action));
    }
    let plan = match action {
        Action::Wait { .. } => ExecutionPlan::single(
            action,
            ExecutionRoute {
                action: action.clone(),
                target: TargetDescriptor::from_action(action),
                mechanism: Some(Mechanism::NativeAutomation),
                intrusiveness: Intrusiveness::Background,
                sensitivity: Sensitivity::Standard,
                requires_foreground: false,
            },
        ),
        Action::Navigate { .. } => ExecutionPlan::single(
            action,
            ExecutionRoute {
                action: action.clone(),
                target: TargetDescriptor::from_action(action),
                mechanism: Some(Mechanism::NativeAutomation),
                intrusiveness: Intrusiveness::Visual,
                sensitivity: Sensitivity::Standard,
                requires_foreground: false,
            },
        ),
        // act() rejects Observe as an engine directive — the legacy
        // route keeps that Unsupported verdict.
        Action::Observe => ExecutionPlan::legacy(action),
        Action::Click { target, .. } => match target {
            // A point can only be reached with real input events.
            Target::Point { .. } => {
                if ctx.allow_coordinates {
                    ExecutionPlan::single(
                        action,
                        cg_route(
                            action,
                            TargetDescriptor::from_action(action),
                            Sensitivity::Standard,
                        ),
                    )
                } else {
                    empty_plan(action)
                }
            }
            // act() refuses window clicks — legacy keeps the verdict.
            Target::Window { .. } => ExecutionPlan::legacy(action),
            _ => element_plan(action, target, ctx, cache)?,
        },
        Action::TypeText { target, .. } => {
            // Same resolution and AXValue-settability check act()
            // performs — declared now so a CGEvent fallback is gated as
            // Physical before any keystroke exists.
            let t = target.clone().unwrap_or(Target::Focused);
            let r = resolve_element(&t, ctx, cache)?;
            let route = if ax_value_settable(&r) || !ctx.allow_coordinates {
                // Not-settable without coords still declares AX: execute
                // produces act()'s Unsupported verdict, matching the
                // mechanism, rather than a plan-time refusal.
                ax_route(action, Some(&t), &r)
            } else {
                cg_route(action, resolved_target(Some(&t), &r), sensitivity_of(&r))
            };
            ExecutionPlan::single(action, route)
        }
        // Key chords are physical input: no semantic equivalent exists.
        Action::Key { .. } => {
            if ctx.allow_coordinates {
                ExecutionPlan::single(
                    action,
                    cg_route(
                        action,
                        TargetDescriptor::from_action(action),
                        Sensitivity::Standard,
                    ),
                )
            } else {
                empty_plan(action)
            }
        }
        Action::Scroll { target, .. } => match target {
            Some(t) => element_plan(action, t, ctx, cache)?,
            // No target = pointer-relative scroll — real input events.
            None => {
                if ctx.allow_coordinates {
                    ExecutionPlan::single(
                        action,
                        cg_route(
                            action,
                            TargetDescriptor::from_action(action),
                            Sensitivity::Standard,
                        ),
                    )
                } else {
                    empty_plan(action)
                }
            }
        },
        Action::Focus { target } => match target {
            // act() refuses window and point focus — legacy keeps the
            // Unsupported verdict.
            Target::Window { .. } | Target::Point { .. } => ExecutionPlan::legacy(action),
            _ => element_plan(action, target, ctx, cache)?,
        },
        Action::SetValue { target, .. } => element_plan(action, target, ctx, cache)?,
    };
    Ok(plan)
}
