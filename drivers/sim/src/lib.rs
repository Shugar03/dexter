//! `SimDriver` — a synthetic [`ComputerDriver`] with a programmable world.
//!
//! Two purposes: hermetic end-to-end tests of the engine (no OS involved),
//! and the seed of the synthetic training environment — a world with ground
//! truth where effects are declared, not inferred.
//!
//! Semantics mirror the real contract: semantic targets resolve through the
//! world model (ambiguous fails closed), element targets are bound to the
//! observation that produced them, and physical-input mechanisms are gated
//! behind `allow_coordinates`.

use dexter_core::{
    Action, ActionResult, ActionStatus, Element, ElementId, Mechanism, Observation, ObservationId,
    ObservationScope, SemanticTarget, Target, Window,
};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

/// A world mutation applied after a successful press on a matching element.
#[derive(Debug, Clone)]
pub enum Effect {
    /// Spawn a new element into the world (id assigned automatically).
    Spawn(Element),
    /// Set the pressed element's value.
    SetValue(String),
    /// Remove the pressed element (e.g. a button that dismisses a dialog).
    RemoveSelf,
    /// Set the value of the first element matching the target.
    SetValueOf(SemanticTarget, String),
}

struct Rule {
    when: SemanticTarget,
    effect: Effect,
}

struct SimState {
    elements: Vec<Element>,
    windows: Vec<Window>,
    next_element_id: u64,
    /// Audit trail — which elements were pressed, in order.
    pressed: Vec<ElementId>,
    obs_cache: VecDeque<(ObservationId, Vec<Element>)>,
    rules: Vec<Rule>,
}

/// A deterministic in-memory computer.
pub struct SimDriver {
    state: Mutex<SimState>,
    next_observation: AtomicU64,
}

impl SimDriver {
    /// A world with these elements. Element ids are used as given — callers
    /// keep them unique and consecutive-ish for readability.
    pub fn new(elements: Vec<Element>) -> Self {
        let next_id = elements.iter().map(|e| e.id.0).max().unwrap_or(0) + 1;
        Self {
            state: Mutex::new(SimState {
                elements,
                windows: vec![Window {
                    id: 1,
                    pid: 1,
                    app: "sim".into(),
                    title: Some("Sim Window".into()),
                    bounds: dexter_core::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 800.0,
                        h: 600.0,
                    },
                    on_screen: true,
                    layer: 0,
                }],
                next_element_id: next_id,
                pressed: Vec::new(),
                obs_cache: VecDeque::new(),
                rules: Vec::new(),
            }),
            next_observation: AtomicU64::new(1),
        }
    }

    /// When an element matching `when` is pressed, apply `effect`.
    pub fn on_press(&self, when: SemanticTarget, effect: Effect) {
        self.state.lock().unwrap().rules.push(Rule { when, effect });
    }

    /// Add a window to the simulated world — tests window scoping.
    pub fn add_window(&self, window: Window) {
        self.state.lock().unwrap().windows.push(window);
    }

    /// Elements pressed so far — test observability hook.
    pub fn pressed(&self) -> Vec<ElementId> {
        self.state.lock().unwrap().pressed.clone()
    }

    /// Current world elements — test observability hook.
    pub fn elements(&self) -> Vec<Element> {
        self.state.lock().unwrap().elements.clone()
    }

    fn snapshot(&self) -> Observation {
        let s = self.state.lock().unwrap();
        let id = ObservationId(self.next_observation.fetch_add(1, Ordering::SeqCst));
        Observation {
            id,
            timestamp: SystemTime::now(),
            app: Some(dexter_core::AppSelector::Pid(1)),
            pid: Some(1),
            windows: s.windows.clone(),
            elements: s.elements.clone(),
            ..Default::default()
        }
    }

    fn resolve(&self, target: &Target, _ctx: &ActContext) -> Result<ElementId, DriverError> {
        match target {
            Target::Element {
                observation,
                element,
            } => {
                let s = self.state.lock().unwrap();
                let entry = s
                    .obs_cache
                    .iter()
                    .find(|(id, _)| *id == *observation)
                    .ok_or_else(|| {
                        DriverError::StaleReference(format!(
                            "observation {} not held — re-observe",
                            observation.0
                        ))
                    })?;
                let stored = entry.1.iter().find(|e| e.id == *element).ok_or_else(|| {
                    DriverError::StaleReference(format!(
                        "element {} not in observation {}",
                        element.0, observation.0
                    ))
                })?;
                // Same contract as macOS: verify the element still matches
                // in the *current* world.
                let fresh = s
                    .elements
                    .iter()
                    .find(|e| e.id == *element)
                    .ok_or_else(|| {
                        DriverError::StaleReference(format!("element {} vanished", element.0))
                    })?;
                if fresh.role != stored.role || fresh.name != stored.name {
                    return Err(DriverError::StaleReference(format!(
                        "element {} changed since observation {}",
                        element.0, observation.0
                    )));
                }
                Ok(*element)
            }
            Target::Semantic(_) | Target::Focused => {
                let obs = self.snapshot();
                let el =
                    dexter_world_model::resolve_element(&obs, target).map_err(|e| match e {
                        dexter_core::DexterError::Ambiguous(m) => DriverError::Ambiguous(m),
                        dexter_core::DexterError::NotFound(m) => DriverError::NotFound(m),
                        other => DriverError::Platform(other.to_string()),
                    })?;
                Ok(el.id)
            }
            Target::Point { .. } | Target::Window { .. } => {
                Err(DriverError::Unsupported("target is not an element".into()))
            }
        }
    }

    fn apply_effects(&self, pressed_id: ElementId) {
        let mut s = self.state.lock().unwrap();
        let pressed_el = match s.elements.iter().find(|e| e.id == pressed_id) {
            Some(e) => e.clone(),
            None => return,
        };
        // First matching rule wins.
        let effect = s
            .rules
            .iter()
            .find(|r| {
                let one = Observation {
                    elements: vec![pressed_el.clone()],
                    ..Default::default()
                };
                !dexter_world_model::find_elements(&one, &r.when).is_empty()
            })
            .map(|r| r.effect.clone());
        let Some(effect) = effect else { return };
        match effect {
            Effect::Spawn(mut el) => {
                el.id = ElementId(s.next_element_id);
                s.next_element_id += 1;
                s.elements.push(el);
            }
            Effect::SetValue(v) => {
                if let Some(e) = s.elements.iter_mut().find(|e| e.id == pressed_id) {
                    e.value = Some(v);
                }
            }
            Effect::RemoveSelf => {
                s.elements.retain(|e| e.id != pressed_id);
            }
            Effect::SetValueOf(target, v) => {
                let obs = Observation {
                    elements: s.elements.clone(),
                    ..Default::default()
                };
                if let Some(found) = dexter_world_model::find_elements(&obs, &target).first() {
                    let fid = found.id;
                    if let Some(e) = s.elements.iter_mut().find(|e| e.id == fid) {
                        e.value = Some(v);
                    }
                }
            }
        }
    }
}

