//! The `ComputerDriver` seam: how Dexter observes and acts on a machine.
//!
//! A driver never simulates success. If it cannot know an action happened,
//! it reports that honestly via [`dexter_core::ActionStatus`].

use dexter_core::{Action, ActionResult, AppSelector, Observation, ObservationScope, Window};

/// Errors a driver can surface. Mapped to `ActionStatus` at the action layer.
#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("permission denied: {0}")]
    PermissionDenied(String),

    #[error("application not found: {0}")]
    AppNotFound(String),

    #[error("target not found: {0}")]
    NotFound(String),

    #[error("ambiguous target: {0}")]
    Ambiguous(String),

    #[error("unsupported on this platform/driver: {0}")]
    Unsupported(String),

    #[error("operation timed out: {0}")]
    Timeout(String),

    #[error("platform error: {0}")]
    Platform(String),

    #[error("stale reference: {0}")]
    StaleReference(String),
}

/// What a driver can actually do on this machine. Callers must check
/// capabilities instead of assuming — e.g. `background_input` is false on
/// drivers that would have to steal the user's cursor.
#[derive(Debug, Clone)]
pub struct DriverCapabilities {
    pub name: &'static str,
    /// Can produce a structured element tree (AX, DOM, UIA...).
    pub element_tree: bool,
    /// Can capture pixels.
    pub screenshots: bool,
    /// Can send input without moving the physical cursor / changing focus.
    pub background_input: bool,
}

/// Context for [`ComputerDriver::act`].
#[derive(Debug, Clone, Default)]
pub struct ActContext {
    /// The application the action is scoped to — required for semantic,
    /// element and focused targets; coordinate points may omit it.
    pub app: Option<AppSelector>,
    /// Permit coordinate-level mechanisms (CGEvent) that move the real
    /// cursor. Never granted implicitly.
    pub allow_coordinates: bool,
}

/// State captured before a [`ComputerDriver::wake`]. Pass it back to
/// [`ComputerDriver::restore`] to return focus to the user. Drivers
/// that never wake return `WakeHandle::default()` and restore is a
/// no-op.
#[derive(Debug, Clone, Default)]
pub struct WakeHandle {
    /// True when the driver actually requested activation — the caller
    /// should settle briefly before re-observing and must call
    /// `restore` when done with window content.
    pub activated: bool,
    /// Platform token consumed by `restore` (macOS: previous frontmost
    /// pid). Opaque to callers.
    token: Option<i64>,
}

impl WakeHandle {
    /// A wake that requested activation — `token` is the platform state
    /// `restore` needs to hand focus back.
    pub fn activated(token: Option<i64>) -> Self {
        Self {
            activated: true,
            token,
        }
    }

    /// The platform token captured at wake time (macOS: previous
    /// frontmost pid). Only meaningful to the driver that produced it.
    pub fn token(&self) -> Option<i64> {
        self.token
    }
}

/// The physical interface to a computer. Synchronous: platform APIs are
/// blocking; async wrappers belong at the daemon boundary, not here.
pub trait ComputerDriver: Send + Sync {
    fn capabilities(&self) -> DriverCapabilities;

    /// Windows currently known to the window server.
    fn windows(&self) -> Result<Vec<Window>, DriverError>;

    /// One normalized snapshot of the world within `scope`.
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError>;

    /// Execute one action. The driver reports the mechanism actually used
    /// and never claims success it can't substantiate.
    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError>;

    /// Best-effort request to expose `app`'s window content. Some
    /// platforms (macOS) only surface an app's AX window tree while it
    /// is frontmost — waking means one bounded activation, never a
    /// loop. The returned handle remembers who had focus so `restore`
    /// can hand it back. Default: no-op, `activated: false`.
    fn wake(&self, app: &AppSelector) -> Result<WakeHandle, DriverError> {
        let _ = app;
        Ok(WakeHandle::default())
    }

    /// Return focus captured by `wake`. No-op unless `handle.activated`.
    fn restore(&self, _handle: &WakeHandle) {}
}

impl ComputerDriver for Box<dyn ComputerDriver> {
    fn capabilities(&self) -> DriverCapabilities {
        (**self).capabilities()
    }
    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        (**self).windows()
    }
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        (**self).observe(scope)
    }
    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        (**self).act(action, ctx)
    }
    fn wake(&self, app: &AppSelector) -> Result<WakeHandle, DriverError> {
        (**self).wake(app)
    }
    fn restore(&self, handle: &WakeHandle) {
        (**self).restore(handle)
    }
}

