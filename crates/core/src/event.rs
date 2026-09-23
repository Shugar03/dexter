use serde::{Deserialize, Serialize};
use std::time::SystemTime;

/// Audit events emitted by the runtime (spec §20).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum EventKind {
    ObservationCreated,
    CandidatesGenerated,
    DecisionMade,
    ActionProposed,
    PolicyChecked,
    ActionExecuted,
    ActionFailed,
    StateChanged,
    VerificationPassed,
    VerificationFailed,
    RecoveryStarted,
    RecoveryCompleted,
    HumanApprovalRequired,
    TaskCompleted,
    TaskFailed,
}

/// One structured event in the execution log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: SystemTime,
    pub kind: EventKind,
    /// Free-form JSON payload (ids and statuses — never secrets).
    pub data: serde_json::Value,
}

impl Event {
    pub fn new(kind: EventKind, data: serde_json::Value) -> Self {
        Self {
            ts: SystemTime::now(),
            kind,
            data,
        }
    }
}
