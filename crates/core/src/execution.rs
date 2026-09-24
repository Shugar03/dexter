//! Execution routing — what the driver *will* do before it does it.
//!
//! A driver never just receives an [`Action`]: it first produces an
//! [`ExecutionPlan`] — the ordered, concrete routes it could take, each
//! carrying the mechanism, intrusiveness tier, sensitivity and
//! foreground requirement that policy must authorize. The engine then
//! executes exactly one authorized route; a more intrusive fallback is
//! never silently chosen.

use crate::{Action, ElementId, Intrusiveness, Mechanism, ObservationId, Point, Target};
use serde::{Deserialize, Serialize};

/// How dangerous the payload/target of a route is, beyond its tier.
/// Sensitivity is bound into the approval fingerprint so a grant for a
/// standard field never silently covers a secrets field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sensitivity {
    #[default]
    Standard,
    /// Secure/password fields, clipboard contents — never journaled.
    Secrets,
    /// Irreversible operations (delete, purchase, send).
    Destructive,
}

/// The resolved identity of where a route lands — what policy can
/// match on (`role`, `name`, `identifier`) and what the fingerprint
/// binds (`element`/`observation`/`window_id`/`point`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TargetDescriptor {
    pub role: Option<String>,
    pub name: Option<String>,
    pub identifier: Option<String>,
    /// Element id within `observation`, when resolved to a live element.
    pub element: Option<ElementId>,
    /// Observation the element id belongs to — staleness is checkable.
    pub observation: Option<ObservationId>,
    /// CGWindowID for window-scoped routes.
    pub window_id: Option<u32>,
    /// Raw coordinate target, when the route is a point.
    pub point: Option<Point>,
    /// The route targets whatever element holds focus.
    #[serde(default)]
    pub focused: bool,
}

impl TargetDescriptor {
    /// Descriptor for a [`Target`] — the fields each variant can attest.
    pub fn from_target(target: Option<&Target>) -> Self {
        match target {
            Some(Target::Element {
                observation,
                element,
            }) => Self {
                element: Some(*element),
                observation: Some(*observation),
                ..Default::default()
            },
            Some(Target::Semantic(st)) => Self {
                role: st.role.clone(),
                name: st.name.clone(),
                identifier: st.identifier.clone(),
                ..Default::default()
            },
            Some(Target::Window { window_id }) => Self {
                window_id: Some(*window_id),
                ..Default::default()
            },
            Some(Target::Point { x, y }) => Self {
                point: Some(Point { x: *x, y: *y }),
                ..Default::default()
            },
            Some(Target::Focused) => Self {
                focused: true,
                ..Default::default()
            },
            None => Self::default(),
        }
    }

    /// Descriptor for the target an action carries, if any.
    pub fn from_action(action: &Action) -> Self {
        let target = match action {
            Action::Click { target, .. }
            | Action::Focus { target }
            | Action::SetValue { target, .. }
            | Action::Invoke { target, .. } => Some(target),
            Action::Scroll { target, .. } => target.as_ref(),
            // A targetless `type_text` lands on whatever is focused —
            // the descriptor mirrors the resolution `act` performs so
            // policy and the sensitivity floor can see it.
            Action::TypeText { target: None, .. } => {
                return Self::from_target(Some(&Target::Focused))
            }
            // A chord goes to whatever holds focus — same de facto
            // target, same descriptor.
            Action::Key { .. } => return Self::from_target(Some(&Target::Focused)),
            Action::TypeText { target, .. } => target.as_ref(),
            Action::Drag { from, .. } => Some(from),
            Action::Window { window_id, .. } => {
                return Self {
                    window_id: *window_id,
                    ..Default::default()
                };
            }
            _ => None,
        };
        Self::from_target(target)
    }
}

/// One concrete way to perform an action — the unit policy authorizes
/// and the engine executes. Routes are ordered by fidelity: API, DOM,
/// Accessibility, native automation, vision-derived point, coordinates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRoute {
    /// The concrete action this route performs — may differ from the
    /// requested action (e.g. a semantic target resolved to an element).
    pub action: Action,
    /// Resolved target identity — what policy matches and fingerprints bind.
    pub target: TargetDescriptor,
    /// Declared mechanism. `Some` is enforced: an [`ActionResult`] whose
    /// mechanism differs is a failure, not a fallback. `None` is the
    /// legacy escape for drivers that have not migrated — policy still
    /// gates on `intrusiveness`, mechanism is whatever `act` reports.
    #[serde(default)]
    pub mechanism: Option<Mechanism>,
    /// The tier this route actually operates at — may differ from the
    /// action's shape (a `type_text` routed to CGEvent is `Physical`).
    pub intrusiveness: Intrusiveness,
    #[serde(default)]
    pub sensitivity: Sensitivity,
    /// This route needs the target app frontmost (real input events).
    #[serde(default)]
    pub requires_foreground: bool,
}

impl ExecutionRoute {
    /// The implicit route a v1 driver acts on: the action itself at its
    /// declared tier, with no mechanism claim — `execute` forwards to
    /// `act`. Used as the compat default and to keep a driver-visible
    /// verdict policy-driven when a plan has no routes at all.
    pub fn legacy(action: &Action) -> Self {
        Self {
            action: action.clone(),
            target: TargetDescriptor::from_action(action),
            mechanism: None,
            intrusiveness: action.intrusiveness(),
            sensitivity: Sensitivity::Standard,
            requires_foreground: action.intrusiveness() == Intrusiveness::Physical,
        }
    }
}

/// The driver's answer to "how could this action be performed here?"
/// `routes` is empty when the action is unsupported — an empty plan is
/// not an error, it is an honest "no route exists".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// The action the engine asked for, verbatim.
    pub requested: Action,
    /// Candidate routes, most-faithful first.
    pub routes: Vec<ExecutionRoute>,
}

impl ExecutionPlan {
    /// Compatibility plan for drivers that have not migrated: the
    /// single legacy route. `execute` forwards to `act`.
    pub fn legacy(requested: &Action) -> Self {
        Self {
            requested: requested.clone(),
            routes: vec![ExecutionRoute::legacy(requested)],
        }
    }

    /// A plan declaring exactly one route.
    pub fn single(requested: &Action, route: ExecutionRoute) -> Self {
        Self {
            requested: requested.clone(),
            routes: vec![route],
        }
    }
}
