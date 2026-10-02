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
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;
use walker::WALKER_JS;
use webdriver::{WebDriverClient, ELEMENT_KEY};

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

    /// Attach to an endpoint and create a session on the persistent
    /// browser profile at `dir` (created if missing) — cookies and
    /// logins survive across runs. chromedriver/geckodriver only:
    /// safaridriver has no profile capability and refuses the session.
    /// Never adopts a live session (it would ignore the profile).
    pub fn connect_with_profile(
        base_url: &str,
        label: &str,
        dir: &std::path::Path,
    ) -> Result<Self, DriverError> {
        Self::from_client(WebDriverClient::connect_with_profile(base_url, dir)?, label)
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

    /// Fetch the WebDriver element reference for `id` — the node from
    /// the live walk, through the same stale/disabled guard as
    /// `exec_on`. `Ok(None)` = disabled: the caller must report a
    /// `Failed` act instead of letting a pointer click land on a
    /// control that can't take it. The returned JSON object is itself
    /// a pointer `origin` (`{element-6066-…: ref}` or legacy `ELEMENT`).
    fn element_origin(&self, id: ElementId) -> Result<Option<serde_json::Value>, DriverError> {
        let resp = self.client.lock().unwrap().execute(
            &format!(
                "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                 if (!el) return {{__dexter_err: 'stale node'}}; \
                 if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') \
                     return {{__dexter_err: 'disabled'}}; \
                 return el; }})()",
                id.0
            ),
            vec![],
        )?;
        match resp["__dexter_err"].as_str() {
            Some("stale node") => Err(DriverError::StaleReference("stale node".into())),
            Some("disabled") => Ok(None),
            Some(other) => Err(DriverError::Platform(format!("element error: {other}"))),
            None => {
                if resp.get(ELEMENT_KEY).is_some() || resp.get("ELEMENT").is_some() {
                    Ok(Some(resp))
                } else {
                    Err(DriverError::Platform(
                        "execute returned no element reference".into(),
                    ))
                }
            }
        }
    }

    /// Post an input-source sequence to `POST /actions` and report the
    /// real-input mechanism. On error, release any held input state
    /// (keys/buttons persist across calls per spec) before surfacing.
    fn perform(
        &self,
        sources: Vec<serde_json::Value>,
        detail: String,
    ) -> Result<ActionResult, DriverError> {
        let mut c = self.client.lock().unwrap();
        match c.perform_actions(sources) {
            Ok(()) => Ok(ActionResult::success(Mechanism::Coordinates, Some(detail))),
            Err(e) => {
                let _ = c.release_actions();
                Err(e)
            }
        }
    }

    /// `KeyChord` → W3C key-source sequence: modifiers down in order,
    /// key down/up, modifiers up in reverse — same shape as a human
    /// chord. Unknown names are `Unsupported`, never guessed.
    fn key_chord(&self, chord: &dexter_core::KeyChord) -> Result<ActionResult, DriverError> {
        let mut mods = Vec::new();
        for m in &chord.modifiers {
            mods.push(
                webdriver_key(m)
                    .ok_or_else(|| DriverError::Unsupported(format!("unknown modifier '{m}'")))?,
            );
        }
        let key = webdriver_key(&chord.key)
            .ok_or_else(|| DriverError::Unsupported(format!("unknown key '{}'", chord.key)))?;
        let mut seq: Vec<serde_json::Value> = Vec::new();
        for v in &mods {
            seq.push(json!({"type":"keyDown","value":v}));
        }
        seq.push(json!({"type":"keyDown","value":key}));
        seq.push(json!({"type":"keyUp","value":key}));
        for v in mods.iter().rev() {
            seq.push(json!({"type":"keyUp","value":v}));
        }
        self.perform(
            vec![json!({"type":"key","id":"dexter-keys","actions":seq})],
            format!("posted chord {}", describe_chord(chord)),
        )
    }

    /// Run `script` with the live DOM node for `id` as `arguments[0]`.
    /// The node comes from `window.__dexterNodes`, which `observe()`
    /// repopulates on every walk — callers must have a current
    /// observation. Every element act runs inside this template, so
    /// the disabled guard lives here once: a disabled control takes a
    /// programmatic click/set silently (the DOM dispatch lands but does
    /// nothing a user could do) — the guard reports it instead of
    /// simulating success.
    fn exec_on(&self, id: ElementId, script: &str) -> Result<serde_json::Value, DriverError> {
        self.client.lock().unwrap().execute(
            &format!(
                "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                 if (!el) return {{__dexter_err: 'stale node'}}; \
                 if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') \
                     return {{__dexter_err: 'disabled'}}; \
                 return (function(el) {{ {} }})(el); }})()",
                id.0, script
            ),
            vec![],
        )
    }

    /// Map an in-page `__dexter_err` to the honest outcome: stale node
    /// → stale error (caller re-observes); disabled → `ActionResult`
    /// failure (the element resolved but can't take the act). `None`
    /// = the act ran.
    fn el_err(resp: &serde_json::Value) -> Result<Option<ActionResult>, DriverError> {
        match resp["__dexter_err"].as_str() {
            None => Ok(None),
            Some("stale node") => Err(DriverError::StaleReference("stale node".into())),
            Some("disabled") => Ok(Some(ActionResult::failure(
                ActionStatus::Failed,
                Mechanism::Dom,
                "element is disabled",
            ))),
            Some(other) => Err(DriverError::Platform(format!("element error: {other}"))),
        }
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
                            "observation {} not held — re-observe",
                            observation.0
                        ))
                    })?;
                    let stored = elements
                        .iter()
                        .find(|e| e.id == *element)
                        .cloned()
                        .ok_or_else(|| {
                            DriverError::StaleReference(format!(
                                "element {} not in observation {}",
                                element.0, observation.0
                            ))
                        })?;
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
                bundle_id: None,
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

    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
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
                if ctx.allow_coordinates {
                    // Real input tier: a pointer sequence anchored at the
                    // element's in-viewport center (element origin — no
                    // raw coordinates) for pages that require trusted
                    // events. The element-ref probe carries the
                    // stale/disabled guard before anything clicks.
                    match self.element_origin(id)? {
                        None => {
                            return Ok(ActionResult::failure(
                                ActionStatus::Failed,
                                Mechanism::Coordinates,
                                "element is disabled",
                            ));
                        }
                        Some(origin) => {
                            let btn = match button {
                                dexter_core::MouseButton::Middle => 1,
                                dexter_core::MouseButton::Right => 2,
                                _ => 0,
                            };
                            return self.perform(
                                vec![json!({
                                    "type":"pointer",
                                    "id":"dexter-pointer",
                                    "parameters":{"pointerType":"mouse"},
                                    "actions":[
                                        {"type":"pointerMove","duration":0,
                                         "origin":origin,"x":0,"y":0},
                                        {"type":"pointerDown","button":btn},
                                        {"type":"pointerUp","button":btn}
                                    ]
                                })],
                                format!("clicked element {id}"),
                            );
                        }
                    }
                }
                let js = match button {
                    dexter_core::MouseButton::Right => {
                        "el.dispatchEvent(new MouseEvent('contextmenu',{bubbles:true})); 'right-clicked'"
                    }
                    _ => "el.click(); 'clicked'",
                };
                let resp = self.exec_on(id, js)?;
                if let Some(res) = Self::el_err(&resp)? {
                    return Ok(res);
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
                if let Some(res) = Self::el_err(&resp)? {
                    return Ok(res);
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
                         if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') \
                             return {{__dexter_err: 'disabled'}}; \
                         el.focus(); el.value = arguments[0]; \
                         el.dispatchEvent(new Event('input',{{bubbles:true}})); \
                         el.dispatchEvent(new Event('change',{{bubbles:true}})); \
                         return 'set'; }})()",
                        id.0
                    ),
                    vec![json!(value)],
                )?;
                if let Some(res) = Self::el_err(&resp)? {
                    return Ok(res);
                }
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("set value on element {id}")),
                ))
            }
            Action::TypeText { text, target } => {
                if ctx.allow_coordinates {
                    // Real key input: covers what a `.value` assignment
                    // never inserts into — contenteditable, rich-text
                    // editors, isTrusted-gated fields. An explicit
                    // target is focused via DOM first; untargeted keys
                    // land on whatever the page has focused.
                    if let Some(t) = target {
                        if !matches!(t, Target::Focused) {
                            let id = self.resolve(t)?;
                            let resp = self.exec_on(id, "el.focus(); 'focused'")?;
                            if let Some(res) = Self::el_err(&resp)? {
                                return Ok(res);
                            }
                        }
                    }
                    let mut seq: Vec<serde_json::Value> = Vec::new();
                    for ch in text.chars() {
                        let v = ch.to_string();
                        seq.push(json!({"type":"keyDown","value":v}));
                        seq.push(json!({"type":"keyUp","value":v}));
                    }
                    return self.perform(
                        vec![json!({"type":"key","id":"dexter-keys","actions":seq})],
                        format!("typed {} chars via /actions", text.chars().count()),
                    );
                }
                let t = target.clone().unwrap_or(Target::Focused);
                let id = self.resolve(&t)?;
                let resp = self.client.lock().unwrap().execute(
                    &format!(
                        "return (() => {{ const el = window.__dexterNodes?.[{}]; \
                         if (!el) return {{__dexter_err:'stale node'}}; \
                         if (el.disabled === true || el.getAttribute('aria-disabled') === 'true') \
                             return {{__dexter_err: 'disabled'}}; \
                         el.focus(); el.value = (el.value||'') + arguments[0]; \
                         el.dispatchEvent(new Event('input',{{bubbles:true}})); \
                         return 'typed'; }})()",
                        id.0
                    ),
                    vec![json!(text)],
                )?;
                if let Some(res) = Self::el_err(&resp)? {
                    return Ok(res);
                }
                Ok(ActionResult::success(
                    Mechanism::Dom,
                    Some(format!("typed into element {id}")),
                ))
            }
            Action::Key { chord } => {
                if ctx.allow_coordinates {
                    // Real input tier — trusted key events to the
                    // focused element, like CGEvent on macOS.
                    return self.key_chord(chord);
                }
                // DOM key dispatch to the focused element — no physical
                // keyboard, no focus stealing.
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
                if ctx.allow_coordinates {
                    // Wheel source at the viewport center — real wheel
                    // events (wheel handlers, scroll-driven effects)
                    // that a silent scrollBy skips.
                    let dims = self
                        .client
                        .lock()
                        .unwrap()
                        .execute("return [window.innerWidth, window.innerHeight];", vec![])?;
                    let (w, h) = (
                        dims[0].as_f64().unwrap_or(0.0) / 2.0,
                        dims[1].as_f64().unwrap_or(0.0) / 2.0,
                    );
                    return self.perform(
                        vec![json!({
                            "type":"wheel",
                            "id":"dexter-wheel",
                            "actions":[{
                                "type":"scroll",
                                "x": w as i64, "y": h as i64,
                                "deltaX": delta.dx, "deltaY": delta.dy,
                                "origin":"viewport","duration":0
                            }]
                        })],
                        format!("scrolled wheel ({},{})", delta.dx, delta.dy),
                    );
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

/// Canonical key name → W3C Actions key value. Single characters pass
/// through verbatim; named keys use the spec's private-use codepoints.
/// Unknown names are an explicit error — never guessed.
fn webdriver_key(name: &str) -> Option<String> {
    let v = match name {
        "shift" => "\u{e008}",
        "ctrl" | "control" => "\u{e009}",
        "alt" | "option" | "opt" => "\u{e00a}",
        "cmd" | "command" | "meta" => "\u{e03d}",
        "return" | "enter" => "\u{e006}",
        "tab" => "\u{e004}",
        "escape" | "esc" => "\u{e00c}",
        "backspace" | "delete" => "\u{e003}",
        "forward_delete" | "del" => "\u{e017}",
        "insert" => "\u{e016}",
        "home" => "\u{e011}",
        "end" => "\u{e010}",
        "page_up" | "pageup" => "\u{e00e}",
        "page_down" | "pagedown" => "\u{e00f}",
        "left" => "\u{e012}",
        "up" => "\u{e013}",
        "right" => "\u{e014}",
        "down" => "\u{e015}",
        "space" => " ",
        "f1" => "\u{e031}",
        "f2" => "\u{e032}",
        "f3" => "\u{e033}",
        "f4" => "\u{e034}",
        "f5" => "\u{e035}",
        "f6" => "\u{e036}",
        "f7" => "\u{e037}",
        "f8" => "\u{e038}",
        "f9" => "\u{e039}",
        "f10" => "\u{e03a}",
        "f11" => "\u{e03b}",
        "f12" => "\u{e03c}",
        // Punctuation aliases — the Actions value is the literal char.
        "equal" => "=",
        "minus" => "-",
        "quote" => "'",
        "backslash" => "\\",
        "comma" => ",",
        "slash" => "/",
        "semicolon" => ";",
        "period" => ".",
        "backtick" => "`",
        _ if name.chars().count() == 1 => name,
        _ => return None,
    };
    Some(v.to_string())
}

fn describe_chord(chord: &dexter_core::KeyChord) -> String {
    let mut parts = chord.modifiers.clone();
    parts.push(chord.key.clone());
    parts.join("+")
}
