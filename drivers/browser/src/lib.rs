//! `BrowserDriver` — `ComputerDriver` over W3C WebDriver REST.
//!
//! The browser is the first *background-safe* real driver: DOM actions
//! (`el.click()`, `el.value=`) dispatch inside the page — no cursor, no
//! focus required, works headless or occluded. Coordinates are never
//! used: `Target::Point` reports `Unsupported` honestly.
//!
//! Element model: `walker.rs` runs a DOM walker in-page that emits
//! semantic nodes (ARIA roles, accessible names, bounds) and stashes
//! live node refs in `window.__dexterNodes`. `Target::Element` re-checks
//! identity against a fresh walk; `Target::Semantic` resolves through
//! the world model on a fresh observation — same contract as macOS.

mod walker;
mod webdriver;

use dexter_core::{
    Action, ActionResult, ActionStatus, AppSelector, Element, ElementId, ExecutionPlan,
    ExecutionRoute, Intrusiveness, Mechanism, Observation, ObservationId, ObservationScope, Rect,
    Sensitivity, Target, TargetDescriptor, Window,
};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
use serde_json::json;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;
use walker::WALKER_JS;
use webdriver::WebDriverClient;

pub struct BrowserDriver {
    client: Mutex<WebDriverClient>,
    /// Last N observations' element snapshots — element targets bind to
    /// the observation that produced them.
    /// (observation id, elements, tab handle) — element targets also
    /// carry the tab they were observed on.
    obs_cache: Mutex<VecDeque<(ObservationId, Vec<Element>, String)>>,
    next_observation: AtomicU64,
    /// Driver label reported in observations ("safari", "chrome", ...).
    label: String,
    /// WebDriver handle → stable Dexter `Window.id` for this driver
    /// instance (assigned lazily, never reused).
    handle_ids: Mutex<HashMap<String, u32>>,
}

impl BrowserDriver {
    /// Spawn `safaridriver` on a free port.
    pub fn safari() -> Result<Self, DriverError> {
        Self::from_client(WebDriverClient::safari()?, "safari")
    }

    /// Attach to an already-running WebDriver endpoint
    /// (chromedriver:9515, remote grid, ...). Opens a fresh session.
    pub fn connect(base_url: &str, label: &str) -> Result<Self, DriverError> {
        Self::from_client(WebDriverClient::connect(base_url)?, label)
    }

    /// Attach to an endpoint and adopt its live session if one exists —
    /// lets one-shot CLI commands see the page the user already has
    /// open. The adopted session is never closed on Drop.
    pub fn connect_attach(base_url: &str, label: &str) -> Result<Self, DriverError> {
        Self::from_client(WebDriverClient::connect_attach(base_url)?, label)
    }

    fn from_client(client: WebDriverClient, label: &str) -> Result<Self, DriverError> {
        Ok(Self {
            client: Mutex::new(client),
            obs_cache: Mutex::new(VecDeque::new()),
            next_observation: AtomicU64::new(1),
            label: label.to_string(),
            handle_ids: Mutex::new(HashMap::new()),
        })
    }

    /// Navigate the current session to `url`.
    pub fn navigate(&self, url: &str) -> Result<(), DriverError> {
        self.client.lock().unwrap().navigate(url)
    }

    /// Open a new tab; it becomes the active context per WebDriver spec.
    pub fn new_tab(&self) -> Result<u32, DriverError> {
        let handle = self.client.lock().unwrap().new_window()?;
        Ok(self.id_for_handle(&handle))
    }

