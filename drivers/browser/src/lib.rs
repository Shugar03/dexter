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
        self.exec_on_args(id, script, vec![])
    }

    /// `exec_on` with extra args, exposed to the script as `a[0], a[1]…`.
    /// The WebDriver `args` array becomes `arguments` of the script's
    /// outer function; the inner call receives them after the bound
    /// element — scripts must use `a[i]`, never `arguments[i]` (inside
    /// a *regular* function `arguments` is that function's own list).
    ///
    /// The `__dexter_err` sentinel is converted to `StaleReference`
    /// here, not at call sites — a node gone between resolve and
    /// dispatch can never simulate success.
    fn exec_on_args(
        &self,
        id: ElementId,
        script: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, DriverError> {
        let resp = self.client.lock().unwrap().execute(
            &format!(
                "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                 if (!el) return {{__dexter_err: 'stale node'}}; \
                 return (function(el, ...a) {{ {} }}).apply(null, [el].concat(Array.from(arguments))); }})()",
                id.0, script
            ),
            args,
        )?;
        if resp["__dexter_err"].is_string() {
            return Err(DriverError::StaleReference("stale node".into()));
        }
        Ok(resp)
    }

    /// The actions the live element advertises — looked up in the
    /// observation the id was minted from. Element ids are per-
    /// observation sequential handles; searching across cached
    /// observations could match a same-id element from a stale
    /// snapshot and validate against the wrong world.
    fn element_actions(
        &self,
        observation: ObservationId,
        id: ElementId,
    ) -> Result<Vec<String>, DriverError> {
        let cache = self.obs_cache.lock().unwrap();
        Ok(cache
            .iter()
            .find(|(obs_id, _, _)| *obs_id == observation)
            .and_then(|(_, els, _)| els.iter().find(|e| e.id == id))
            .map(|e| e.actions.clone())
            .unwrap_or_default())
    }

    fn cache_observation(&self, obs: &Observation, tab: &str) {
        let mut cache = self.obs_cache.lock().unwrap();
        cache.retain(|(id, _, _)| *id != obs.id);
        cache.push_front((obs.id, obs.elements.clone(), tab.to_string()));
        cache.truncate(4);
    }

    /// Resolve a `Target` to the element id and the observation that
    /// minted it — callers checking observation-scoped state (e.g.
    /// advertised actions) must pin that same observation.
    fn resolve(&self, target: &Target) -> Result<(ElementId, ObservationId), DriverError> {
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
                Ok((*element, *observation))
            }
            Target::Semantic(_) | Target::Focused => {
                // Fresh observation + world-model resolution (ambiguous /
                // not-found fail closed, same as macOS).
                let obs = self.observe(&ObservationScope::default())?;
                let el = dexter_driver::resolve::resolve_semantic(&obs, target)?;
                Ok((el.id, obs.id))
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
            Action::Invoke { .. } => dom(TargetDescriptor::from_action(action)),
            // act() reports Unsupported — no honest route exists.
            Action::LaunchApp { .. }
            | Action::QuitApp { .. }
            | Action::ReadClipboardText
            | Action::WriteClipboardText { .. } => ExecutionPlan {
                requested: action.clone(),
                routes: vec![],
            },
            Action::Window { operation, .. } => {
                use dexter_core::WindowOperation as Op;
                match operation {
                    Op::New | Op::Focus | Op::Close => single(
                        Mechanism::Api,
                        Intrusiveness::Visual,
                        TargetDescriptor::from_action(action),
                    ),
                    _ => ExecutionPlan {
                        requested: action.clone(),
                        routes: vec![],
                    },
                }
            }
            Action::Drag { .. } => dom(TargetDescriptor::from_action(action)),
            Action::Observe => ExecutionPlan::legacy(action),
        };
        // Element-handle routes carry only the minted id — fill the
        // semantic identity from the cached observation so a grant or
        // audit line reads "button Pay now", not "element 3".
        let mut plan = plan;
        for route in &mut plan.routes {
            if let (Some(obs), Some(el)) = (route.target.observation, route.target.element) {
                let cache = self.obs_cache.lock().unwrap();
                if let Some(found) = cache
                    .iter()
                    .find(|(id, _, _)| *id == obs)
                    .and_then(|(_, els, _)| els.iter().find(|e| e.id == el))
                {
                    route.target.enrich_element(found);
                }
            }
        }
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
                    // Query strings carry signed tokens — the detail is
                    // journaled, so it reports the redacted form.
                    Some(format!("navigated to {}", dexter_core::redact_url(url))),
                ))
            }
            Action::Click {
                target,
                button,
                count,
            } => {
                if let Target::Point { .. } = target {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Coordinates,
                        "browser driver never uses coordinates — target elements",
                    ));
                }
                if *count == 0 || *count > 3 {
                    return Ok(ActionResult::failure(
                        ActionStatus::Failed,
                        Mechanism::Dom,
                        format!("click count {count} out of range 1..=3"),
                    ));
                }
                // A context menu is a single semantic event — there is
                // no "double right-click"; fail closed rather than
                // silently degrading to one show_menu.
                if !matches!(button, dexter_core::MouseButton::Left) && *count > 1 {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Dom,
                        format!("multi-click count {count} only applies to the left button"),
                    ));
                }
                let (id, _) = self.resolve(target)?;
                let js = match (button, *count) {
                    (dexter_core::MouseButton::Right, _) => {
                        "el.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true})); 'right-clicked'"
                    }
                    (_, 1) => "el.click(); 'clicked'",
                    // A real double-click sequence: mousedown/up pairs
                    // plus the dblclick event apps listen for.
                    (_, 2..=3) => {
                        "for(let i=0;i<a[0];i++){el.dispatchEvent(new MouseEvent('mousedown',{bubbles:true}));el.dispatchEvent(new MouseEvent('mouseup',{bubbles:true}));el.click();}\
                         el.dispatchEvent(new MouseEvent('dblclick',{bubbles:true})); 'multi-clicked'"
                    }
                    _ => "el.click(); 'clicked'",
                };
                self.exec_on_args(id, js, vec![json!(count)])?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("clicked element {id} x{count}")),
                )
                .with_element(Some(id)))
            }
            Action::Invoke { target, action } => {
                let (id, obs_id) = self.resolve(target)?;
                // DOM/API mapping — only names the element advertises.
                let js = match action.as_str() {
                    "press" | "open" => "el.click(); 'invoked'",
                    "show_menu" => {
                        "el.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true})); 'menu'"
                    }
                    "focus" => "el.focus(); 'focused'",
                    "scroll_into_view" => "el.scrollIntoView({block:'center'}); 'scrolled'",
                    _ => {
                        return Ok(ActionResult::failure(
                            ActionStatus::Unsupported,
                            Mechanism::Dom,
                            format!("no DOM mapping for invoke '{action}'"),
                        ));
                    }
                };
                // The element must still advertise the action — same
                // fail-closed contract as AX, checked against the
                // observation the resolved id was minted from.
                let advertised = self.element_actions(obs_id, id)?;
                if !advertised.iter().any(|a| a == action) {
                    return Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Dom,
                        format!("element {id} does not advertise '{action}'"),
                    ));
                }
                self.exec_on(id, js)?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("invoked '{action}' on element {id}")),
                )
                .with_element(Some(id)))
            }
            Action::LaunchApp { .. } | Action::QuitApp { .. } => Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Dom,
                "browser driver cannot manage application lifecycle",
            )),
            Action::Window {
                window_id,
                operation,
            } => {
                use dexter_core::WindowOperation as Op;
                match operation {
                    Op::New => {
                        let handle = self.client.lock().unwrap().new_window()?;
                        Ok(ActionResult::success(
                            Mechanism::Api,
                            Some(format!("opened tab {handle}")),
                        ))
                    }
                    Op::Focus => {
                        let id = window_id.ok_or_else(|| {
                            DriverError::NotFound("browser window focus needs a window_id".into())
                        })?;
                        let handle = self.handle_for_id(id).ok_or_else(|| {
                            DriverError::NotFound(format!("window {id} — not a live tab"))
                        })?;
                        self.client.lock().unwrap().switch_to_window(&handle)?;
                        Ok(ActionResult::success(
                            Mechanism::Api,
                            Some(format!("switched to tab (window {id})")),
                        ))
                    }
                    Op::Close => {
                        if let Some(id) = window_id {
                            let handle = self.handle_for_id(*id).ok_or_else(|| {
                                DriverError::NotFound(format!("window {id} — not a live tab"))
                            })?;
                            self.client.lock().unwrap().switch_to_window(&handle)?;
                        }
                        self.client.lock().unwrap().close_window()?;
                        Ok(ActionResult::success(
                            Mechanism::Api,
                            Some(format!("closed tab {window_id:?}")),
                        ))
                    }
                    _ => Ok(ActionResult::failure(
                        ActionStatus::Unsupported,
                        Mechanism::Dom,
                        format!("window op {operation:?} has no browser mapping"),
                    )),
                }
            }
            Action::ReadClipboardText | Action::WriteClipboardText { .. } => {
                Ok(ActionResult::failure(
                    ActionStatus::Unsupported,
                    Mechanism::Dom,
                    "WebDriver has no clipboard API — clipboard is unsupported here",
                ))
            }
            Action::Drag {
                from,
                to,
                duration_ms: _,
            } => {
                // DOM event synthesis — never moves the OS cursor.
                let (a, _) = self.resolve(from)?;
                let (b, _) = self.resolve(to)?;
                self.exec_on_args(
                    a,
                    "const to = window.__dexterNodes?.[a[0]]; \
                     if (!to) return {__dexter_err: 'stale node'}; \
                     const f = el.getBoundingClientRect(), t = to.getBoundingClientRect(); \
                     const [x1,y1,x2,y2] = [f.x+f.width/2, f.y+f.height/2, t.x+t.width/2, t.y+t.height/2]; \
                     const o = {bubbles:true, clientX:x1, clientY:y1, button:0}; \
                     el.dispatchEvent(new PointerEvent('pointerdown', o)); \
                     el.dispatchEvent(new MouseEvent('mousedown', o)); \
                     el.dispatchEvent(new DragEvent('dragstart', o)); \
                     const steps = 6; \
                     for(let i=1;i<=steps;i++){ \
                       const x = x1+(x2-x1)*i/steps, y = y1+(y2-y1)*i/steps; \
                       to.dispatchEvent(new PointerEvent('pointermove',{bubbles:true,clientX:x,clientY:y})); \
                       to.dispatchEvent(new DragEvent('dragover',{bubbles:true,clientX:x,clientY:y})); \
                     } \
                     const d = {bubbles:true, clientX:x2, clientY:y2, button:0}; \
                     to.dispatchEvent(new DragEvent('drop', d)); \
                     to.dispatchEvent(new PointerEvent('pointerup', d)); \
                     to.dispatchEvent(new MouseEvent('mouseup', d)); \
                     el.dispatchEvent(new DragEvent('dragend', d)); \
                     'dragged'",
                    // a[0] = the drop target's node index; `duration_ms`
                    // paces real-pointer drags — DOM dispatch is instant.
                    vec![json!(b.0)],
                )?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("dragged element {a} onto {b}")),
                )
                .with_element(Some(a)))
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
                let (id, _) = self.resolve(target)?;
                self.exec_on(id, "el.focus(); 'focused'")?;
                Ok(
                    ActionResult::success(Mechanism::Dom, Some(format!("focused element {id}")))
                        .with_element(Some(id)),
                )
            }
            Action::SetValue { target, value } => {
                let (id, _) = self.resolve(target)?;
                self.exec_on_args(
                    id,
                    "el.focus(); el.value = a[0]; \
                     el.dispatchEvent(new Event('input',{bubbles:true})); \
                     el.dispatchEvent(new Event('change',{bubbles:true})); 'set'",
                    vec![json!(value)],
                )?;
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("set value on element {id}")),
                )
                .with_element(Some(id)))
            }
            Action::TypeText { text, target } => {
                let t = target.clone().unwrap_or(Target::Focused);
                let (id, _) = self.resolve(&t)?;
                self.exec_on_args(
                    id,
                    "el.focus(); el.value = (el.value||'') + a[0]; \
                     el.dispatchEvent(new Event('input',{bubbles:true})); 'typed'",
                    vec![json!(text)],
                )?;
                Ok(
                    ActionResult::success(Mechanism::Dom, Some(format!("typed into element {id}")))
                        .with_element(Some(id)),
                )
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
                    let (id, _) = self.resolve(t)?;
                    self.exec_on(id, "el.scrollIntoView({block:'center'}); 'scrolled'")?;
                    return Ok(ActionResult::success(
                        Mechanism::Dom,
                        Some(format!("scrolled element {id} into view")),
                    )
                    .with_element(Some(id)));
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
