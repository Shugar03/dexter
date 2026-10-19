use std::ffi::OsStr;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use dexter_core::{
    Action, ActionResult, ActionStatus, Element, ElementId, ElementSource, Mechanism, Observation,
    Target,
};
use dexter_driver::{identity, ActContext, DriverError};
use zbus::blocking::Connection;

use crate::act_rules::{numeric_value, press_action_index};
use crate::atspi::States;
use crate::bus::{self, Acc, LiveWalk, MAX_ACTIONS};
use crate::xtest::{self, InputOutcome};
use crate::ObsCache;

const ACTION_WALK_DEPTH: u32 = 40;
const ACTION_WALK_MAX: usize = 4_000;
const IFACE_ACTION: &str = "org.a11y.atspi.Action";
const IFACE_COMPONENT: &str = "org.a11y.atspi.Component";
const IFACE_EDITABLE_TEXT: &str = "org.a11y.atspi.EditableText";
const IFACE_VALUE: &str = "org.a11y.atspi.Value";

struct Resolved {
    element: Element,
    node: Acc,
    pid: i32,
}

impl Resolved {
    fn detail(&self) -> String {
        let role = self
            .element
            .role
            .as_deref()
            .or(self.element.raw_role.as_deref())
            .unwrap_or("element");
        match self.element.name.as_deref().filter(|name| !name.is_empty()) {
            Some(name) => format!("{role} {name:?}"),
            None => role.to_string(),
        }
    }
}

fn platform(context: &str, error: impl std::fmt::Display) -> DriverError {
    DriverError::Platform(format!("atspi: {context}: {error}"))
}

fn element_index(id: ElementId) -> Option<usize> {
    usize::try_from(id.0.checked_sub(1)?).ok()
}

fn stale_mid_walk() -> DriverError {
    DriverError::StaleReference("resolved element vanished mid-walk".into())
}

fn resolve_target(
    target: &Target,
    ctx: &ActContext,
    cache: &ObsCache,
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
            let idx = element_index(*element).ok_or_else(|| {
                DriverError::StaleReference(format!(
                    "element {} out of range in observation {}",
                    element.0, observation.0
                ))
            })?;
            let stored = entry.elements.get(idx).cloned().ok_or_else(|| {
                DriverError::StaleReference(format!(
                    "element {} out of range in observation {}",
                    element.0, observation.0
                ))
            })?;
            if stored.source == ElementSource::Ocr {
                return Err(DriverError::StaleReference(format!(
                    "element {} is OCR-derived — target its bounds center as a Point instead",
                    element.0
                )));
            }
            let live =
                bus::walk_live(entry.pid, entry.window, entry.max_depth, entry.max_elements)?;
            if identity::fresh_at_stored_index(&stored, &live.elements).is_none() {
                return Err(DriverError::StaleReference(format!(
                    "element {} changed since observation {} — re-observe",
                    element.0, observation.0
                )));
            }
            let fresh = live.elements.get(idx).cloned().ok_or_else(stale_mid_walk)?;
            let node = live.nodes.get(idx).cloned().ok_or_else(stale_mid_walk)?;
            Ok(Resolved {
                element: fresh,
                node,
                pid: entry.pid,
            })
        }
        Target::Semantic(_) => {
            let (pid, live) = walk_app(ctx)?;
            let LiveWalk {
                elements,
                nodes,
                truncated,
            } = live;
            let observation = Observation {
                elements,
                elements_truncated: truncated,
                ..Observation::default()
            };
            let found = dexter_world_model::resolve_element(&observation, target)
                .map_err(|error| identity::resolve_error(error, truncated))?;
            let idx = element_index(found.id).ok_or_else(stale_mid_walk)?;
            let element = observation
                .elements
                .get(idx)
                .cloned()
                .ok_or_else(stale_mid_walk)?;
            let node = nodes.get(idx).cloned().ok_or_else(stale_mid_walk)?;
            Ok(Resolved { element, node, pid })
        }
        Target::Focused => {
            let (pid, live) = walk_app(ctx)?;
            let focused: Vec<usize> = live
                .elements
                .iter()
                .enumerate()
                .filter_map(|(idx, element)| element.focused.then_some(idx))
                .collect();
            match focused.as_slice() {
                [] => Err(DriverError::NotFound(format!(
                    "no focused element in pid {pid}"
                ))),
                [idx] => {
                    let element = live
                        .elements
                        .get(*idx)
                        .cloned()
                        .ok_or_else(stale_mid_walk)?;
                    let node = live.nodes.get(*idx).cloned().ok_or_else(stale_mid_walk)?;
                    Ok(Resolved { element, node, pid })
                }
                many => Err(DriverError::Ambiguous(format!(
                    "{} focused elements in pid {pid}",
                    many.len()
                ))),
            }
        }
        Target::Point { .. } | Target::Window { .. } => {
            Err(DriverError::Unsupported("target is not an element".into()))
        }
    }
}

