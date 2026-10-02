//! Minimal W3C WebDriver client — session lifecycle + the endpoints
//! Dexter uses. Sync (ureq); the `ComputerDriver` trait is sync.

use dexter_driver::DriverError;
use serde_json::{json, Value};
use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// WebDriver's W3C element-reference key — the shape `execute/sync`
/// uses to return a node and `actions` uses as a pointer origin.
/// Pre-W3C drivers answer `ELEMENT` instead; callers accept both.
pub const ELEMENT_KEY: &str = "element-6066-11e4-a52e-4f735466cecf";

pub struct WebDriverClient {
    base: String,
    agent: ureq::Agent,
    session: Option<String>,
    /// We DELETE the session on Drop only if we created it — sessions
    /// adopted via `GET /sessions` (attach to the user's live page)
    /// belong to whoever opened them.
    owns_session: bool,
    /// Owned driver process (safaridriver spawned by us) — killed on drop.
    proc: Option<Child>,
    /// Persistent browser profile for sessions we create (absolute,
    /// canonical). `None` = the driver's throwaway default.
    profile: Option<PathBuf>,
}

/// New-session capabilities. Without a profile: `alwaysMatch: {}` —
/// the driver picks its own browser. With one: a `firstMatch` entry
/// per browser that takes a profile arg, so whichever driver answers
/// (chromedriver or geckodriver) matches its own entry and launches on
/// that profile; safaridriver matches none and refuses the session.
pub fn session_capabilities(profile: Option<&Path>) -> Value {
    match profile.and_then(Path::to_str) {
        None => json!({"capabilities": {"alwaysMatch": {}}}),
        Some(dir) => json!({"capabilities": {
            "alwaysMatch": {},
            "firstMatch": [
                {"browserName": "chrome",
                 "goog:chromeOptions": {"args": [format!("--user-data-dir={dir}")]}},
                {"browserName": "firefox",
                 "moz:firefoxOptions": {"args": ["-profile", dir]}}
            ]
        }}),
    }
}

/// Create `dir` if missing and resolve it absolute + canonical — the
/// browser resolves relative paths against the driver process's cwd,
/// and Firefox's `-profile` requires an existing directory.
fn profile_dir(dir: &Path) -> Result<PathBuf, DriverError> {
    std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::canonicalize(dir))
        .map_err(|e| DriverError::Platform(format!("browser profile {}: {e}", dir.display())))
        .and_then(|abs| {
            abs.to_str().map(|_| abs.clone()).ok_or_else(|| {
                DriverError::Platform(format!(
                    "browser profile {}: path is not valid UTF-8",
                    abs.display()
                ))
            })
        })
}

/// WebDriver reports errors as HTTP 4xx/5xx with a JSON body
/// (`{"value":{"error":...,"message":...}}`). `http_status_as_error(false)`
/// lets us read that body instead of losing the message.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr().map(|a| a.port()))
        .unwrap_or(4444)
}

