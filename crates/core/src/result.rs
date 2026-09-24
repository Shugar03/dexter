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

    /// Bind the element the act resolved to — the journal carries the
    /// id so an audit trail knows *which* node the input landed on.
    pub fn with_element(mut self, element: Option<crate::ElementId>) -> Self {
        self.element = element;
        self
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

/// Why a verdict came back `Uncertain` — the machine-readable half of
/// the tri-state contract, pruned to the three sources of uncertainty
/// this verifier can actually produce (cua-driver's seven, minus the
/// ones our predicates can't hit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownReason {
    /// The element tree was partial (`elements_truncated`/`ax_limited`)
    /// — an absent-looking result can't be trusted.
    TreePartial,
    /// `FocusedElement` checked but no element claims focus.
    NoFocusedElement,
    /// No window exposes a title to check against.
    NoWindowTitle,
    /// The check needs a value a sensitive field never exposes — the
    /// redaction is deliberate, so the verdict can't be definite.
    RedactedValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verification {
    pub status: VerificationStatus,
    /// Per-expectation detail lines for auditing.
    pub checks: Vec<String>,
    /// Why the verdict is `Uncertain`, when it is. `None` on any
    /// definite verdict — an uncertain verdict without a reason is a
    /// bug, not a mystery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<UnknownReason>,
}

impl Verification {
    pub fn verified() -> Self {
        Self {
            status: VerificationStatus::Verified,
            checks: vec![],
            unknown_reason: None,
        }
    }

    pub fn failed(checks: Vec<String>) -> Self {
        Self {
            status: VerificationStatus::Failed,
            checks,
            unknown_reason: None,
        }
    }

    /// An `Uncertain` verdict *requires* its reason — uncertain without
    /// a `why` is a bug, so the constructor takes it rather than letting
    /// a `None` slip through.
    pub fn uncertain(reason: UnknownReason, checks: Vec<String>) -> Self {
        Self {
            status: VerificationStatus::Uncertain,
            checks,
            unknown_reason: Some(reason),
        }
    }
}
