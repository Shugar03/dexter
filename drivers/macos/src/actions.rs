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

use crate::{apps, ax, ffi, keymap, permissions, v2};

const ACTION_WALK_DEPTH: u32 = 40;
const ACTION_WALK_MAX: usize = 4_000;

/// A live element resolved for an action, with a display detail.
/// `id` is the element's observation id when resolution produced one —
/// `Target::Focused` resolves straight to an AX node, so it has none.
struct Resolved {
    el: AXUIElement,
    id: Option<dexter_core::ElementId>,
    detail: String,
}

/// How an observation's element ids were minted — a `Target::Element`
/// token is only comparable to a tree walked the same way.
#[derive(Clone, Copy)]
pub(crate) enum Minted {
    /// Full app tree (`ax::collect`) — ids span every window + menubar.
    AppWide,
    /// One window's subtree (`ax::collect_window`) — ids are dense over
    /// that subtree and name a *different* element in a full walk when
    /// the pinned window isn't first in `AXWindows` order.
    Window { cg_bounds: dexter_core::Rect },
}

#[derive(Clone)]
struct ObsEntry {
    pid: Option<i32>,
    elements: Vec<Element>,
    minted: Minted,
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

    pub fn store(
        &self,
        obs: ObservationId,
        pid: Option<i32>,
        elements: Vec<Element>,
        minted: Minted,
    ) {
        let mut g = self.inner.lock().unwrap();
        g.retain(|(id, _)| *id != obs);
        g.push_front((
            obs,
            ObsEntry {
                pid,
                elements,
                minted,
            },
        ));
        g.truncate(4);
    }

    fn get(&self, obs: ObservationId) -> Option<ObsEntry> {
        let g = self.inner.lock().unwrap();
        g.iter().find(|(id, _)| *id == obs).map(|(_, e)| e.clone())
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
                entry.as_ref().map(|e| e.elements.as_slice()),
                *observation,
                *element,
            )?
            .clone();
            let entry = entry.expect("stored_element passed — observation is cached");
            let pid = entry.pid.ok_or_else(|| {
                DriverError::NotFound(
                    "observation was not app-scoped — cannot re-resolve element".into(),
                )
            })?;
            let app = AXUIElement::application(pid);
            let _ = app.set_messaging_timeout(1.5);
            // Re-walk the way the token was minted: scoped ids are dense
            // over the pinned window's subtree, so in a full walk they
            // name a different element whenever that window isn't first
            // in `AXWindows` order (and `AXWindows` order itself shifts
            // — activation reorders it). A moved/closed pinned window
            // is an honest stale, not a fallback to app-wide ids.
            let tree = match entry.minted {
                Minted::Window { cg_bounds } => {
                    ax::collect_window(&app, cg_bounds, ACTION_WALK_DEPTH, ACTION_WALK_MAX)
                        .map_err(|e| {
                            DriverError::StaleReference(format!(
                                "the scoped window moved or closed since observation {}: {e}",
                                observation.0
                            ))
                        })?
                }
                Minted::AppWide => ax::collect(&app, ACTION_WALK_DEPTH, ACTION_WALK_MAX, true),
            };
            let fresh_idx = tree.elements.iter().position(|e| e.id == *element);
            dexter_driver::resolve::verify_identity(
                &stored,
                fresh_idx.map(|i| &tree.elements[i]),
                *observation,
                *element,
            )?;
            let idx = fresh_idx.ok_or_else(|| {
                DriverError::StaleReference(format!(
                    "element {element} vanished — the tree shrank since observation {}",
                    observation.0
                ))
            })?;
            let el = tree.nodes.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference("resolved element vanished mid-walk".into())
            })?;
            Ok(Resolved {
                id: Some(*element),
                detail: describe(&el),
                el,
            })
        }
        Target::Semantic(_) => {
            let (_pid, app) = resolve_app(ctx)?;
            let tree = ax::collect(&app, ACTION_WALK_DEPTH, ACTION_WALK_MAX, true);
            let obs = Observation {
                elements: tree.elements,
                elements_truncated: tree.truncated,
                ..Default::default()
            };
            let found = dexter_driver::resolve::resolve_semantic(&obs, target)?;
            // The parallel `nodes` vec shares element order — find the
            // element's position rather than assuming `id == index + 1`
            // (ids are minted per walk; nothing pins them to offsets).
            let idx = obs
                .elements
                .iter()
                .position(|e| e.id == found.id)
                .ok_or_else(|| {
                    DriverError::StaleReference("resolved element vanished mid-walk".into())
                })?;
            let el = tree.nodes.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference("resolved element vanished mid-walk".into())
            })?;
            Ok(Resolved {
                id: Some(found.id),
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
                id: None,
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
    )
    .with_element(r.id))
}