fn walk_app(ctx: &ActContext) -> Result<(i32, LiveWalk), DriverError> {
    let selector = ctx
        .app
        .as_ref()
        .ok_or_else(|| DriverError::NotFound("this target requires an app scope".into()))?;
    let pid = bus::resolve_app(selector)?;
    let live = bus::walk_live(pid, None, ACTION_WALK_DEPTH, ACTION_WALK_MAX)?;
    Ok((pid, live))
}

fn disabled_verdict(resolved: &Resolved) -> Option<ActionResult> {
    (resolved.element.enabled == Some(false)).then(|| {
        ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            format!("{} is disabled", resolved.detail()),
        )
    })
}

fn physical_input(ctx: &ActContext) -> ActionResult {
    let detail = if ctx.allow_coordinates {
        "XTest input not implemented on linux"
    } else {
        "coordinate input disabled — pass the explicit coords flag"
    };
    ActionResult::failure(ActionStatus::Unsupported, Mechanism::Coordinates, detail)
}

fn input_action<T>(
    input: Result<InputOutcome<T>, DriverError>,
    detail: impl FnOnce(T) -> String,
) -> Result<ActionResult, DriverError> {
    match input? {
        InputOutcome::Complete(value) => Ok(ActionResult::success(
            Mechanism::Coordinates,
            Some(detail(value)),
        )),
        InputOutcome::Unsupported(reason) => Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Coordinates,
            reason,
        )),
    }
}

fn foreground_required(
    ctx: &ActContext,
    detail: &str,
) -> Result<Option<ActionResult>, DriverError> {
    let Some(selector) = ctx.app.as_ref() else {
        return Ok(None);
    };
    let pid = bus::resolve_app(selector)?;
    let windows = bus::list_windows()?;
    let conn = bus::connect()?;
    for (window, node) in windows {
        if window.pid == pid
            && node
                .states(&conn)
                .map_err(|error| platform("Accessible.GetState", error))?
                .has(States::ACTIVE)
        {
            return Ok(None);
        }
    }
    Ok(Some(ActionResult::failure(
        ActionStatus::ForegroundRequired,
        Mechanism::Coordinates,
        detail,
    )))
}

fn interfaces(node: &Acc, conn: &Connection) -> Result<Vec<String>, DriverError> {
    node.interfaces(conn)
        .map_err(|error| platform("Accessible.GetInterfaces", error))
}

fn press(resolved: &Resolved) -> Result<ActionResult, DriverError> {
    let conn = bus::connect()?;
    let interfaces = interfaces(&resolved.node, &conn)?;
    let names = if interfaces.iter().any(|name| name == IFACE_ACTION) {
        let count = resolved
            .node
            .n_actions(&conn)
            .map_err(|error| platform("Action.NActions", error))?
            .clamp(0, MAX_ACTIONS);
        (0..count)
            .map(|index| {
                resolved
                    .node
                    .action_name(&conn, index)
                    .map_err(|error| platform("Action.GetName", error))
            })
            .collect::<Result<Vec<_>, _>>()?
    } else {
        Vec::new()
    };
    let Some(index) = press_action_index(&names) else {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            format!("{} exposes no press action", resolved.detail()),
        ));
    };
    let verb = names[index as usize].trim().to_ascii_lowercase();
    let did_action = resolved
        .node
        .do_action(&conn, index)
        .map_err(|error| platform("Action.DoAction", error))?;
    if !did_action {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            format!("DoAction refused on {}", resolved.detail()),
        ));
    }
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("pressed {} via {verb}", resolved.detail())),
    ))
}

