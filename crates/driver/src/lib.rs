//! The `ComputerDriver` seam: how Dexter observes and acts on a machine.
//!
//! A driver never simulates success. If it cannot know an action happened,
//! it reports that honestly via [`dexter_core::ActionStatus`].

use dexter_core::{
    Action, ActionResult, AppSelector, ExecutionPlan, ExecutionRoute, Observation,
    ObservationScope, Window,
};

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
    ///
    /// v1 shim — drivers should implement [`ComputerDriver::plan`] and
    /// [`ComputerDriver::execute`]; the engine calls `act` only through
    /// the default `execute` for one migration release.
    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError>;

    /// Read-only planning: the ordered routes this driver could take for
    /// `action` — each with its real mechanism, intrusiveness tier,
    /// sensitivity and foreground requirement. Planning may observe the
    /// world but must never mutate it. An empty `routes` list is an
    /// honest "unsupported". Default: the single legacy route — policy
    /// still gates the declared tier before `execute` runs.
    fn plan(&self, action: &Action, _ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        Ok(ExecutionPlan::legacy(action))
    }

    /// Execute exactly one route the engine authorized. The driver must
    /// revalidate the route's target immediately before side effects —
    /// a stale or vanished element is an error, never a different
    /// target. Default: forward the route's concrete action to `act`.
    fn execute(
        &self,
        route: &ExecutionRoute,
        ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        self.act(&route.action, ctx)
    }

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
    fn plan(&self, action: &Action, ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        (**self).plan(action, ctx)
    }
    fn execute(
        &self,
        route: &ExecutionRoute,
        ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        (**self).execute(route, ctx)
    }
    fn wake(&self, app: &AppSelector) -> Result<WakeHandle, DriverError> {
        (**self).wake(app)
    }
    fn restore(&self, handle: &WakeHandle) {
        (**self).restore(handle)
    }
}