fn ax_show_menu(r: &Resolved) -> Result<ActionResult, DriverError> {
    r.el.perform_action(&CFString::new("AXShowMenu"))
        .map_err(|e| DriverError::Platform(format!("AXShowMenu failed: {e:?}")))?;
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("opened menu on {}", r.detail)),
    )
    .with_element(r.id))
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

/// Actions that need the accessibility grant — everything that touches
/// the AX tree. Lifecycle, clipboard, navigation and waits work without
/// it; the grant check lives here so a missing AX permission never
/// blocks an action that doesn't need it.
fn needs_ax(action: &Action) -> bool {
    !matches!(
        action,
        Action::Wait { .. }
            | Action::Navigate { .. }
            | Action::LaunchApp { .. }
            | Action::QuitApp { .. }
            | Action::ReadClipboardText
            | Action::WriteClipboardText { .. }
            | Action::Observe
    )
}

/// The plan→execute contract: when a route declared a mechanism,
/// execution must honor it or refuse — never silently take a more
/// intrusive fallback. `would_take` is the mechanism the current world
/// would push us to; a mismatch means the route no longer applies.
fn route_refusal(
    authorized: Option<Mechanism>,
    would_take: Mechanism,
    why: String,
) -> Option<ActionResult> {
    match authorized {
        Some(declared) if declared != would_take => Some(ActionResult::failure(
            ActionStatus::Unsupported,
            declared,
            format!("authorized route {declared:?} no longer applies ({why}) — refusing to switch to {would_take:?}"),
        )),
        _ => None,
    }
}