fn writable_text(
    resolved: &Resolved,
    conn: &Connection,
) -> Result<Option<ActionResult>, DriverError> {
    let states = resolved
        .node
        .states(conn)
        .map_err(|error| platform("Accessible.GetState", error))?;
    if states.has(States::READ_ONLY) || !states.has(States::EDITABLE) {
        Ok(Some(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            format!("{} is read-only", resolved.detail()),
        )))
    } else {
        Ok(None)
    }
}

fn set_editable_text(
    resolved: &Resolved,
    text: &str,
    conn: &Connection,
) -> Result<ActionResult, DriverError> {
    if let Some(verdict) = writable_text(resolved, conn)? {
        return Ok(verdict);
    }
    let set = resolved
        .node
        .set_text_contents(conn, text)
        .map_err(|error| platform("EditableText.SetTextContents", error))?;
    if !set {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            format!("SetTextContents refused on {}", resolved.detail()),
        ));
    }
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("set text on {}", resolved.detail())),
    ))
}

fn set_value(resolved: &Resolved, value: &str) -> Result<ActionResult, DriverError> {
    let conn = bus::connect()?;
    let interfaces = interfaces(&resolved.node, &conn)?;
    if interfaces.iter().any(|name| name == IFACE_VALUE) {
        if let Some(numeric) = numeric_value(value) {
            resolved
                .node
                .set_current_value(&conn, numeric)
                .map_err(|error| platform("Value.CurrentValue", error))?;
            return Ok(ActionResult::success(
                Mechanism::Accessibility,
                Some(format!("set value on {}", resolved.detail())),
            ));
        }
    }
    if interfaces.iter().any(|name| name == IFACE_EDITABLE_TEXT) {
        return set_editable_text(resolved, value, &conn);
    }
    Ok(ActionResult::failure(
        ActionStatus::Unsupported,
        Mechanism::Accessibility,
        format!("no Value or EditableText on {}", resolved.detail()),
    ))
}

fn type_text(
    text: &str,
    target: Option<&Target>,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    let focused = Target::Focused;
    let resolved = resolve_target(target.unwrap_or(&focused), ctx, cache)?;
    if let Some(verdict) = disabled_verdict(&resolved) {
        return Ok(verdict);
    }
    let conn = bus::connect()?;
    let interfaces = interfaces(&resolved.node, &conn)?;
    if !interfaces.iter().any(|name| name == IFACE_EDITABLE_TEXT) {
        if !ctx.allow_coordinates {
            return Ok(physical_input(ctx));
        }
        if !interfaces.iter().any(|name| name == IFACE_COMPONENT) {
            return Ok(ActionResult::failure(
                ActionStatus::Failed,
                Mechanism::Accessibility,
                format!("{} cannot receive focus", resolved.detail()),
            ));
        }
        let grabbed = resolved
            .node
            .grab_focus(&conn)
            .map_err(|error| platform("Component.GrabFocus", error))?;
        let focused = grabbed
            && resolved
                .node
                .states(&conn)
                .map_err(|error| platform("Accessible.GetState", error))?
                .has(States::FOCUSED);
        if !focused {
            return Ok(ActionResult::failure(
                ActionStatus::Failed,
                Mechanism::Accessibility,
                format!("{} would not take keyboard focus", resolved.detail()),
            ));
        }
        if let Some(verdict) = foreground_required(
            ctx,
            "keyboard input goes to the foreground window — target app is not foreground",
        )? {
            return Ok(verdict);
        }
        return input_action(xtest::type_text(text), |count| {
            format!("typed {count} chars via XTest")
        });
    }
    set_editable_text(&resolved, text, &conn)
}

fn coordinate_scale(pid: Option<i32>) -> Result<f64, DriverError> {
    pid.map(crate::scale::gdk_scale_for_pid)
        .transpose()
        .map(|scale| scale.unwrap_or(1.0))
}

fn scale_action_error(error: DriverError) -> Result<ActionResult, DriverError> {
    match error {
        DriverError::Unsupported(reason) => Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Coordinates,
            reason,
        )),
        error => Err(error),
    }
}

