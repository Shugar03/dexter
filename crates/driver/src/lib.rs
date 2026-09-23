//! The `ComputerDriver` seam: how Dexter observes and acts on a machine.
//!
//! A driver never simulates success. If it cannot know an action happened,
//! it reports that honestly via [`dexter_core::ActionStatus`].

use dexter_core::{Observation, ObservationScope, Window};

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

/// The physical interface to a computer. Synchronous: platform APIs are
/// blocking; async wrappers belong at the daemon boundary, not here.
pub trait ComputerDriver: Send + Sync {
    fn capabilities(&self) -> DriverCapabilities;

    /// Windows currently known to the window server.
    fn windows(&self) -> Result<Vec<Window>, DriverError>;

    /// One normalized snapshot of the world within `scope`.
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError>;
}
