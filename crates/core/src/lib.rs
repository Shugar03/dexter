//! Dexter core types.
//!
//! The LLM decides *what* to achieve; Dexter decides *how* to interact with
//! the computer. These types are the lingua franca between drivers, the world
//! model, the verifier, the policy engine and the decision engines.

mod action;
mod element;
mod error;
mod event;
mod execution;
mod observation;
mod result;
mod target;

pub use action::{Action, Intrusiveness, KeyChord, MouseButton, ScrollDelta, WindowOperation};
pub use element::{is_sensitive_role, Element, ElementId, ElementSource, Rect};
pub use error::DexterError;
pub use event::{Event, EventKind};
pub use execution::{ExecutionPlan, ExecutionRoute, Sensitivity, TargetDescriptor};
pub use observation::{AppSelector, Observation, ObservationId, ObservationScope, Window};
pub use result::{
    classify_effect, ActionResult, ActionStatus, Effect, Escalation, EscalationReason,
    EscalationTarget, Mechanism, UnknownReason, Verification, VerificationStatus,
};
pub use target::{ExpectedState, SemanticTarget, Target, ValuePredicate};

/// A point in global screen coordinates (pixels, top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}