/// `authorized` binds the mechanism a planned route declared (`Some`)
/// — `None` is the legacy `act` path, which keeps the dynamic choice.
pub fn act(
    action: &Action,
    ctx: &ActContext,
    cache: &ObsCache,
    authorized: Option<Mechanism>,
) -> Result<ActionResult, DriverError> {
    if needs_ax(action) && !permissions::accessibility_trusted() {
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
            let status = std::process::Command::new("/usr/bin/open")
                .arg(url)
                .status()
                .map_err(|e| DriverError::Platform(format!("open: {e}")))?;
            if status.success() {
                Ok(ActionResult::success(
                    Mechanism::NativeAutomation,
                    // Query strings carry signed tokens — the detail is
                    // journaled, so it logs the redacted form.
                    Some(format!("opened {}", dexter_core::redact_url(url))),
                ))
            } else {
                Ok(ActionResult::failure(
                    ActionStatus::Failed,
                    Mechanism::NativeAutomation,
                    format!("open {url} exited {status}"),
                ))
            }
        }
        Action::Click {
            target,
            button,
            count,
        } => {
            if *count > 3 || *count == 0 {
                return Ok(ActionResult::failure(
                    ActionStatus::Failed,
                    Mechanism::Accessibility,
                    format!("click count {count} out of range 1..=3"),
                ));
            }
            match target {
                Target::Point { x, y } => {
                    if !ctx.allow_coordinates {
                        return Ok(ActionResult::failure(
                            ActionStatus::Unsupported,
                            Mechanism::Coordinates,
                            "coordinate input disabled — pass the explicit coords flag",
                        ));
                    }
                    v2::cg_multi_click(*x, *y, *button, *count)?;
                    Ok(ActionResult::success(
                        Mechanism::Coordinates,
                        Some(format!("clicked x{count} at ({x},{y})")),
                    ))
                }
                Target::Window { .. } => Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Accessibility,
                    "window targets not implemented — use semantic or element targets",
                )),
                _ => {
                    // A context menu is one semantic event — there is
                    // no "double right-click". Refuse up front rather
                    // than silently degrading `right x2` to one
                    // show_menu after resolving.
                    if !matches!(button, MouseButton::Left) && *count > 1 {
                        return Ok(ActionResult::failure(
                            ActionStatus::Unsupported,
                            Mechanism::Accessibility,
                            format!("multi-click count {count} only applies to the left button"),
                        ));
                    }
                    let r = resolve_element(target, ctx, cache)?;
                    // v2: a multi-click on an element advertising `open`
                    // is a semantic open — never a physical double-click.
                    if *count >= 2 && *button == MouseButton::Left {
                        if authorized != Some(Mechanism::Coordinates) {
                            if let Ok(raw) = v2::invoke(&r.el, "open") {
                                return Ok(ActionResult::success(
                                    Mechanism::Accessibility,
                                    Some(format!("{raw} on {}", r.detail)),
                                )
                                .with_element(r.id));
                            }
                            if let Some(refusal) = route_refusal(
                                authorized,
                                Mechanism::Coordinates,
                                format!("{} does not advertise 'open'", r.detail),
                            ) {
                                return Ok(refusal);
                            }
                        }
                        // Element doesn't advertise open — a physical
                        // multi-click is the only way, gated hard.
                        if !ctx.allow_coordinates {
                            return Ok(ActionResult::failure(
                                ActionStatus::Unsupported,
                                Mechanism::Accessibility,
                                format!(
                                    "{} does not advertise 'open' — enable coords for a physical {count}-click",
                                    r.detail
                                ),
                            ));
                        }
                        let (pid, _) = resolve_app(ctx)?;
                        if !is_frontmost(pid) {
                            return Ok(ActionResult::failure(
                                ActionStatus::ForegroundRequired,
                                Mechanism::Coordinates,
                                "physical clicks need the target app frontmost",
                            ));
                        }
                        let bounds = v2::ax_bounds(&r.el).ok_or_else(|| {
                            DriverError::NotFound(format!("{} has no bounds", r.detail))
                        })?;
                        let (cx, cy) = v2::center(&bounds);
                        v2::cg_multi_click(cx, cy, *button, *count)?;
                        return Ok(ActionResult::success(
                            Mechanism::Coordinates,
                            Some(format!("clicked x{count} on {}", r.detail)),
                        )
                        .with_element(r.id));
                    }
                    match button {
                        MouseButton::Left => ax_press(&r),
                        _ => ax_show_menu(&r),
                    }
                }
            }
        }
        Action::TypeText { text, target } => {
            let t = target.clone().unwrap_or(Target::Focused);
            let r = resolve_element(&t, ctx, cache)?;
            // v2 semantics: TypeText *appends* — read the scalar value,
            // concatenate, set. A value we can't read back (rich text,
            // attributed content) never gets silently replaced: the
            // physical route types at the caret instead. An authorized
            // AX route that no longer applies refuses rather than
            // escalating to keystrokes.
            if authorized != Some(Mechanism::Coordinates) {
                if ax_value_settable(&r) {
                    let current =
                        r.el.value()
                            .ok()
                            .and_then(|v| v.downcast::<CFString>().map(|s| s.to_string()));
                    if let Some(cur) = current {
                        ax_set_value(&r, &format!("{cur}{text}"))?;
                        return Ok(ActionResult::success(
                            Mechanism::Accessibility,
                            Some(format!("appended on {}", r.detail)),
                        )
                        .with_element(r.id));
                    }
                    // Rich/unreadable value — would need physical typing.
                    if let Some(refusal) = route_refusal(
                        authorized,
                        Mechanism::Coordinates,
                        format!("AXValue on {} is settable but unreadable", r.detail),
                    ) {
                        return Ok(refusal);
                    }
                } else if let Some(refusal) = route_refusal(
                    authorized,
                    Mechanism::Coordinates,
                    format!("AXValue not settable on {}", r.detail),
                ) {
                    return Ok(refusal);
                }
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
            )
            .with_element(r.id))
        }
        Action::Key { chord } => {
            // v2 semantic route first: a chord a menu item advertises
            // becomes an AXPress on that item — no physical input, no
            // foreground requirement. Ambiguity fails closed. An
            // authorized AX route whose menu claim vanished refuses —
            // it never degrades to physical input under a semantic grant.
            if let Some(sel) = &ctx.app {
                let pid = apps::resolve_pid(sel)?;
                if authorized != Some(Mechanism::Coordinates) {
                    if let Some(item) = v2::menu_item_for_chord(pid, chord)? {
                        item.perform_action(&CFString::new("AXPress"))
                            .map_err(|e| {
                                DriverError::Platform(format!("menu AXPress failed: {e:?}"))
                            })?;
                        return Ok(ActionResult::success(
                            Mechanism::Accessibility,
                            Some(format!("pressed menu item for {}", describe_chord(chord))),
                        ));
                    }
                    if let Some(refusal) = route_refusal(
                        authorized,
                        Mechanism::Coordinates,
                        "no menu item advertises this chord".into(),
                    ) {
                        return Ok(refusal);
                    }
                }
                // No menu claim — physical keys need the app frontmost.
                if !ctx.allow_coordinates {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Coordinates,
                        "no menu item advertises this chord and physical input is disabled",
                    ));
                }
                if !is_frontmost(pid) {
                    return Ok(ActionResult::failure(
                        ActionStatus::ForegroundRequired,
                        Mechanism::Coordinates,
                        "key chords go to the frontmost app — target app is not frontmost",
                    ));
                }
                return cg_key_chord(chord);
            }
            // No app scope: the semantic menu route was never available.
            if let Some(refusal) = route_refusal(
                authorized,
                Mechanism::Coordinates,
                "no app scope — cannot resolve a menu claim".into(),
            ) {
                return Ok(refusal);
            }
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Coordinates,
                    "key chords require physical input — pass the explicit coords flag",
                ));
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
                )
                .with_element(r.id));
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
            // v2: window focus normalizes to the window op — AXRaise +
            // app activation, resolved fresh by bounds.
            Target::Window { window_id } => {
                let pid = ctx.app.as_ref().map(apps::resolve_pid).transpose()?;
                let (wpid, win) = v2::resolve_window(Some(*window_id), pid)?;
                let what = v2::window_op(wpid, &win, &dexter_core::WindowOperation::Focus)?;
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("{what} window {window_id}")),
                ))
            }
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
                )
                .with_element(r.id))
            }
        },
        Action::SetValue { target, value } => {
            let r = resolve_element(target, ctx, cache)?;
            ax_set_value(&r, value)?;
            Ok(ActionResult::success(
                Mechanism::Accessibility,
                Some(format!("set value on {}", r.detail)),
            )
            .with_element(r.id))
        }
        Action::Invoke { target, action } => {
            let r = resolve_element(target, ctx, cache)?;
            let raw = v2::invoke(&r.el, action)?;
            Ok(ActionResult::success(
                Mechanism::Accessibility,
                Some(format!("{raw} on {}", r.detail)),
            )
            .with_element(r.id))
        }
        Action::LaunchApp { app, activate } => {
            apps::launch(app, *activate)?;
            Ok(ActionResult::success(
                Mechanism::Api,
                Some(format!("launched {app:?}")),
            ))
        }
        Action::QuitApp { app } => {
            apps::terminate(app)?;
            Ok(ActionResult::success(
                Mechanism::Api,
                Some(format!("quit {app:?}")),
            ))
        }
        Action::Window {
            window_id,
            operation,
        } => {
            let pid = ctx.app.as_ref().map(apps::resolve_pid).transpose()?;
            let (wpid, win) = v2::resolve_window(*window_id, pid)?;
            let what = v2::window_op(wpid, &win, operation)?;
            Ok(ActionResult::success(
                Mechanism::Accessibility,
                Some(format!("{what} (pid {wpid})")),
            ))
        }
        Action::ReadClipboardText => {
            // The text is returned only inside the ActionResult — it is
            // never journaled, fingerprinted or shown in the overlay.
            let text = v2::clipboard_read(None)?;
            Ok(ActionResult::success(
                Mechanism::Api,
                Some(text.unwrap_or_else(|| "clipboard empty".into())),
            ))
        }
        Action::WriteClipboardText { text } => {
            v2::clipboard_write(None, text)?;
            Ok(ActionResult::success(
                Mechanism::Api,
                Some(format!("clipboard set ({} bytes)", text.len())),
            ))
        }
        Action::Drag {
            from,
            to,
            duration_ms,
        } => {
            // Resolve and revalidate both endpoints *before* the button
            // goes down — a stale target discovered mid-drag would
            // strand the pointer.
            if !ctx.allow_coordinates {
                return Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Coordinates,
                    "drag requires physical input — pass the explicit coords flag",
                ));
            }
            let a = resolve_element(from, ctx, cache)?;
            let b = resolve_element(to, ctx, cache)?;
            let ab = v2::ax_bounds(&a.el)
                .ok_or_else(|| DriverError::NotFound(format!("{} has no bounds", a.detail)))?;
            let bb = v2::ax_bounds(&b.el)
                .ok_or_else(|| DriverError::NotFound(format!("{} has no bounds", b.detail)))?;
            let (pid, _) = resolve_app(ctx)?;
            if !is_frontmost(pid) {
                return Ok(ActionResult::failure(
                    ActionStatus::ForegroundRequired,
                    Mechanism::Coordinates,
                    "drag moves the real pointer — target app is not frontmost",
                ));
            }
            let (x1, y1) = v2::center(&ab);
            let (x2, y2) = v2::center(&bb);
            v2::cg_drag(x1, y1, x2, y2, *duration_ms)?;
            Ok(ActionResult::success(
                Mechanism::Coordinates,
                Some(format!("dragged {} onto {}", a.detail, b.detail)),
            )
            .with_element(a.id))
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
    // Role AND subrole — `Element::is_sensitive` checks all three
    // fields; an element sensitive only via subrole must not slip
    // through with a Standard route.
    let sensitive = [r.el.role().ok(), r.el.subrole().ok()]
        .into_iter()
        .flatten()
        .any(|s| ax::is_sensitive_role(&s.to_string()));
    if sensitive {
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
    plan_inner(action, ctx, cache, permissions::accessibility_trusted())
}