/// Split `text` into UTF-16 chunks of at most `max_units` code units,
/// never separating a surrogate pair — a lone surrogate posted as its
/// own keyboard event types U+FFFD (or nothing) instead of the char.
/// A chunk always holds at least one whole char, so `max_units < 2`
/// still makes progress on astral chars.
pub fn utf16_chunks(text: &str, max_units: usize) -> Vec<Vec<u16>> {
    let mut chunks: Vec<Vec<u16>> = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    let mut buf = [0u16; 2];
    for ch in text.chars() {
        let units = ch.encode_utf16(&mut buf);
        if !cur.is_empty() && cur.len() + units.len() > max_units {
            chunks.push(std::mem::take(&mut cur));
        }
        cur.extend_from_slice(units);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// The one pid behind an app selector. `pids` are the running
/// processes the platform matched for `what` (e.g. "bundle id 'com.a'");
/// non-positive pids are not running. Several distinct instances fail
/// closed as [`DriverError::Ambiguous`] — acting on an arbitrary one
/// would target a window the caller never chose.
pub fn unique_app_pid(pids: &[i32], what: &str) -> Result<i32, DriverError> {
    let mut found: Option<i32> = None;
    for &pid in pids.iter().filter(|&&p| p > 0) {
        match found {
            Some(prev) if prev != pid => {
                return Err(DriverError::Ambiguous(format!(
                    "more than one running application with {what} — use --pid"
                )));
            }
            _ => found = Some(pid),
        }
    }
    found.ok_or_else(|| DriverError::AppNotFound(format!("no running application with {what}")))
}

#[cfg(test)]
mod tests {
    use super::{unique_app_pid, utf16_chunks, DriverError};

    fn roundtrip(chunks: &[Vec<u16>]) -> Vec<String> {
        chunks
            .iter()
            .map(|c| String::from_utf16(c).expect("chunk splits a surrogate pair"))
            .collect()
    }

    #[test]
    fn utf16_chunks_never_split_surrogate_pairs() {
        // 19 ASCII units + an emoji (2 units) straddles a 20-unit boundary.
        let text = format!("{}😀tail", "a".repeat(19));
        let chunks = utf16_chunks(&text, 20);
        assert!(chunks.iter().all(|c| c.len() <= 20));
        assert_eq!(roundtrip(&chunks).concat(), text);
    }

    #[test]
    fn utf16_chunks_bmp_text_fills_chunks() {
        let text = "ñ".repeat(45);
        let chunks = utf16_chunks(&text, 20);
        let lens: Vec<usize> = chunks.iter().map(Vec::len).collect();
        assert_eq!(lens, vec![20, 20, 5]);
    }

    #[test]
    fn utf16_chunks_tiny_budget_keeps_whole_chars() {
        let text = "😀😀a";
        let chunks = utf16_chunks(text, 1);
        assert_eq!(roundtrip(&chunks), vec!["😀", "😀", "a"]);
    }

    #[test]
    fn utf16_chunks_empty_text_is_empty() {
        assert!(utf16_chunks("", 20).is_empty());
    }

    #[test]
    fn unique_app_pid_single_instance_resolves() {
        assert_eq!(unique_app_pid(&[412], "bundle id 'com.a'").unwrap(), 412);
    }

    #[test]
    fn unique_app_pid_none_is_app_not_found() {
        let err = unique_app_pid(&[], "bundle id 'com.a'").unwrap_err();
        assert!(matches!(err, DriverError::AppNotFound(m) if m.contains("com.a")));
    }

    #[test]
    fn unique_app_pid_non_positive_pids_are_not_running() {
        let err = unique_app_pid(&[-1, 0], "bundle id 'com.a'").unwrap_err();
        assert!(matches!(err, DriverError::AppNotFound(_)));
    }

    #[test]
    fn unique_app_pid_multiple_instances_fail_closed() {
        let err = unique_app_pid(&[412, 913], "bundle id 'com.a'").unwrap_err();
        assert!(
            matches!(err, DriverError::Ambiguous(m) if m.contains("com.a") && m.contains("--pid"))
        );
    }

    #[test]
    fn unique_app_pid_duplicate_reports_of_one_pid_resolve() {
        assert_eq!(
            unique_app_pid(&[412, 412], "bundle id 'com.a'").unwrap(),
            412
        );
    }
}