    /// Close the current tab and switch to the first remaining one, if
    /// any. Returns `false` when the session has no windows left.
    pub fn close_tab(&self) -> Result<bool, DriverError> {
        let mut c = self.client.lock().unwrap();
        let remaining = c.close_window()?;
        match remaining.first() {
            Some(h) => {
                c.switch_to_window(h)?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Stable Dexter window id for a WebDriver handle.
    fn id_for_handle(&self, handle: &str) -> u32 {
        let mut map = self.handle_ids.lock().unwrap();
        if let Some(id) = map.get(handle) {
            return *id;
        }
        let id = map.len() as u32 + 1;
        map.insert(handle.to_string(), id);
        id
    }

    /// The WebDriver handle behind a Dexter `Window.id`, if known.
    fn handle_for_id(&self, id: u32) -> Option<String> {
        self.handle_ids
            .lock()
            .unwrap()
            .iter()
            .find(|(_, v)| **v == id)
            .map(|(h, _)| h.clone())
    }

    /// Run a script in the page (test/debug/CLI escape hatch).
    pub fn client_exec(
        &self,
        script: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, DriverError> {
        self.client.lock().unwrap().execute(script, args)
    }

    /// Fresh DOM walk → (element list, iframe collection errors).
    fn walk(&self) -> Result<(Vec<Element>, u32), DriverError> {
        let raw = self.client.lock().unwrap().execute(WALKER_JS, vec![])?;
        Ok(walker::parse_elements(raw))
    }

    /// Run `script` with the live DOM node for `id` as `arguments[0]`.
    /// The node comes from `window.__dexterNodes`, which `observe()`
    /// repopulates on every walk — callers must have a current
    /// observation.
    fn exec_on(&self, id: ElementId, script: &str) -> Result<serde_json::Value, DriverError> {
        self.client.lock().unwrap().execute(
            &format!(
                "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                 if (!el) return {{__dexter_err: 'stale node'}}; \
                 return (function(el) {{ {} }})(el); }})()",
                id.0, script
            ),
            vec![],
        )
    }

    fn cache_observation(&self, obs: &Observation, tab: &str) {
        let mut cache = self.obs_cache.lock().unwrap();
        cache.retain(|(id, _, _)| *id != obs.id);
        cache.push_front((obs.id, obs.elements.clone(), tab.to_string()));
        cache.truncate(4);
    }

    /// Resolve a `Target` to the element id within the *current* walk.
    fn resolve(&self, target: &Target) -> Result<ElementId, DriverError> {
        match target {
            Target::Element {
                observation,
                element,
            } => {
                let (stored, tab) = {
                    let cache = self.obs_cache.lock().unwrap();
                    let entry = cache.iter().find(|(id, _, _)| *id == *observation);
                    let (_, elements, tab) = entry.ok_or_else(|| {
                        DriverError::StaleReference(format!(
                            "observation {} is no longer held — re-observe",
                            observation.0
                        ))
                    })?;
                    let stored = dexter_driver::resolve::stored_element(
                        Some(elements.as_slice()),
                        *observation,
                        *element,
                    )?
                    .clone();
                    (stored, tab.clone())
                };
                // Fresh walk + identity check — DOMs mutate. The walk
                // must happen on the tab the observation came from.
                let current = self.client.lock().unwrap().current_window_handle()?;
                if current != tab {
                    return Err(DriverError::StaleReference(format!(
                        "observation {} belongs to another tab — switch to it \
                         (Target::Window) or re-observe",
                        observation.0
                    )));
                }
                let (fresh_els, _) = self.walk()?;
                dexter_driver::resolve::verify_identity(
                    &stored,
                    fresh_els.iter().find(|e| e.id == *element),
                    *observation,
                    *element,
                )?;
                Ok(*element)
            }
            Target::Semantic(_) | Target::Focused => {
                // Fresh observation + world-model resolution (ambiguous /
                // not-found fail closed, same as macOS).
                let obs = self.observe(&ObservationScope::default())?;
                let el = dexter_driver::resolve::resolve_semantic(&obs, target)?;
                Ok(el.id)
            }
            Target::Point { .. } | Target::Window { .. } => {
                Err(DriverError::Unsupported("target is not an element".into()))
            }
        }
    }
}

impl ComputerDriver for BrowserDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "browser",
            element_tree: true,
            screenshots: true,
            // DOM actions dispatch inside the page — no cursor, no focus.
            background_input: true,
        }
    }

    /// Every session tab is a window. Only the *active* tab carries
    /// title/url — reading the others would require an observable tab
    /// switch, so they honestly report `None` and `on_screen: false`.
    /// WebDriver handles are strings; `id` is a stable per-driver u32
    /// via `handle_ids`.
    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        let (handles, current, title, url) = {
            let mut c = self.client.lock().unwrap();
            (
                c.window_handles()?,
                c.current_window_handle().unwrap_or_default(),
                c.title().unwrap_or_default(),
                c.url().unwrap_or_default(),
            )
        };
        Ok(handles
            .iter()
            .map(|h| Window {
                id: self.id_for_handle(h),
                pid: 0,
                app: self.label.clone(),
                title: (*h == current).then(|| format!("{title} — {url}")),
                bounds: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 0.0,
                    h: 0.0,
                },
                on_screen: *h == current,
                layer: 0,
            })
            .collect())
    }

    /// Observations are per-tab: `scope.window` selects *which* tab to
    /// look at — switching to it first when needed (an observable API
    /// action, never coordinate input). Tabs are never flattened into
    /// one tree.
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        let mut windows = self.windows().unwrap_or_default();
        if let Some(win) = scope.window {
            let handle = self.handle_for_id(win).ok_or_else(|| {
                DriverError::NotFound(format!(
                    "window {win} — not a live browser tab id (re-observe for fresh ids)"
                ))
            })?;
            let mut c = self.client.lock().unwrap();
            if c.current_window_handle()? != handle {
                c.switch_to_window(&handle)?;
                for w in &mut windows {
                    w.on_screen = w.id == win;
                }
            }
        }
        let tab = self
            .client
            .lock()
            .unwrap()
            .current_window_handle()
            .unwrap_or_default();
        let (elements, errors) = self.walk()?;
        let screenshot = if scope.screenshot {
            match &scope.screenshot_path {
                Some(path) => {
                    self.client.lock().unwrap().screenshot(path)?;
                    Some(path.clone())
                }
                None => None,
            }
        } else {
            None
        };
        let truncated = elements.len() >= 4000;
        let mut obs = Observation {
            id: ObservationId(self.next_observation.fetch_add(1, Ordering::SeqCst)),
            timestamp: SystemTime::now(),
            app: Some(AppSelector::Name(self.label.clone())),
            pid: None,
            windows,
            elements,
            elements_truncated: truncated,
            collection_errors: errors,
            ax_limited: false, // DOM has no degraded-grant mode
            screenshot,
            digest: String::new(),
        };
        obs.digest = dexter_world_model::digest(&obs, 250);
        self.cache_observation(&obs, &tab);
        Ok(obs)
    }

    /// Routes mirror `act`: everything dispatches through the DOM —
    /// including `Key` chords and unscoped `Scroll`, which the action
    /// shape classes as physical but here are honestly background.
    /// `Target::Point` has no route at all: this driver never emits
    /// coordinates. `execute` forwards the authorized route to `act`,
    /// so a declared mechanism is always what the act reports.
    fn plan(&self, action: &Action, _ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        let single = |mechanism, intrusiveness, desc: TargetDescriptor| {
            ExecutionPlan::single(
                action,
                ExecutionRoute {
                    action: action.clone(),
                    target: desc,
                    mechanism: Some(mechanism),
                    intrusiveness,
                    sensitivity: Sensitivity::Standard,
                    requires_foreground: false,
                },
            )
        };
        let dom = |desc| single(Mechanism::Dom, Intrusiveness::Background, desc);
        let plan = match action {
            Action::Wait { .. } => single(
                Mechanism::Api,
                Intrusiveness::Background,
                TargetDescriptor::from_action(action),
            ),
            Action::Navigate { .. } => single(
                Mechanism::Dom,
                Intrusiveness::Visual,
                TargetDescriptor::from_action(action),
            ),
            Action::Click {
                target: Target::Point { .. },
                ..
            } => ExecutionPlan {
                requested: action.clone(),
                routes: vec![],
            },
            Action::Click { .. } | Action::SetValue { .. } => {
                dom(TargetDescriptor::from_action(action))
            }
            Action::Focus {
                target: Target::Window { .. },
            } => single(
                Mechanism::Api,
                Intrusiveness::Visual,
                TargetDescriptor::from_action(action),
            ),
            Action::Focus { .. } => dom(TargetDescriptor::from_action(action)),
            Action::TypeText { target, .. } => {
                let t = target.clone().unwrap_or(Target::Focused);
                dom(TargetDescriptor::from_target(Some(&t)))
            }
            Action::Key { .. } | Action::Scroll { .. } => {
                dom(TargetDescriptor::from_action(action))
            }
            Action::Observe => ExecutionPlan::legacy(action),
        };
        Ok(plan)
    }

    fn act(&self, action: &Action, _ctx: &ActContext) -> Result<ActionResult, DriverError> {
        match action {
            Action::Wait { millis } => {
                std::thread::sleep(std::time::Duration::from_millis(*millis));
                Ok(ActionResult::success(
                    Mechanism::Api,
                    Some(format!("waited {millis}ms")),
                ))
            }
            Action::Observe => Err(DriverError::Unsupported(
                "Action::Observe is an engine directive".into(),
            )),
            Action::Navigate { url } => {
                self.client.lock().unwrap().navigate(url)?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("navigated to {url}")),
                ))
            }
            Action::Click { target, button } => {
                if let Target::Point { .. } = target {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Coordinates,
                        "browser driver never uses coordinates — target elements",
                    ));
                }
                let id = self.resolve(target)?;
                let js = match button {
                    dexter_core::MouseButton::Right => {
                        "el.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true})); 'right-clicked'"
                    }
                    _ => "el.click(); 'clicked'",
                };
                let resp = self.exec_on(id, js)?;
                if resp["__dexter_err"].is_string() {
                    return Err(DriverError::StaleReference("stale node".into()));
                }
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("clicked element {id}")),
                ))
            }
            Action::Focus { target } => {
                // A window target is a tab switch — an observable API
                // action, not pointer input.
                if let Target::Window { window_id } = target {
                    let handle = self.handle_for_id(*window_id).ok_or_else(|| {
                        DriverError::NotFound(format!(
                            "window {window_id} — not a live browser tab id"
                        ))
                    })?;
                    self.client.lock().unwrap().switch_to_window(&handle)?;
                    return Ok(ActionResult::success(
                        Mechanism::Api,
                        Some(format!("switched to tab (window {window_id})")),
                    ));
                }
                let id = self.resolve(target)?;
                let resp = self.exec_on(id, "el.focus(); 'focused'")?;
                if resp["__dexter_err"].is_string() {
                    return Err(DriverError::StaleReference("stale node".into()));
                }
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("focused element {id}")),
                ))
            }
            Action::SetValue { target, value } => {
                let id = self.resolve(target)?;
                let resp = self.client.lock().unwrap().execute(
                    &format!(
                        "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                         if (!el) return {{__dexter_err:'stale node'}}; \
                         el.focus(); el.value = arguments[0]; \
                         el.dispatchEvent(new Event('input',{{bubbles:true}})); \
                         el.dispatchEvent(new Event('change',{{bubbles:true}})); \
                         return 'set'; }})()",
                        id.0
                    ),
                    vec![json!(value)],
                )?;
                if resp["__dexter_err"].is_string() {
                    return Err(DriverError::StaleReference("stale node".into()));
                }
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("set value on element {id}")),
                ))
            }
            Action::TypeText { text, target } => {
                let t = target.clone().unwrap_or(Target::Focused);
                let id = self.resolve(&t)?;
                self.client.lock().unwrap().execute(
                    &format!(
                        "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                         if (!el) return {{__dexter_err:'stale node'}}; \
                         el.focus(); el.value = (el.value||'') + arguments[0]; \
                         el.dispatchEvent(new Event('input',{{bubbles:true}})); \
                         return 'typed'; }})()",
                        id.0
                    ),
                    vec![json!(text)],
                )?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("typed into element {id}")),
                ))
            }
            Action::Key { chord } => {
                // DOM key dispatch to the focused element — no physical
                // keyboard, no focus stealing. Deliberately not the
                // WebDriver /actions endpoint (that's a *different*
                // mechanism ladder for later).
                self.client.lock().unwrap().execute(
                    "return (() => { const el = document.activeElement || document.body; \
                     const init = {key: arguments[0], bubbles:true, \
                        metaKey: arguments[1], ctrlKey: arguments[2], \
                        altKey: arguments[3], shiftKey: arguments[4]}; \
                     el.dispatchEvent(new KeyboardEvent('keydown', init)); \
                     el.dispatchEvent(new KeyboardEvent('keyup', init)); \
                     return 'key'; })()"
                        .to_string()
                        .as_str(),
                    vec![
                        json!(chord.key),
                        json!(chord.modifiers.iter().any(|m| m == "cmd" || m == "meta")),
                        json!(chord.modifiers.iter().any(|m| m == "ctrl")),
                        json!(chord.modifiers.iter().any(|m| m == "alt")),
                        json!(chord.modifiers.iter().any(|m| m == "shift")),
                    ],
                )?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("dispatched key {}", chord.key)),
                ))
            }
            Action::Scroll { delta, target } => {
                if let Some(t) = target {
                    let id = self.resolve(t)?;
                    self.exec_on(id, "el.scrollIntoView({block:'center'}); 'scrolled'")?;
                    return Ok(ActionResult::success(
                        Mechanism::Dom,
                        Some(format!("scrolled element {id} into view")),
                    ));
                }
                self.client.lock().unwrap().execute(
                    "window.scrollBy(arguments[0], arguments[1]); return 'scrolled';",
                    vec![json!(delta.dx), json!(delta.dy)],
                )?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some("scrolled window".into()),
                ))
            }
        }
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
}