impl ComputerDriver for SimDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "sim",
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Ok(self.state.lock().unwrap().windows.clone())
    }

    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        let mut obs = self.snapshot();
        if let Some(win) = scope.window {
            obs = dexter_world_model::within_window(&obs, win)
                .ok_or_else(|| DriverError::NotFound(format!("window {win}")))?;
        }
        obs.digest = dexter_world_model::digest(&obs, 250);
        let mut s = self.state.lock().unwrap();
        s.obs_cache.retain(|(id, _)| *id != obs.id);
        s.obs_cache.push_front((obs.id, obs.elements.clone()));
        s.obs_cache.truncate(4);
        Ok(obs)
    }

    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        match action {
            Action::Wait { millis } => Ok(ActionResult::success(
                Mechanism::NativeAutomation,
                Some(format!("waited {millis}ms")),
            )),
            Action::Observe => Err(DriverError::Unsupported(
                "Action::Observe is an engine directive".into(),
            )),
            Action::Navigate { url } => Ok(ActionResult::success(
                Mechanism::Api,
                Some(format!("navigated to {url}")),
            )),
            Action::Click { target, .. } => {
                if let Target::Point { x, y } = target {
                    if !ctx.allow_coordinates {
                        return Ok(ActionResult::failure(
                            ActionStatus::Unsupported,
                            Mechanism::Coordinates,
                            "coordinate input disabled",
                        ));
                    }
                    return Ok(ActionResult::success(
                        Mechanism::Coordinates,
                        Some(format!("clicked at ({x},{y})")),
                    ));
                }
                let id = self.resolve(target, ctx)?;
                {
                    let mut s = self.state.lock().unwrap();
                    s.pressed.push(id);
                }
                self.apply_effects(id);
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("pressed {id}")),
                ))
            }
            Action::TypeText { text, target } => {
                let t = target.clone().unwrap_or(Target::Focused);
                let id = self.resolve(&t, ctx)?;
                let mut s = self.state.lock().unwrap();
                let el = s
                    .elements
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or_else(|| DriverError::NotFound(format!("element {id}")))?;
                el.value = Some(text.clone());
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("typed into {id}")),
                ))
            }
            Action::SetValue { target, value } => {
                let id = self.resolve(target, ctx)?;
                let mut s = self.state.lock().unwrap();
                let el = s
                    .elements
                    .iter_mut()
                    .find(|e| e.id == id)
                    .ok_or_else(|| DriverError::NotFound(format!("element {id}")))?;
                el.value = Some(value.clone());
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("set value on {id}")),
                ))
            }
            Action::Focus { target } => {
                let id = self.resolve(target, ctx)?;
                let mut s = self.state.lock().unwrap();
                for e in s.elements.iter_mut() {
                    e.focused = e.id == id;
                }
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("focused {id}")),
                ))
            }
            Action::Key { .. } => {
                if !ctx.allow_coordinates {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Coordinates,
                        "key chords require physical input",
                    ));
                }
                Ok(ActionResult::success(
                    Mechanism::Coordinates,
                    Some("posted chord".into()),
                ))
            }
            Action::Scroll { target, .. } => {
                if let Some(t) = target {
                    let id = self.resolve(t, ctx)?;
                    return Ok(ActionResult::success(
                        Mechanism::Api,
                        Some(format!("scrolled {id} into view")),
                    ));
                }
                Ok(ActionResult::success(
                    Mechanism::Coordinates,
                    Some("scrolled".into()),
                ))
            }
        }
    }
}
