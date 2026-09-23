//! Minimal W3C WebDriver client — session lifecycle + the endpoints
//! Dexter uses. Sync (ureq); the `ComputerDriver` trait is sync.

use dexter_driver::DriverError;
use serde_json::{json, Value};
use std::io::Read;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub struct WebDriverClient {
    base: String,
    agent: ureq::Agent,
    session: Option<String>,
    /// Owned driver process (safaridriver spawned by us) — killed on drop.
    proc: Option<Child>,
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
            proc: Some(proc),
        };
        client.wait_ready()?;
        Ok(client)
    }

    /// Attach to a driver already listening (chromedriver, remote grid).
    pub fn connect(base: &str) -> Result<Self, DriverError> {
        let client = Self {
            base: base.trim_end_matches('/').to_string(),
            agent: agent(),
            session: None,
            proc: None,
        };
        client.wait_ready()?;
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

    /// Lazily create the browser session.
    fn ensure_session(&mut self) -> Result<&str, DriverError> {
        if self.session.is_none() {
            let resp = self.post(
                "/session",
                json!({"capabilities": {"alwaysMatch": {"browserName": "safari"}}}),
            )?;
            let sid = resp["value"]["sessionId"]
                .as_str()
                .ok_or_else(|| {
                    let msg = resp["value"]["message"].as_str().unwrap_or("no sessionId");
                    DriverError::Platform(format!("webdriver session: {msg}"))
                })?
                .to_string();
            self.session = Some(sid);
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