fn click(
    target: &Target,
    button: dexter_core::MouseButton,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    match target {
        Target::Point { x, y } => {
            if !ctx.allow_coordinates {
                return Ok(physical_input(ctx));
            }
            let pid = ctx.app.as_ref().map(bus::resolve_app).transpose()?;
            let scale = match coordinate_scale(pid) {
                Ok(scale) => scale,
                Err(error) => return scale_action_error(error),
            };
            return input_action(xtest::click(*x * scale, *y * scale, button), |_| {
                format!("clicked {} at ({x},{y})", button_name(button))
            });
        }
        Target::Window { .. } => {
            return Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "window targets are not clickable — target an element",
            ));
        }
        _ => {}
    }
    let resolved = resolve_target(target, ctx, cache)?;
    if let Some(verdict) = disabled_verdict(&resolved) {
        return Ok(verdict);
    }
    if button == dexter_core::MouseButton::Left {
        return press(&resolved);
    }
    if !ctx.allow_coordinates {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            "no AT-SPI action reaches a context menu — enable coords for a pointer click",
        ));
    }
    let Some(bounds) = resolved.element.bounds else {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Coordinates,
            format!("{} reports no bounds to click", resolved.detail()),
        ));
    };
    let x = bounds.x + bounds.w / 2.0;
    let y = bounds.y + bounds.h / 2.0;
    let scale = match coordinate_scale(Some(resolved.pid)) {
        Ok(scale) => scale,
        Err(error) => return scale_action_error(error),
    };
    input_action(xtest::click(x * scale, y * scale, button), |_| {
        format!("clicked {} at ({x},{y})", button_name(button))
    })
}

fn button_name(button: dexter_core::MouseButton) -> &'static str {
    match button {
        dexter_core::MouseButton::Left => "left",
        dexter_core::MouseButton::Right => "right",
        dexter_core::MouseButton::Middle => "middle",
    }
}

fn focus_window(window_id: u32, ctx: &ActContext) -> Result<ActionResult, DriverError> {
    let pid = ctx.app.as_ref().map(bus::resolve_app).transpose()?;
    let windows = bus::list_windows()?;
    let Some((frame, node)) = windows
        .into_iter()
        .find(|(window, _)| window.id == window_id && pid.is_none_or(|pid| window.pid == pid))
    else {
        return Err(DriverError::NotFound(format!(
            "window {window_id} not found"
        )));
    };
    let conn = bus::connect()?;
    let interfaces = interfaces(&node, &conn)?;
    let grabbed = interfaces.iter().any(|name| name == IFACE_COMPONENT)
        && node.grab_focus(&conn).unwrap_or(false);
    if grabbed && wait_for_active(&node, &conn, Duration::from_millis(500))? {
        return Ok(ActionResult::success(
            Mechanism::Accessibility,
            Some(format!("activated window {window_id}")),
        ));
    }
    let failure = |status, mechanism, detail| ActionResult::failure(status, mechanism, detail);
    match crate::ewmh::request_activation(&frame)? {
        crate::ewmh::ActivationRequest::Unsupported(reason) => {
            return Ok(failure(
                ActionStatus::Unsupported,
                Mechanism::NativeAutomation,
                reason,
            ));
        }
        crate::ewmh::ActivationRequest::NoMatch => {
            return Ok(failure(
                ActionStatus::Failed,
                Mechanism::NativeAutomation,
                format!("no X11 client window matches window {window_id}"),
            ));
        }
        crate::ewmh::ActivationRequest::Sent => {}
    }
    if wait_for_active(&node, &conn, Duration::from_secs(1))? {
        return Ok(ActionResult::success(
            Mechanism::NativeAutomation,
            Some(format!(
                "activated window {window_id} via _NET_ACTIVE_WINDOW"
            )),
        ));
    }
    Ok(failure(
        ActionStatus::Failed,
        Mechanism::NativeAutomation,
        format!("window manager did not activate window {window_id}"),
    ))
}

