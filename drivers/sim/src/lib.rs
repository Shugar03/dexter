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
    Action, ActionResult, ActionStatus, Element, ElementId, ExecutionPlan, ExecutionRoute,
    Intrusiveness, Mechanism, Observation, ObservationId, ObservationScope, SemanticTarget,
    Sensitivity, Target, TargetDescriptor, Window,
};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError, WakeHandle};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

/// A world mutation applied after a successful press on a matching element,
/// or on every observe via [`SimDriver::on_tick`].
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
    /// Set `enabled` on the first element matching the target — models
    /// "the checkbox enables the Continue button".
    SetEnabledOf(SemanticTarget, bool),
    /// Remove the first element matching the target — models the world
    /// changing under the agent between observations.
    Remove(SemanticTarget),
    /// Advance through `values` one step per application, then hold the
    /// last value — models progress completing while the agent waits.
    CycleValueOf(SemanticTarget, Vec<String>),
}

struct Rule {
    when: SemanticTarget,
    effect: Effect,
    /// Consumed position for `CycleValueOf` (also counts applications).
    cursor: usize,
}

struct SimState {
    elements: Vec<Element>,
    windows: Vec<Window>,
    next_element_id: u64,
    /// Audit trail — which elements were pressed, in order.
    pressed: Vec<ElementId>,
    /// Click counts parallel to `pressed`.
    counts: Vec<u8>,
    /// `(element, action)` pairs performed through `Invoke`.
    invoked: Vec<(ElementId, String)>,
    /// `(from, to)` element pairs dragged, in order.
    dragged: Vec<(ElementId, ElementId)>,
    /// The simulated pasteboard — plain text only.
    clipboard: String,
    /// Names of running apps; launching spawns a window, quitting
    /// removes them. The sim itself runs as "sim".
    apps: Vec<String>,
    obs_cache: VecDeque<(ObservationId, Vec<Element>)>,
    rules: Vec<Rule>,
    /// Effects applied on every `observe()` — worlds that evolve while
    /// the agent looks (downloads finishing, elements vanishing).
    ticks: Vec<Rule>,
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
                counts: Vec::new(),
                invoked: Vec::new(),
                dragged: Vec::new(),
                clipboard: String::new(),
                apps: vec!["sim".into()],
                obs_cache: VecDeque::new(),
                rules: Vec::new(),
                ticks: Vec::new(),
            }),
            next_observation: AtomicU64::new(1),
        }
    }

    /// When an element matching `when` is pressed, apply `effect`.
    pub fn on_press(&self, when: SemanticTarget, effect: Effect) {
        self.state.lock().unwrap().rules.push(Rule {
            when,
            effect,
            cursor: 0,
        });
    }

    /// Apply `effect` on every `observe()` — a world that evolves while
    /// the agent re-observes (progress bars finishing, elements
    /// vanishing). The effect's own target selects what mutates;
    /// press-bound effects (`SetValue`, `RemoveSelf`) are meaningless here.
    pub fn on_tick(&self, effect: Effect) {
        self.state.lock().unwrap().ticks.push(Rule {
            when: SemanticTarget::default(),
            effect,
            cursor: 0,
        });
    }

    /// Add a window to the simulated world — tests window scoping.
    pub fn add_window(&self, window: Window) {
        self.state.lock().unwrap().windows.push(window);
    }

    /// Elements pressed so far — test observability hook.
    pub fn pressed(&self) -> Vec<ElementId> {
        self.state.lock().unwrap().pressed.clone()
    }

    /// Click counts parallel to `pressed` — test observability hook.
    pub fn click_counts(&self) -> Vec<u8> {
        self.state.lock().unwrap().counts.clone()
    }

    /// `(element, action)` pairs invoked — test observability hook.
    pub fn invoked(&self) -> Vec<(ElementId, String)> {
        self.state.lock().unwrap().invoked.clone()
    }

    /// `(from, to)` pairs dragged — test observability hook.
    pub fn dragged(&self) -> Vec<(ElementId, ElementId)> {
        self.state.lock().unwrap().dragged.clone()
    }

    /// Current pasteboard text — test observability hook.
    pub fn clipboard(&self) -> String {
        self.state.lock().unwrap().clipboard.clone()
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
                let stored = s
                    .obs_cache
                    .iter()
                    .find(|(id, _)| *id == *observation)
                    .map(|(_, els)| els.as_slice());
                dexter_driver::resolve::resolve_element_ref(
                    stored,
                    &s.elements,
                    *observation,
                    *element,
                )?;
                Ok(*element)
            }
            Target::Semantic(_) | Target::Focused => {
                let obs = self.snapshot();
                let el = dexter_driver::resolve::resolve_semantic(&obs, target)?;
                Ok(el.id)
            }
            Target::Point { .. } | Target::Window { .. } => {
                Err(DriverError::Unsupported("target is not an element".into()))
            }
        }
    }

    /// Mutate the first element matching `target` with `f`.
    fn mutate_first(
        elements: &mut [Element],
        target: &SemanticTarget,
        f: impl FnOnce(&mut Element),
    ) {
        let obs = Observation {
            elements: elements.to_vec(),
            ..Default::default()
        };
        if let Some(found) = dexter_world_model::find_elements(&obs, target).first() {
            let fid = found.id;
            if let Some(e) = elements.iter_mut().find(|e| e.id == fid) {
                f(e);
            }
        }
    }

    /// First element matching `target` — for effects that remove rather
    /// than mutate.
    fn first_match(elements: &[Element], target: &SemanticTarget) -> Option<ElementId> {
        let obs = Observation {
            elements: elements.to_vec(),
            ..Default::default()
        };
        dexter_world_model::find_elements(&obs, target)
            .first()
            .map(|e| e.id)
    }

    /// Apply one effect against the world. `pressed` is the element the
    /// rule fired on — `SetValue`/`RemoveSelf` act on it; `*Of` effects
    /// resolve their own target.
    fn apply_effect(s: &mut SimState, effect: &Effect, pressed: ElementId, cursor: usize) {
        match effect {
            Effect::Spawn(el) => {
                let mut el = el.clone();
                el.id = ElementId(s.next_element_id);
                s.next_element_id += 1;
                s.elements.push(el);
            }
            Effect::SetValue(v) => {
                if let Some(e) = s.elements.iter_mut().find(|e| e.id == pressed) {
                    e.value = Some(v.clone());
                }
            }
            Effect::RemoveSelf => {
                s.elements.retain(|e| e.id != pressed);
            }
            Effect::SetValueOf(target, v) => {
                Self::mutate_first(&mut s.elements, target, |e| e.value = Some(v.clone()));
            }
            Effect::SetEnabledOf(target, enabled) => {
                Self::mutate_first(&mut s.elements, target, |e| e.enabled = Some(*enabled));
            }
            Effect::Remove(target) => {
                if let Some(id) = Self::first_match(&s.elements, target) {
                    s.elements.retain(|e| e.id != id);
                }
            }
            Effect::CycleValueOf(target, values) => {
                if let Some(v) = values.get(cursor.min(values.len().saturating_sub(1))) {
                    Self::mutate_first(&mut s.elements, target, |e| e.value = Some(v.clone()));
                }
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
        let idx = s.rules.iter().position(|r| {
            let one = Observation {
                elements: vec![pressed_el.clone()],
                ..Default::default()
            };
            !dexter_world_model::find_elements(&one, &r.when).is_empty()
        });
        let Some(idx) = idx else { return };
        let cursor = s.rules[idx].cursor;
        let effect = s.rules[idx].effect.clone();
        s.rules[idx].cursor += 1;
        Self::apply_effect(&mut s, &effect, pressed_id, cursor);
    }

    /// Tick rules fire on every observe — before the snapshot is taken,
    /// so the agent sees the evolved world immediately.
    fn apply_ticks(&self) {
        let mut s = self.state.lock().unwrap();
        for i in 0..s.ticks.len() {
            let cursor = s.ticks[i].cursor;
            let effect = s.ticks[i].effect.clone();
            s.ticks[i].cursor += 1;
            // Tick effects resolve their own targets; `pressed` is unused
            // by the `*Of`/`Remove`/`CycleValueOf` variants.
            Self::apply_effect(&mut s, &effect, ElementId(0), cursor);
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
        self.apply_ticks();
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
            Action::Click { target, count, .. } => {
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
                        Some(format!("clicked x{count} at ({x},{y})")),
                    ));
                }
                let id = self.resolve(target, ctx)?;
                {
                    let mut s = self.state.lock().unwrap();
                    s.pressed.push(id);
                    s.counts.push(*count);
                }
                self.apply_effects(id);
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("pressed {id} x{count}")),
                ))
            }
            Action::Invoke { target, action } => {
                let id = self.resolve(target, ctx)?;
                {
                    let mut s = self.state.lock().unwrap();
                    let el = s
                        .elements
                        .iter()
                        .find(|e| e.id == id)
                        .ok_or_else(|| DriverError::NotFound(format!("element {id}")))?;
                    if !el.actions.iter().any(|a| a == action) {
                        return Ok(ActionResult::failure(
                            ActionStatus::Unsupported,
                            Mechanism::Accessibility,
                            format!("element {id} does not advertise '{action}'"),
                        ));
                    }
                    s.invoked.push((id, action.clone()));
                }
                self.apply_effects(id);
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("invoked '{action}' on {id}")),
                ))
            }
            Action::LaunchApp { app, activate } => {
                let name = match app {
                    dexter_core::AppSelector::Name(n) => n.clone(),
                    dexter_core::AppSelector::BundleId(b) => b.clone(),
                    dexter_core::AppSelector::Pid(_) => {
                        return Err(DriverError::Unsupported(
                            "cannot launch an app by pid".into(),
                        ));
                    }
                };
                let mut s = self.state.lock().unwrap();
                if !s.apps.iter().any(|a| a == &name) {
                    s.apps.push(name.clone());
                }
                let id = s.windows.iter().map(|w| w.id).max().unwrap_or(0) + 1;
                let win = Window {
                    id,
                    pid: (s.apps.len() + 1) as i32,
                    app: name.clone(),
                    title: Some(name.clone()),
                    bounds: dexter_core::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 800.0,
                        h: 600.0,
                    },
                    on_screen: true,
                    layer: 0,
                };
                if *activate {
                    s.windows.insert(0, win); // frontmost
                } else {
                    s.windows.push(win);
                }
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("launched {name}")),
                ))
            }
            Action::QuitApp { app } => {
                let name = match app {
                    dexter_core::AppSelector::Name(n) => n.clone(),
                    dexter_core::AppSelector::BundleId(b) => b.clone(),
                    dexter_core::AppSelector::Pid(_) => {
                        return Err(DriverError::Unsupported("cannot quit an app by pid".into()));
                    }
                };
                let mut s = self.state.lock().unwrap();
                if !s.apps.iter().any(|a| a == &name) {
                    return Err(DriverError::NotFound(format!("app '{name}' not running")));
                }
                s.apps.retain(|a| a != &name);
                s.windows.retain(|w| w.app != name);
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("quit {name}")),
                ))
            }
            Action::Window {
                window_id,
                operation,
            } => {
                use dexter_core::WindowOperation as Op;
                let mut s = self.state.lock().unwrap();
                match operation {
                    Op::New => {
                        let id = s.windows.iter().map(|w| w.id).max().unwrap_or(0) + 1;
                        let app = s
                            .windows
                            .first()
                            .map(|w| w.app.clone())
                            .unwrap_or_else(|| "sim".into());
                        s.windows.insert(
                            0,
                            Window {
                                id,
                                pid: 1,
                                app: app.clone(),
                                title: Some(format!("{app} {id}")),
                                bounds: dexter_core::Rect {
                                    x: 0.0,
                                    y: 0.0,
                                    w: 800.0,
                                    h: 600.0,
                                },
                                on_screen: true,
                                layer: 0,
                            },
                        );
                    }
                    op => {
                        let pos = match window_id {
                            Some(id) => s
                                .windows
                                .iter()
                                .position(|w| w.id == *id)
                                .ok_or_else(|| DriverError::NotFound(format!("window {id}")))?,
                            None => 0,
                        };
                        match op {
                            Op::Focus | Op::Raise => {
                                let w = s.windows.remove(pos);
                                s.windows.insert(0, w);
                            }
                            Op::Close => {
                                s.windows.remove(pos);
                            }
                            Op::Minimize => s.windows[pos].on_screen = false,
                            Op::Restore => s.windows[pos].on_screen = true,
                            Op::Move { x, y } => {
                                s.windows[pos].bounds.x = *x;
                                s.windows[pos].bounds.y = *y;
                            }
                            Op::Resize { width, height } => {
                                s.windows[pos].bounds.w = *width;
                                s.windows[pos].bounds.h = *height;
                            }
                            Op::New => unreachable!(),
                        }
                    }
                }
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("window {operation:?}")),
                ))
            }
            Action::ReadClipboardText => {
                let text = self.state.lock().unwrap().clipboard.clone();
                Ok(ActionResult::success(
                    Mechanism::Api,
                    if text.is_empty() {
                        Some("clipboard empty".into())
                    } else {
                        Some(text)
                    },
                ))
            }
            Action::WriteClipboardText { text } => {
                if text.len() > 1024 * 1024 {
                    return Err(DriverError::Unsupported("clipboard payload > 1 MiB".into()));
                }
                self.state.lock().unwrap().clipboard = text.clone();
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
                let a = self.resolve(from, ctx)?;
                let b = self.resolve(to, ctx)?;
                {
                    let mut s = self.state.lock().unwrap();
                    s.dragged.push((a, b));
                }
                // The drop target receives the effect — the world's rule
                // decides what a drop onto it means.
                self.apply_effects(b);
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("dragged {a} onto {b} in {duration_ms}ms")),
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
                // v2 semantics: typing inserts/appends — `SetValue` is
                // the replace verb.
                match &mut el.value {
                    Some(v) => v.push_str(text),
                    None => el.value = Some(text.clone()),
                }
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
            Action::Focus {
                target: Target::Window { window_id },
            } => {
                // v2 normalization: focusing a window is a window op.
                let mut s = self.state.lock().unwrap();
                let pos = s
                    .windows
                    .iter()
                    .position(|w| w.id == *window_id)
                    .ok_or_else(|| DriverError::NotFound(format!("window {window_id}")))?;
                let w = s.windows.remove(pos);
                s.windows.insert(0, w);
                Ok(ActionResult::success(
                    Mechanism::Accessibility,
                    Some(format!("focused window {window_id}")),
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

    /// Read-only route declaration: one route per action, its mechanism
    /// exactly what `act` reports. `resolve` reaches element-shaped
    /// targets only — point and window targets are categorically
    /// unresolvable, so they declare no route; an empty plan is the
    /// honest "unsupported" and the engine still judges the action's
    /// own tier, keeping refusal a policy verdict. Coordinate paths
    /// exist only under `allow_coordinates`. `Observe` stays legacy:
    /// `act` rejects it as an engine directive.
    fn plan(&self, action: &Action, ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        let resolvable = |t: &Target| {
            matches!(
                t,
                Target::Element { .. } | Target::Semantic(_) | Target::Focused
            )
        };
        let route = |mechanism: Mechanism,
                     intrusiveness: Intrusiveness,
                     requires_foreground: bool| ExecutionRoute {
            action: action.clone(),
            target: TargetDescriptor::from_action(action),
            mechanism: Some(mechanism),
            intrusiveness,
            sensitivity: Sensitivity::Standard,
            requires_foreground,
        };
        let api = || route(Mechanism::Api, Intrusiveness::Background, false);
        let coords = || route(Mechanism::Coordinates, Intrusiveness::Physical, true);
        let routes = match action {
            Action::Wait { .. } => {
                vec![route(
                    Mechanism::NativeAutomation,
                    Intrusiveness::Background,
                    false,
                )]
            }
            Action::Navigate { .. } => {
                vec![route(Mechanism::Api, Intrusiveness::Visual, false)]
            }
            Action::Click { target, .. } => match target {
                Target::Point { .. } if ctx.allow_coordinates => vec![coords()],
                t if resolvable(t) => vec![api()],
                _ => vec![],
            },
            Action::Key { .. } if ctx.allow_coordinates => vec![coords()],
            Action::Key { .. } => vec![],
            Action::Scroll { target, .. } => match target {
                Some(t) if resolvable(t) => vec![api()],
                Some(_) => vec![],
                // `act` scrolls by coordinates unconditionally — the
                // route is declared whether or not coordinates were
                // opted into, so policy sees the physical tier.
                None => vec![coords()],
            },
            // `act` types into `Target::Focused` when no target is given —
            // the descriptor mirrors that resolution.
            Action::TypeText { target, .. } if target.as_ref().is_none_or(resolvable) => {
                let mut r = api();
                if target.is_none() {
                    r.target = TargetDescriptor::from_target(Some(&Target::Focused));
                }
                vec![r]
            }
            Action::TypeText { .. } => vec![],
            Action::SetValue { target, .. } | Action::Focus { target } if resolvable(target) => {
                vec![api()]
            }
            // `Focus` on a window target is a window op in v2 — AX-level,
            // visible but not input-capturing.
            Action::Focus {
                target: Target::Window { .. },
            }
            | Action::Window { .. } => vec![route(
                Mechanism::Accessibility,
                Intrusiveness::Visual,
                false,
            )],
            Action::SetValue { .. } | Action::Focus { .. } => vec![],
            Action::Invoke { target, .. } if resolvable(target) => vec![route(
                Mechanism::Accessibility,
                Intrusiveness::Background,
                false,
            )],
            Action::Invoke { .. } => vec![],
            Action::LaunchApp { .. } | Action::QuitApp { .. } => {
                vec![route(Mechanism::Api, Intrusiveness::Visual, false)]
            }
            // Clipboard is semantic but secret-bearing — the sensitivity
            // floor travels on the route so policy can gate it alone.
            Action::ReadClipboardText | Action::WriteClipboardText { .. } => {
                let mut r = route(Mechanism::Api, Intrusiveness::Background, false);
                r.sensitivity = Sensitivity::Secrets;
                vec![r]
            }
            Action::Drag { from, to, .. } => {
                if resolvable(from) && resolvable(to) {
                    vec![route(
                        Mechanism::Accessibility,
                        Intrusiveness::Background,
                        false,
                    )]
                } else {
                    vec![]
                }
            }
            Action::Observe => return Ok(ExecutionPlan::legacy(action)),
        };
        Ok(ExecutionPlan {
            requested: action.clone(),
            routes,
        })
    }

    /// The authorized route's action is exactly what `act` performs —
    /// the declared mechanism already mirrors its report.
    fn execute(
        &self,
        route: &ExecutionRoute,
        ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        self.act(&route.action, ctx)
    }

    /// Wake means "the app is on stage": like the real driver, a
    /// stopped app gains a window. Sim has no focus to restore, so the
    /// handle stays inert.
    fn wake(&self, app: &dexter_core::AppSelector) -> Result<WakeHandle, DriverError> {
        let name = match app {
            dexter_core::AppSelector::Name(n) | dexter_core::AppSelector::BundleId(n) => n.clone(),
            dexter_core::AppSelector::Pid(_) => return Ok(WakeHandle::default()),
        };
        let mut s = self.state.lock().unwrap();
        if !s.apps.iter().any(|a| a == &name) {
            let id = s.windows.iter().map(|w| w.id).max().unwrap_or(0) + 1;
            let pid = (s.apps.len() + 2) as i32;
            s.apps.push(name.clone());
            s.windows.insert(
                0,
                Window {
                    id,
                    pid,
                    app: name.clone(),
                    title: Some(name),
                    bounds: dexter_core::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 800.0,
                        h: 600.0,
                    },
                    on_screen: true,
                    layer: 0,
                },
            );
        }
        Ok(WakeHandle::default())
    }
}
