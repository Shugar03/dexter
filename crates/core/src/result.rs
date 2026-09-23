use serde::{Deserialize, Serialize};

/// Which mechanism executed an action. Ordered by preference — the action
/// router always tries the highest-fidelity mechanism available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mechanism {
    Api,
    Dom,
    Accessibility,
    NativeAutomation,
    Vision,
    Coordinates,
}

/// Explicit result status — never simulate success.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionStatus {
    Success,
    ForegroundRequired,
    Unsupported,
    PermissionDenied,
    Timeout,
    Failed,
}

impl ActionStatus {
    pub fn ok(self) -> bool {
        matches!(self, Self::Success)
    }
}

/// Outcome of executing an [`crate::Action`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionResult {
    pub status: ActionStatus,
    pub mechanism: Mechanism,
    /// Human/machine readable detail (e.g. which element was pressed).
    pub detail: Option<String>,
    /// Element acted upon, when applicable.
    pub element: Option<crate::ElementId>,
}

impl ActionResult {
    pub fn success(mechanism: Mechanism, detail: impl Into<Option<String>>) -> Self {
        Self {
            status: ActionStatus::Success,
            mechanism,
            detail: detail.into(),
            element: None,
        }
    }

    pub fn failure(status: ActionStatus, mechanism: Mechanism, detail: impl Into<String>) -> Self {
        Self {
            status,
            mechanism,
            detail: Some(detail.into()),
            element: None,
        }
    }
}

/// Verification verdict. `Uncertain` must never be treated as `Verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum VerificationStatus {
    Verified,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub status: VerificationStatus,
    /// Per-expectation detail lines for auditing.
    pub checks: Vec<String>,
}

impl Verification {
    pub fn verified() -> Self {
        Self {
            status: VerificationStatus::Verified,
            checks: vec![],
        }
    }

    pub fn failed(checks: Vec<String>) -> Self {
        Self {
            status: VerificationStatus::Failed,
            checks,
        }
    }

    pub fn uncertain(checks: Vec<String>) -> Self {
        Self {
            status: VerificationStatus::Uncertain,
            checks,
        }
    }
}