fn wait_for_active(node: &Acc, conn: &Connection, timeout: Duration) -> Result<bool, DriverError> {
    let deadline = Instant::now() + timeout;
    loop {
        if node
            .states(conn)
            .map_err(|error| platform("Accessible.GetState", error))?
            .has(States::ACTIVE)
        {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn focus(target: &Target, ctx: &ActContext, cache: &ObsCache) -> Result<ActionResult, DriverError> {
    match target {
        Target::Window { window_id } => return focus_window(*window_id, ctx),
        Target::Point { .. } => {
            return Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "cannot focus a point",
            ));
        }
        _ => {}
    }
    let resolved = resolve_target(target, ctx, cache)?;
    if let Some(verdict) = disabled_verdict(&resolved) {
        return Ok(verdict);
    }
    let conn = bus::connect()?;
    let interfaces = interfaces(&resolved.node, &conn)?;
    if !interfaces.iter().any(|name| name == IFACE_COMPONENT) {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            format!("no Component interface on {}", resolved.detail()),
        ));
    }
    let focused = resolved
        .node
        .grab_focus(&conn)
        .map_err(|error| platform("Component.GrabFocus", error))?;
    if !focused {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            format!("{} would not take focus", resolved.detail()),
        ));
    }
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("focused {}", resolved.detail())),
    ))
}

struct Scrollbar {
    node: Acc,
    vertical: bool,
    horizontal: bool,
}

struct ScrollUpdate {
    node: Acc,
    before: f64,
    after: f64,
}

fn scrollbars(resolved: &Resolved, conn: &Connection) -> Result<Vec<Scrollbar>, DriverError> {
    let role = resolved
        .element
        .role
        .as_deref()
        .or(resolved.element.raw_role.as_deref())
        .unwrap_or_default();
    let candidates = if role == "scroll_bar" {
        vec![resolved.node.clone()]
    } else {
        resolved
            .node
            .children(conn)
            .map_err(|error| platform("Accessible.GetChildren", error))?
    };
    let mut bars = Vec::new();
    for node in candidates {
        let role = crate::atspi_role_name(
            node.role(conn)
                .map_err(|error| platform("Accessible.GetRole", error))?,
        )
        .and_then(crate::atspi_role);
        if role != Some("scroll_bar")
            || !interfaces(&node, conn)?
                .iter()
                .any(|name| name == IFACE_VALUE)
        {
            continue;
        }
        let states = node
            .states(conn)
            .map_err(|error| platform("Accessible.GetState", error))?;
        if !states.has(States::SHOWING) || !states.has(States::VISIBLE) {
            continue;
        }
        bars.push(Scrollbar {
            node,
            vertical: states.has(States::VERTICAL),
            horizontal: states.has(States::HORIZONTAL),
        });
    }
    Ok(bars)
}

fn scroll(
    target: &Target,
    dx: f64,
    dy: f64,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    let resolved = resolve_target(target, ctx, cache)?;
    let conn = bus::connect()?;
    let bars = scrollbars(&resolved, &conn)?;
    if bars.is_empty() {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            format!("{} exposes no Value scroll bars", resolved.detail()),
        ));
    }
    if !dx.is_finite() || !dy.is_finite() {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            "scroll delta must be finite",
        ));
    }

    let mut updates = Vec::new();
    for (delta, horizontal) in [(dx, true), (dy, false)] {
        if delta == 0.0 {
            continue;
        }
        let bar = bars.iter().find(|bar| {
            if horizontal {
                bar.horizontal
            } else {
                bar.vertical
            }
        });
        let Some(bar) = bar else {
            let orientation = if horizontal { "horizontal" } else { "vertical" };
            return Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                format!("{} has no {orientation} scroll bar", resolved.detail()),
            ));
        };
        let before = bar
            .node
            .current_value(&conn)
            .map_err(|error| platform("Value.CurrentValue", error))?;
        let (min, max) = bar
            .node
            .value_range(&conn)
            .map_err(|error| platform("Value range", error))?;
        if !before.is_finite() || !min.is_finite() || !max.is_finite() || min > max {
            return Err(DriverError::Platform(
                "atspi: invalid scrollbar Value range".into(),
            ));
        }
        updates.push(ScrollUpdate {
            node: bar.node.clone(),
            before,
            after: xtest::clamp_scroll_value(before, delta, min, max),
        });
    }
    if updates.is_empty() {
        return Ok(ActionResult::success(
            Mechanism::Accessibility,
            Some(format!("scrolled {} by ({dx},{dy})", resolved.detail())),
        ));
    }
    if updates.iter().all(|update| update.before == update.after) {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            "already at scroll limit",
        ));
    }
    for update in &updates {
        update
            .node
            .set_current_value(&conn, update.after)
            .map_err(|error| platform("Value.CurrentValue", error))?;
    }
    let mut changed = false;
    for update in updates {
        let after = update
            .node
            .current_value(&conn)
            .map_err(|error| platform("Value.CurrentValue", error))?;
        changed |= after != update.before;
    }
    if !changed {
        return Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::Accessibility,
            "scroll value did not change",
        ));
    }
    Ok(ActionResult::success(
        Mechanism::Accessibility,
        Some(format!("scrolled {} by ({dx},{dy})", resolved.detail())),
    ))
}