impl WebDriverClient {
    /// Spawn `safaridriver` on a free port and connect.
    pub fn safari() -> Result<Self, DriverError> {
        let port = free_port();
        let proc = Command::new("safaridriver")
            .args(["-p", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| DriverError::Platform(format!("spawn safaridriver: {e}")))?;
        let client = Self {
            base: format!("http://localhost:{port}"),
            agent: agent(),
            session: None,
            owns_session: true,
            proc: Some(proc),
            profile: None,
        };
        client.wait_ready()?;
        Ok(client)
    }

    /// Attach to a driver already listening (chromedriver, remote grid).
    /// Always opens a fresh session.
    pub fn connect(base: &str) -> Result<Self, DriverError> {
        Self::connect_mode(base, false)
    }

    /// Attach and adopt the endpoint's existing session if any — the
    /// one-shot CLI can then see the user's actual page. The adopted
    /// session is never deleted on Drop.
    pub fn connect_attach(base: &str) -> Result<Self, DriverError> {
        Self::connect_mode(base, true)
    }

    /// Attach and always create our own session on the persistent
    /// profile at `dir` (created if missing). Never adopts a live
    /// session — an adopted session runs on whatever profile its
    /// creator chose, so the profile would be silently ignored.
    pub fn connect_with_profile(base: &str, dir: &Path) -> Result<Self, DriverError> {
        let profile = profile_dir(dir)?;
        let mut client = Self::connect_mode(base, false)?;
        client.profile = Some(profile);
        Ok(client)
    }

    fn connect_mode(base: &str, adopt: bool) -> Result<Self, DriverError> {
        let mut client = Self {
            base: base.trim_end_matches('/').to_string(),
            agent: agent(),
            session: None,
            owns_session: true,
            proc: None,
            profile: None,
        };
        client.wait_ready()?;
        // Adopt a live session if the endpoint has one — `GET /sessions`
        // is non-standard but implemented by chromedriver/geckodriver;
        // it lets a one-shot CLI observe the user's actual page instead
        // of a fresh about:blank. Endpoints without it just 404 and we
        // create our own session lazily.
        if adopt {
            if let Ok(resp) = client.get("/sessions") {
                if let Some(sid) = resp["value"]
                    .as_array()
                    .and_then(|a| a.first())
                    .and_then(|s| s["id"].as_str().or_else(|| s["sessionId"].as_str()))
                {
                    client.session = Some(sid.to_string());
                    client.owns_session = false;
                }
            }
        }
        Ok(client)
    }

    fn wait_ready(&self) -> Result<(), DriverError> {
        for _ in 0..40 {
            if self.get("/status").is_ok() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        Err(DriverError::Platform(format!(
            "webdriver at {} never became ready",
            self.base
        )))
    }

    fn get(&self, path: &str) -> Result<Value, DriverError> {
        self.agent
            .get(format!("{}{}", self.base, path))
            .call()
            .map_err(|e| DriverError::Platform(format!("webdriver GET {path}: {e}")))
            .and_then(|r| read_json(r, path))
    }

    fn post(&self, path: &str, body: Value) -> Result<Value, DriverError> {
        self.agent
            .post(format!("{}{}", self.base, path))
            .header("content-type", "application/json")
            .send(serde_json::to_vec(&body).unwrap_or_default())
            .map_err(|e| DriverError::Platform(format!("webdriver POST {path}: {e}")))
            .and_then(|r| read_json(r, path))
    }

    fn delete(&self, path: &str) -> Result<Value, DriverError> {
        self.agent
            .delete(format!("{}{}", self.base, path))
            .call()
            .map_err(|e| DriverError::Platform(format!("webdriver DELETE {path}: {e}")))
            .and_then(|r| read_json(r, path))
    }

    /// Lazily create the browser session (skipped when a session was
    /// adopted at connect) with `session_capabilities` — never a bare
    /// browser name, which would break cross-driver attach.
    fn ensure_session(&mut self) -> Result<&str, DriverError> {
        if self.session.is_none() {
            let caps = session_capabilities(self.profile.as_deref());
            let resp = self.post("/session", caps)?;
            let sid = resp["value"]["sessionId"]
                .as_str()
                .ok_or_else(|| {
                    let msg = resp["value"]["message"].as_str().unwrap_or("no sessionId");
                    DriverError::Platform(format!("webdriver session: {msg}"))
                })?
                .to_string();
            self.session = Some(sid);
            self.owns_session = true;
        }
        Ok(self.session.as_deref().unwrap())
    }
}

fn read_json(mut r: ureq::http::Response<ureq::Body>, path: &str) -> Result<Value, DriverError> {
    let status = r.status().as_u16();
    let mut s = String::new();
    r.body_mut()
        .as_reader()
        .read_to_string(&mut s)
        .map_err(|e| DriverError::Platform(format!("webdriver read: {e}")))?;
    let v: Value = serde_json::from_str(&s)
        .map_err(|e| DriverError::Platform(format!("webdriver {path}: invalid json: {e}")))?;
    if status >= 400 {
        // WebDriver error body: {"value":{"error":"...","message":"..."}}
        let msg = v["value"]["message"].as_str().unwrap_or(&s);
        return Err(DriverError::Platform(format!(
            "webdriver {path} (http {status}): {msg}"
        )));
    }
    Ok(v)
}

impl WebDriverClient {
    pub fn close_session(&mut self) {
        if !self.owns_session {
            self.session = None;
            return;
        }
        if let Some(sid) = self.session.take() {
            let _ = self.delete(&format!("/session/{sid}"));
        }
    }

    /// Navigate to a URL.
    pub fn navigate(&mut self, url: &str) -> Result<(), DriverError> {
        let sid = self.ensure_session()?.to_string();
        self.post(&format!("/session/{sid}/url"), json!({"url": url}))
            .map(|_| ())
    }

    /// Current page title.
    pub fn title(&mut self) -> Result<String, DriverError> {
        let sid = self.ensure_session()?.to_string();
        Ok(self.get(&format!("/session/{sid}/title"))?["value"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    /// Current URL.
    pub fn url(&mut self) -> Result<String, DriverError> {
        let sid = self.ensure_session()?.to_string();
        Ok(self.get(&format!("/session/{sid}/url"))?["value"]
            .as_str()
            .unwrap_or("")
            .to_string())
    }

    /// All window handles in the session (W3C `GET /window/handles`).
    pub fn window_handles(&mut self) -> Result<Vec<String>, DriverError> {
        let sid = self.ensure_session()?.to_string();
        let resp = self.get(&format!("/session/{sid}/window/handles"))?;
        Ok(resp["value"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// The currently focused handle (W3C `GET /window`).
    pub fn current_window_handle(&mut self) -> Result<String, DriverError> {
        let sid = self.ensure_session()?.to_string();
        Ok(self.get(&format!("/session/{sid}/window"))?["value"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Focus a different tab/window (W3C `POST /window`). This is an
    /// observable switch — the tab becomes active in the browser.
    pub fn switch_to_window(&mut self, handle: &str) -> Result<(), DriverError> {
        let sid = self.ensure_session()?.to_string();
        self.post(&format!("/session/{sid}/window"), json!({"handle": handle}))
            .map(|_| ())
    }

    /// Open a new tab and return its handle (W3C `POST /window/new`).
    /// The driver focuses it per spec.
    pub fn new_window(&mut self) -> Result<String, DriverError> {
        let sid = self.ensure_session()?.to_string();
        let resp = self.post(
            &format!("/session/{sid}/window/new"),
            json!({"type": "tab"}),
        )?;
        Ok(resp["value"]["handle"]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Close the current tab (W3C `DELETE /window`). Returns the
    /// remaining handles; the caller must switch to one — the session
    /// has no focused window until then.
    pub fn close_window(&mut self) -> Result<Vec<String>, DriverError> {
        let sid = self.ensure_session()?.to_string();
        let resp = self.delete(&format!("/session/{sid}/window"))?;
        Ok(resp["value"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| h.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Perform a W3C Actions sequence (`POST /session/:id/actions`) —
    /// the driver's real input pipeline: pointer/key/wheel sources
    /// produce trusted events, not DOM synthesis.
    pub fn perform_actions(&mut self, actions: Vec<Value>) -> Result<(), DriverError> {
        let sid = self.ensure_session()?.to_string();
        self.post(
            &format!("/session/{sid}/actions"),
            json!({"actions": actions}),
        )
        .map(|_| ())
    }

    /// Release all input-source state (`DELETE /session/:id/actions`).
    /// Held modifiers/buttons persist across calls, so a failed
    /// sequence releases before surfacing its error.
    pub fn release_actions(&mut self) -> Result<(), DriverError> {
        let sid = self.ensure_session()?.to_string();
        self.delete(&format!("/session/{sid}/actions")).map(|_| ())
    }

    /// Execute a synchronous script; returns the JSON-serialized result.
    /// `args` are passed to the script as `arguments`.
    pub fn execute(&mut self, script: &str, args: Vec<Value>) -> Result<Value, DriverError> {
        let sid = self.ensure_session()?.to_string();
        let resp = self.post(
            &format!("/session/{sid}/execute/sync"),
            json!({"script": script, "args": args}),
        )?;
        if let Some(err) = resp["value"]["error"].as_str() {
            let msg = resp["value"]["message"].as_str().unwrap_or("");
            return Err(DriverError::Platform(format!("script error {err}: {msg}")));
        }
        Ok(resp["value"].clone())
    }

    /// Page screenshot → writes PNG to `path`.
    pub fn screenshot(&mut self, path: &str) -> Result<(), DriverError> {
        let sid = self.ensure_session()?.to_string();
        let resp = self.get(&format!("/session/{sid}/screenshot"))?;
        let b64 = resp["value"]
            .as_str()
            .ok_or_else(|| DriverError::Platform("screenshot: no data".into()))?;
        use base64::Engine;
        let png = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| DriverError::Platform(format!("screenshot decode: {e}")))?;
        std::fs::write(path, png)
            .map_err(|e| DriverError::Platform(format!("screenshot write: {e}")))
    }
}

impl Drop for WebDriverClient {
    fn drop(&mut self) {
        self.close_session();
        if let Some(mut p) = self.proc.take() {
            let _ = p.kill();
            let _ = p.wait();
        }
    }
}
