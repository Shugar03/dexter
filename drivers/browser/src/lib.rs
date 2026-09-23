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
    Action, ActionResult, ActionStatus, AppSelector, Element, ElementId, Mechanism, Observation,
    ObservationId, ObservationScope, Rect, Target, Window,
};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
use serde_json::json;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;
use walker::WALKER_JS;
use webdriver::WebDriverClient;

pub struct BrowserDriver {
    client: Mutex<WebDriverClient>,
    /// Last N observations' element snapshots — element targets bind to
    /// the observation that produced them.
    obs_cache: Mutex<VecDeque<(ObservationId, Vec<Element>)>>,
    next_observation: AtomicU64,
    /// Driver label reported in observations ("safari", "chrome", ...).
    label: String,
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
        })
    }

    /// Navigate the current session to `url`.
    pub fn navigate(&self, url: &str) -> Result<(), DriverError> {
        self.client.lock().unwrap().navigate(url)
    }

    /// Run a script in the page (test/debug/CLI escape hatch).
    pub fn client_exec(
        &self,
        script: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, DriverError> {
        self.client.lock().unwrap().execute(script, args)
    }

    /// Fresh DOM walk → element list.
    fn walk(&self) -> Result<Vec<Element>, DriverError> {
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

    fn cache_observation(&self, obs: &Observation) {
        let mut cache = self.obs_cache.lock().unwrap();
        cache.retain(|(id, _)| *id != obs.id);
        cache.push_front((obs.id, obs.elements.clone()));
        cache.truncate(4);
    }

    /// Resolve a `Target` to the element id within the *current* walk.
    fn resolve(&self, target: &Target) -> Result<ElementId, DriverError> {
        match target {
            Target::Element {
                observation,
                element,
            } => {
                let stored = {
                    let cache = self.obs_cache.lock().unwrap();
                    let entry = cache.iter().find(|(id, _)| *id == *observation);
                    let (_, elements) = entry.ok_or_else(|| {
                        DriverError::StaleReference(format!(
                            "observation {} not held — re-observe",
                            observation.0
                        ))
                    })?;
                    elements
                        .iter()
                        .find(|e| e.id == *element)
                        .cloned()
                        .ok_or_else(|| {
                            DriverError::StaleReference(format!(
                                "element {} not in observation {}",
                                element.0, observation.0
                            ))
                        })?
                };
                // Fresh walk + identity check — DOMs mutate.
                let fresh_els = self.walk()?;
                let fresh = fresh_els.iter().find(|e| e.id == *element).ok_or_else(|| {
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
                // Fresh observation + world-model resolution (ambiguous /
                // not-found fail closed, same as macOS).
                let obs = self.observe(&ObservationScope::default())?;
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

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        let (title, url) = {
            let mut c = self.client.lock().unwrap();
            (c.title().unwrap_or_default(), c.url().unwrap_or_default())
        };
        Ok(vec![Window {
            id: 1,
            pid: 0,
            app: self.label.clone(),
            title: Some(format!("{title} — {url}")),
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            },
            on_screen: true,
            layer: 0,
        }])
    }

    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        let elements = self.walk()?;
        let windows = self.windows().unwrap_or_default();
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
            collection_errors: 0,
            ax_limited: false, // DOM has no degraded-grant mode
            screenshot,
            digest: String::new(),
        };
        obs.digest = dexter_world_model::digest(&obs, 250);
        self.cache_observation(&obs);
        Ok(obs)
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
}