fn navigable_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        )
}

fn navigate_with(opener: &OsStr, url: &str) -> Result<ActionResult, DriverError> {
    if !navigable_url(url) {
        return Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::NativeAutomation,
            "refusing to open non-URL input — only http/https/mailto reach xdg-open",
        ));
    }
    match Command::new(opener).arg(url).status() {
        Ok(status) if status.success() => Ok(ActionResult::success(
            Mechanism::NativeAutomation,
            Some(format!("opened {url}")),
        )),
        Ok(status) => Ok(ActionResult::failure(
            ActionStatus::Failed,
            Mechanism::NativeAutomation,
            format!("xdg-open {url} exited {status}"),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::NativeAutomation,
            "xdg-open not found",
        )),
        Err(error) => Err(DriverError::Platform(format!("xdg-open {url}: {error}"))),
    }
}

pub fn act(
    action: &Action,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    match action {
        Action::Wait { millis } => {
            thread::sleep(Duration::from_millis(*millis));
            Ok(ActionResult::success(
                Mechanism::NativeAutomation,
                Some(format!("waited {millis}ms")),
            ))
        }
        Action::Observe => Err(DriverError::Unsupported(
            "Action::Observe is an engine directive, not a driver action".into(),
        )),
        Action::Navigate { url } => navigate_with(OsStr::new("xdg-open"), url),
        Action::Click { target, button } => click(target, *button, ctx, cache),
        Action::TypeText { text, target } => type_text(text, target.as_ref(), ctx, cache),
        Action::Key { chord } => {
            if !ctx.allow_coordinates {
                return Ok(physical_input(ctx));
            }
            if let Some(verdict) = foreground_required(
                ctx,
                "key chords go to the foreground window — target app is not foreground",
            )? {
                return Ok(verdict);
            }
            input_action(xtest::key_chord(chord), |_| {
                "sent key chord via XTest".into()
            })
        }
        Action::Scroll {
            delta,
            target: Some(target),
        } => scroll(target, delta.dx, delta.dy, ctx, cache),
        Action::Scroll {
            delta,
            target: None,
        } => {
            if !ctx.allow_coordinates {
                return Ok(physical_input(ctx));
            }
            input_action(xtest::scroll(delta.dx, delta.dy), |_| {
                "scrolled via XTest".into()
            })
        }
        Action::Focus { target } => focus(target, ctx, cache),
        Action::SetValue { target, value } => {
            let resolved = resolve_target(target, ctx, cache)?;
            if let Some(verdict) = disabled_verdict(&resolved) {
                return Ok(verdict);
            }
            set_value(&resolved, value)
        }
    }
}

#[cfg(test)]
mod navigate_tests {
    use super::{navigable_url, navigate_with};
    use dexter_core::{ActionResult, ActionStatus, Mechanism};
    use dexter_driver::DriverError;
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRIPT_ID: AtomicU64 = AtomicU64::new(0);

