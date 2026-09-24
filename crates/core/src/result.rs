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

/// What the driver can attest about an act's effect — the common
/// vocabulary drivers report and the engine/journal consume. Distinct
/// from `status` (whether the delivery path ran) — this is whether the
/// world demonstrably moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Read-back evidence exists that the act landed.
    Confirmed,
    /// The act ran but the driver cannot prove or disprove an effect.
    Unverifiable,
    /// Delivery reported success but the world shows no change — the
    /// classic absorbed click.
    SuspectedNoop,
    /// The driver refused before delivering — policy, staleness,
    /// unsupported. No input was emitted.
    Refused,
}

/// Where an unconfirmed act should be tried next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationTarget {
    /// Coordinates over the same surface.
    Px,
    /// Foreground delivery (focus-raising input).
    Foreground,
    /// Page/DOM level re-dispatch.
    Page,
    /// A fresh session/observation — the reference itself is stale.
    Session,
}

/// Why the current route cannot confirm the act.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// This mechanism cannot reach the target at all.
    RouteUnavailable,
    /// Input was emitted but the surface dropped it.
    DeliveryFailed,
    /// The act ran; verification could not confirm an effect.
    EffectUnconfirmed,
    /// The world demonstrably did not change.
    SuspectedNoop,
    /// A grant or OS permission stands in the way.
    PermissionRequired,
}

/// A structured "try this next" — carried on results so the engine can
/// escalate without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    pub target: EscalationTarget,
    pub reason: EscalationReason,
}

/// The minimal effect classifier — borrowed from the cua-driver rule:
/// confirmed requires read-back; changed-or-unproven is unverifiable;
/// unchanged-with-readback is a suspected no-op. `Refused` never comes
/// from classification — it is attached to refusal verdicts directly.
pub fn classify_effect(changed: bool, readback_available: bool) -> Effect {
    match (readback_available, changed) {
        (true, true) => Effect::Confirmed,
        (true, false) => Effect::SuspectedNoop,
        (false, _) => Effect::Unverifiable,
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
    /// What the driver can attest about the act's effect, when it
    /// classified the outcome. `None` = not classified (legacy path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect: Option<Effect>,
    /// The read-back the `Confirmed` claim rests on (what changed, in
    /// driver terms). Required for `Confirmed`; forbidden on `Refused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<String>,
    /// Structured next step when this route could not confirm.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation: Option<Escalation>,
}

impl ActionResult {
    pub fn success(mechanism: Mechanism, detail: impl Into<Option<String>>) -> Self {
        Self {
            status: ActionStatus::Success,
            mechanism,
            detail: detail.into(),
            element: None,
            effect: None,
            evidence: None,
            escalation: None,
        }
    }

    pub fn failure(status: ActionStatus, mechanism: Mechanism, detail: impl Into<String>) -> Self {
        Self {
            status,
            mechanism,
            detail: Some(detail.into()),
            element: None,
            effect: None,
            evidence: None,
            escalation: None,
        }
    }

    /// A result whose effect the driver classified. Validated:
    /// `Confirmed` requires evidence (a claim needs a read-back);
    /// `Refused` admits neither evidence nor a successful status (no
    /// input was delivered); `Confirmed` never rides a failure status.
    pub fn classified(
        status: ActionStatus,
        mechanism: Mechanism,
        detail: impl Into<Option<String>>,
        effect: Effect,
        evidence: impl Into<Option<String>>,
        escalation: Option<Escalation>,
    ) -> Result<Self, String> {
        let evidence = evidence.into();
        match effect {
            Effect::Confirmed => {
                if evidence.is_none() {
                    return Err("Confirmed requires read-back evidence".into());
                }
                if !status.ok() {
                    return Err("Confirmed cannot ride a failure status".into());
                }
            }
            Effect::Refused => {
                if evidence.is_some() {
                    return Err("Refused admits no evidence — nothing ran".into());
                }
                if status.ok() {
                    return Err("Refused cannot claim Success — no delivery".into());
                }
            }
            Effect::Unverifiable | Effect::SuspectedNoop => {}
        }
        Ok(Self {
            status,
            mechanism,
            detail: detail.into(),
            element: None,
            effect: Some(effect),
            evidence,
            escalation,
        })
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

    pub fn uncertain(checks: Vec<String>) -> Self {
        Self {
            status: VerificationStatus::Uncertain,
            checks,
            unknown_reason: None,
        }
    }

    pub fn uncertain_because(reason: UnknownReason, checks: Vec<String>) -> Self {
        Self {
            status: VerificationStatus::Uncertain,
            checks,
            unknown_reason: Some(reason),
        }
    }
}
