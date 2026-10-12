use std::thread;
use std::time::Duration;

use dexter_core::{
    Action, ActionResult, ActionStatus, Element, ElementId, ElementSource, Mechanism, Observation,
    Target,
};
use dexter_driver::{identity, ActContext, DriverError};
use zbus::blocking::Connection;

use crate::act_rules::{numeric_value, press_action_index};
use crate::atspi::States;
use crate::bus::{self, Acc, LiveWalk, MAX_ACTIONS};
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
            })
        }
        Target::Semantic(_) => {
            let (_, live) = walk_app(ctx)?;
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
            Ok(Resolved { element, node })
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
                    Ok(Resolved { element, node })
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
        return Ok(physical_input(ctx));
    }
    set_editable_text(&resolved, text, &conn)
}

fn click(
    target: &Target,
    button: dexter_core::MouseButton,
    ctx: &ActContext,
    cache: &ObsCache,
) -> Result<ActionResult, DriverError> {
    match target {
        Target::Point { .. } => return Ok(physical_input(ctx)),
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
    Ok(physical_input(ctx))
}

fn focus(target: &Target, ctx: &ActContext, cache: &ObsCache) -> Result<ActionResult, DriverError> {
    match target {
        Target::Window { .. } => {
            return Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Accessibility,
                "window focus not implemented on linux",
            ));
        }
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
        Action::Navigate { .. } => Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Api,
            "navigate not implemented on linux",
        )),
        Action::Click { target, button } => click(target, *button, ctx, cache),
        Action::TypeText { text, target } => type_text(text, target.as_ref(), ctx, cache),
        Action::Key { .. } => Ok(physical_input(ctx)),
        Action::Scroll {
            target: Some(_), ..
        } => Ok(ActionResult::failure(
            ActionStatus::Unsupported,
            Mechanism::Accessibility,
            "AT-SPI scroll not implemented — Component.ScrollTo only reveals, it doesn't scroll by a delta",
        )),
        Action::Scroll { target: None, .. } => Ok(physical_input(ctx)),
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