    fn navigate_retrying(opener: &OsStr, url: &str) -> Result<ActionResult, DriverError> {
        for retry in 0..=20 {
            let result = navigate_with(opener, url);
            let is_text_file_busy = matches!(
                &result,
                Err(DriverError::Platform(message)) if message.contains("Text file busy")
            );
            if retry == 20 || !is_text_file_busy {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        unreachable!()
    }

    struct Script {
        dir: PathBuf,
        path: PathBuf,
    }

    impl Script {
        fn new(body: &str) -> Self {
            let nonce = NEXT_SCRIPT_ID.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "dexter-linux-navigate-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&dir).expect("create opener test directory");
            let path = dir.join("opener");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write opener script");
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("make opener executable");
            Self { dir, path }
        }

        fn quoted(path: &Path) -> String {
            format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
        }
    }

    impl Drop for Script {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn navigable_url_allowlists_document_schemes() {
        for ok in [
            "https://example.com",
            "http://localhost:8080/x",
            "mailto:a@b.c",
            "HTTP:example",
            "MaIlTo:a@b.c",
        ] {
            assert!(navigable_url(ok), "{ok}");
        }
        for bad in [
            "notepad.exe",
            "C:\\Windows\\System32\\calc.exe",
            "file:///c:/windows/system32/calc.exe",
            "shell:AppsFolder\\x",
            "ms-settings:display",
            "javascript:alert(1)",
            "vbscript:x",
            "-a foo",
            "",
            "http:",
        ] {
            assert!(!navigable_url(bad), "{bad}");
        }
    }

    #[test]
    fn allowed_url_is_passed_as_one_argument_to_the_opener() {
        let args = std::env::temp_dir().join(format!("dexter-args-{}", std::process::id()));
        let script = Script::new(&format!(
            "printf '%s\\n' \"$#\" > {}; printf '%s\\n' \"$@\" > {}",
            Script::quoted(&args),
            Script::quoted(&args.with_extension("argv"))
        ));
        let url = "https://example.com/path?q=1";
        let result = navigate_retrying(script.path.as_os_str(), url).expect("spawn opener");
        assert_eq!(result.status, ActionStatus::Success, "{result:?}");
        assert_eq!(result.mechanism, Mechanism::NativeAutomation);
        assert_eq!(
            fs::read_to_string(&args).expect("read argument count"),
            "1\n"
        );
        assert_eq!(
            fs::read_to_string(args.with_extension("argv")).expect("read argv"),
            format!("{url}\n")
        );
        let _ = fs::remove_file(args);
        let _ = fs::remove_file(
            std::env::temp_dir().join(format!("dexter-args-{}.argv", std::process::id())),
        );
    }

    #[test]
    fn nonzero_opener_exit_is_a_failed_action() {
        let script = Script::new("exit 3");
        let result = navigate_retrying(script.path.as_os_str(), "https://example.com")
            .expect("spawn opener");
        assert_eq!(result.status, ActionStatus::Failed, "{result:?}");
        assert_eq!(result.mechanism, Mechanism::NativeAutomation);
        assert!(result
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("xdg-open https://example.com exited")));
    }

    #[test]
    fn missing_opener_is_unsupported() {
        let missing =
            std::env::temp_dir().join(format!("dexter-missing-opener-{}", std::process::id()));
        let result = navigate_with(missing.as_os_str(), "https://example.com")
            .expect("missing opener verdict");
        assert_eq!(result.status, ActionStatus::Unsupported, "{result:?}");
        assert_eq!(result.mechanism, Mechanism::NativeAutomation);
        assert_eq!(result.detail.as_deref(), Some("xdg-open not found"));
    }

    #[test]
    fn refused_url_never_runs_the_opener() {
        let marker =
            std::env::temp_dir().join(format!("dexter-refused-opener-{}", std::process::id()));
        let _ = fs::remove_file(&marker);
        let script = Script::new(&format!("printf ran > {}", Script::quoted(&marker)));
        let result = navigate_retrying(script.path.as_os_str(), "file:///etc/passwd")
            .expect("refused URL verdict");
        assert_eq!(result.status, ActionStatus::Unsupported, "{result:?}");
        assert_eq!(result.mechanism, Mechanism::NativeAutomation);
        assert_eq!(
            result.detail.as_deref(),
            Some("refusing to open non-URL input — only http/https/mailto reach xdg-open")
        );
        assert!(!marker.exists(), "refused URL ran the opener");
    }

    #[test]
    fn other_spawn_errors_remain_platform_errors() {
        let dir =
            std::env::temp_dir().join(format!("dexter-opener-directory-{}", std::process::id()));
        fs::create_dir(&dir).expect("create opener directory");
        let result = navigate_with(dir.as_os_str(), "https://example.com");
        let _ = fs::remove_dir(dir);
        assert!(matches!(result, Err(DriverError::Platform(_))));
    }
}