fn plan_inner(
    action: &Action,
    ctx: &ActContext,
    cache: &ObsCache,
    ax_trusted: bool,
) -> Result<ExecutionPlan, DriverError> {
    // Without the AX grant, AX-needing actions have no honest route —
    // keep act()'s refusal verdict on the legacy route. Clipboard and
    // lifecycle actions don't need the grant: they still plan their
    // real routes so policy sees mechanism + sensitivity (the secrets
    // floor must not silently drop in the degraded-permission case).
    if !ax_trusted && needs_ax(action) {
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
        Action::Click { target, count, .. } => match target {
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
            _ => {
                // v2: a multi-click is semantic when the element
                // advertises `open`, physical otherwise — declare the
                // route act() will actually take.
                if *count >= 2 {
                    let r = resolve_element(target, ctx, cache)?;
                    let advertises_open =
                        r.el.action_names()
                            .map(|names| {
                                names
                                    .iter()
                                    .any(|n| normalize_ax_role(&n.to_string()) == "open")
                            })
                            .unwrap_or(false);
                    if advertises_open {
                        ExecutionPlan::single(action, ax_route(action, Some(target), &r))
                    } else if ctx.allow_coordinates {
                        ExecutionPlan::single(
                            action,
                            cg_route(
                                action,
                                resolved_target(Some(target), &r),
                                sensitivity_of(&r),
                            ),
                        )
                    } else {
                        empty_plan(action)
                    }
                } else {
                    element_plan(action, target, ctx, cache)?
                }
            }
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
        // v2: a chord the menu bar advertises is an AX press — declare
        // the semantic route when the menu claims it, physical only as
        // the last resort. Ambiguity fails closed at plan time.
        Action::Key { chord } => {
            if let Some(sel) = &ctx.app {
                let pid = apps::resolve_pid(sel)?;
                if v2::menu_item_for_chord(pid, chord)?.is_some() {
                    return Ok(ExecutionPlan::single(
                        action,
                        ExecutionRoute {
                            action: action.clone(),
                            target: TargetDescriptor::from_action(action),
                            mechanism: Some(Mechanism::Accessibility),
                            intrusiveness: Intrusiveness::Background,
                            sensitivity: Sensitivity::Standard,
                            requires_foreground: false,
                        },
                    ));
                }
            }
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
            // v2: window focus is a real window op — AX, visual tier.
            Target::Window { .. } => ExecutionPlan::single(
                action,
                ExecutionRoute {
                    action: action.clone(),
                    target: TargetDescriptor::from_action(action),
                    mechanism: Some(Mechanism::Accessibility),
                    intrusiveness: Intrusiveness::Visual,
                    sensitivity: Sensitivity::Standard,
                    requires_foreground: false,
                },
            ),
            Target::Point { .. } => ExecutionPlan::legacy(action),
            _ => element_plan(action, target, ctx, cache)?,
        },
        Action::SetValue { target, .. } => element_plan(action, target, ctx, cache)?,
        Action::Invoke { target, .. } => element_plan(action, target, ctx, cache)?,
        // Lifecycle runs through LaunchServices/NSRunningApplication —
        // the API mechanism, visible but not input-capturing, and it
        // does not need the AX grant (act() exempts it).
        Action::LaunchApp { .. } | Action::QuitApp { .. } => ExecutionPlan::single(
            action,
            ExecutionRoute {
                action: action.clone(),
                target: TargetDescriptor::from_action(action),
                mechanism: Some(Mechanism::Api),
                intrusiveness: Intrusiveness::Visual,
                // Quitting an app can discard unsaved state — the
                // destructive floor asks for an explicit grant.
                sensitivity: match action {
                    Action::QuitApp { .. } => Sensitivity::Destructive,
                    _ => Sensitivity::Standard,
                },
                requires_foreground: false,
            },
        ),
        // `window_op` has no new-window verb — declare no route for it
        // rather than an AX route that execute then rejects.
        Action::Window { operation, .. } => match operation {
            dexter_core::WindowOperation::New => empty_plan(action),
            _ => ExecutionPlan::single(
                action,
                ExecutionRoute {
                    action: action.clone(),
                    target: TargetDescriptor::from_action(action),
                    mechanism: Some(Mechanism::Accessibility),
                    intrusiveness: Intrusiveness::Visual,
                    // Closing a window can discard unsaved state; the
                    // other ops are rearrangements.
                    sensitivity: match operation {
                        dexter_core::WindowOperation::Close => Sensitivity::Destructive,
                        _ => Sensitivity::Standard,
                    },
                    requires_foreground: false,
                },
            ),
        },
        // Clipboard is semantic but secret-bearing — the sensitivity
        // floor travels on the route so policy gates it independently.
        Action::ReadClipboardText | Action::WriteClipboardText { .. } => ExecutionPlan::single(
            action,
            ExecutionRoute {
                action: action.clone(),
                target: TargetDescriptor::from_action(action),
                mechanism: Some(Mechanism::Api),
                intrusiveness: Intrusiveness::Background,
                sensitivity: Sensitivity::Secrets,
                requires_foreground: false,
            },
        ),
        // A drag is physical wherever it lands — element resolution is
        // AX, but the gesture itself moves the real pointer.
        Action::Drag { .. } => {
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
    };
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    // plan→execute contract: an authorized mechanism is a promise. If
    // the world moved and the only remaining path is a different
    // mechanism, execute must refuse — never escalate silently.
    #[test]
    fn route_refusal_blocks_mechanism_escalation() {
        let refusal = route_refusal(
            Some(Mechanism::Accessibility),
            Mechanism::Coordinates,
            "element no longer settable".into(),
        )
        .expect("declared AX, would take Coordinates — must refuse");
        assert_eq!(refusal.status, ActionStatus::Unsupported);
        assert_eq!(refusal.mechanism, Mechanism::Accessibility);

        // Same mechanism stays allowed.
        assert!(route_refusal(
            Some(Mechanism::Accessibility),
            Mechanism::Accessibility,
            "still applies".into(),
        )
        .is_none());
        // Legacy routes (mechanism `None`) keep the dynamic choice —
        // the compat seam the enforcement deliberately doesn't cover.
        assert!(route_refusal(None, Mechanism::Coordinates, "legacy".into()).is_none());
    }

    #[test]
    fn window_new_declares_no_route() {
        // `window_op` has no new-window verb — planning an AX route for
        // it would only fail at execute. The honest answer is an empty
        // plan, matching the browser driver.
        if !permissions::accessibility_trusted() {
            eprintln!("AX untrusted host — plan falls back to legacy; skipping");
            return;
        }
        let action = Action::Window {
            operation: dexter_core::WindowOperation::New,
            window_id: None,
        };
        let plan = plan(&action, &ActContext::default(), &ObsCache::new()).unwrap();
        assert!(
            plan.routes.is_empty(),
            "Window::New must plan empty, got {:?}",
            plan.routes
        );
    }

    #[test]
    fn degraded_permissions_keep_non_ax_route_metadata() {
        // Without the AX grant a clipboard write still plans its real
        // route — the secrets floor must not silently drop exactly in
        // the degraded-permission case.
        let write = Action::WriteClipboardText { text: "x".into() };
        let plan = plan_inner(&write, &ActContext::default(), &ObsCache::new(), false).unwrap();
        assert_eq!(plan.routes.len(), 1);
        assert_eq!(plan.routes[0].sensitivity, Sensitivity::Secrets);
        assert_eq!(plan.routes[0].mechanism, Some(Mechanism::Api));

        // An AX-needing action still collapses to the legacy refusal.
        let click = Action::Click {
            target: Target::Focused,
            button: MouseButton::Left,
            count: 1,
        };
        let plan = plan_inner(&click, &ActContext::default(), &ObsCache::new(), false).unwrap();
        assert!(
            plan.routes.len() == 1 && plan.routes[0].mechanism.is_none(),
            "AX-needing action must keep the legacy refusal, got {:?}",
            plan.routes
        );
    }

    #[test]
    fn non_left_multi_click_is_rejected_before_resolving() {
        // `right x2` has no semantics — a context menu is one event.
        // The refusal fires before element resolution, so it needs no
        // AX grant to be observable.
        let click = Action::Click {
            target: Target::Focused,
            button: MouseButton::Right,
            count: 2,
        };
        let result = act(&click, &ActContext::default(), &ObsCache::new(), None).unwrap();
        assert_eq!(result.status, ActionStatus::Unsupported);
    }

    #[test]
    fn obs_cache_records_id_minting() {
        // A `Target::Element` token is only comparable to a tree
        // walked the way its observation minted ids: scoped ids are
        // dense over one window's subtree, app-wide ids span every
        // window + menubar. The cache must remember which walk to
        // re-run or resolution compares tokens across namespaces.
        let cache = ObsCache::new();
        let bounds = dexter_core::Rect {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        };
        cache.store(
            ObservationId(1),
            Some(42),
            vec![],
            Minted::Window { cg_bounds: bounds },
        );
        let scoped = cache.get(ObservationId(1)).unwrap();
        assert!(
            matches!(scoped.minted, Minted::Window { cg_bounds } if cg_bounds == bounds),
            "scoped observation must record the window bounds to re-walk"
        );
        cache.store(ObservationId(2), Some(42), vec![], Minted::AppWide);
        assert!(matches!(
            cache.get(ObservationId(2)).unwrap().minted,
            Minted::AppWide
        ));
    }
}
