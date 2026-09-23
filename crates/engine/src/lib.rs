//! `dexter-engine` — the runtime loop every interface shares:
//!
//! ```text
//! policy.evaluate ─┬─ Deny            → StepStatus::Denied
//!                  ├─ RequireApproval → grant? → else NeedsApproval
//!                  └─ Allow           → driver.act
//!                                       ├─ no expect → Done
//!                                       └─ expect    → re-observe → verify
//!                                                        ├─ VERIFIED → Done
//!                                                        └─ FAILED/UNCERTAIN → bounded retry
//! ```
//!
//! `UNCERTAIN` is never success. `FOREGROUND_REQUIRED` / `UNSUPPORTED` /
//! `PERMISSION_DENIED` are not retried — they are verdicts, not transient
//! failures. Every step writes structured events into the journal.

use dexter_core::{
    Action, ActionResult, AppSelector, Event, EventKind, ExpectedState, ObservationScope,
    Verification, VerificationStatus,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_policy::{ActionContext, ApprovalStore, Policy, PolicyDecision};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// One step of a scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    /// Optional human note for the journal/report.
    #[serde(default)]
    pub note: Option<String>,
    pub action: Action,
    /// What must hold after the action for the step to be done.
    #[serde(default)]
    pub expect: Option<ExpectedState>,
    /// Retry bound for this step (overrides engine default).
    #[serde(default)]
    pub max_attempts: Option<u32>,
    /// Optional per-step app scope override.
    #[serde(default)]
    pub app: Option<AppSelector>,
}

/// How a step ended.
#[derive(Debug)]
pub enum StepStatus {
    /// Action executed (and expectation verified, when present).
    Done {
        result: ActionResult,
        verification: Option<Verification>,
        attempts: u32,
    },
    /// Policy denied the action outright.
    Denied { reason: String },
    /// Policy requires approval and no live grant covers this fingerprint.
    NeedsApproval { fingerprint: String, reason: String },
    /// Acted but never reached a verified state within the attempt bound.
    Failed { reason: String, attempts: u32 },
    /// Driver could not produce a verdict at all.
    Errored { error: DriverError },
}

impl StepStatus {
    pub fn done(&self) -> bool {
        matches!(self, Self::Done { .. })
    }
}

/// Result of running a whole scenario.
#[derive(Debug)]
pub struct ScenarioReport {
    /// One entry per executed step, in order. Stops at the first step that
    /// is not `Done`.
    pub steps: Vec<(usize, StepStatus)>,
}

impl ScenarioReport {
    pub fn ok(&self) -> bool {
        self.steps.iter().all(|(_, s)| s.done())
    }
}

/// Bounds and context for one run.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// Default app scope for steps without their own.
    pub app: Option<AppSelector>,
    /// Default per-step attempt bound (action + verify cycles).
    pub max_attempts: u32,
    /// Settle time before re-observing for verification.
    pub verify_delay: Duration,
    /// Permit coordinate mechanisms to reach the driver.
    pub allow_coordinates: bool,
    /// Treat `RequireApproval` as granted for this run — the human approved
    /// the whole batch up front (e.g. `dexter run --approve-all`). Every
    /// grant is still journaled.
    pub approve_all: bool,
    /// Element cap for verification re-observations.
    pub observe_max_elements: usize,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            app: None,
            max_attempts: 3,
            verify_delay: Duration::from_millis(250),
            allow_coordinates: false,
            approve_all: false,
            observe_max_elements: 4_000,
        }
    }
}

/// The shared runtime. One driver, one policy, one approval store, one
/// journal — CLI, MCP and SDKs all go through this.
pub struct Engine<D: ComputerDriver> {
    driver: D,
    policy: Policy,
    approvals: ApprovalStore,
    events: Vec<Event>,
}

impl<D: ComputerDriver> Engine<D> {
    pub fn new(driver: D, policy: Policy, approval_ttl: Duration) -> Self {
        Self {
            driver,
            policy,
            approvals: ApprovalStore::new(approval_ttl),
            events: Vec::new(),
        }
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// Pre-grant an approval fingerprint for this session (e.g. a scenario's
    /// `grants` list). Grants remain single-use and time-boxed.
    pub fn grant_approval(&mut self, fingerprint: &str) {
        self.approvals.grant(fingerprint);
    }

    fn journal(&mut self, kind: EventKind, data: serde_json::Value) {
        self.events.push(Event::new(kind, data));
    }

    /// Run one step: policy gate → bounded act/verify loop.
    pub fn run_step(&mut self, step: &Step, cfg: &RunConfig) -> StepStatus {
        let app = step.app.clone().or_else(|| cfg.app.clone());
        let ctx = ActionContext {
            app: app.clone(),
            target_hint: None,
        };
        let fp = dexter_policy::fingerprint(&step.action, &ctx);
        self.journal(
            EventKind::ActionProposed,
            serde_json::json!({"action": &step.action, "app": &app, "fingerprint": &fp}),
        );

        match self.policy.evaluate(&step.action, &ctx) {
            PolicyDecision::Allow => {}
            PolicyDecision::Deny { reason } => {
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({"decision": "deny", "reason": &reason}),
                );
                return StepStatus::Denied { reason };
            }
            PolicyDecision::RequireApproval { reason } => {
                if cfg.approve_all {
                    self.journal(
                        EventKind::HumanApprovalRequired,
                        serde_json::json!({
                            "fingerprint": &fp,
                            "reason": &reason,
                            "granted": "approve_all",
                        }),
                    );
                } else if !self.approvals.check_and_consume(&fp) {
                    self.journal(
                        EventKind::HumanApprovalRequired,
                        serde_json::json!({"fingerprint": &fp, "reason": &reason}),
                    );
                    return StepStatus::NeedsApproval {
                        fingerprint: fp,
                        reason,
                    };
                }
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({"decision": "approved", "fingerprint": &fp}),
                );
            }
        }

        let act_ctx = ActContext {
            app: app.clone(),
            allow_coordinates: cfg.allow_coordinates,
        };
        let max_attempts = step.max_attempts.unwrap_or(cfg.max_attempts).max(1);
        let mut last_detail = String::new();
        let mut last_verification: Option<Verification> = None;

        for attempt in 1..=max_attempts {
            if attempt > 1 {
                self.journal(
                    EventKind::RecoveryStarted,
                    serde_json::json!({"attempt": attempt}),
                );
            }
            let result = match self.driver.act(&step.action, &act_ctx) {
                Ok(r) => r,
                Err(e) => {
                    self.journal(
                        EventKind::ActionFailed,
                        serde_json::json!({"error": e.to_string(), "attempt": attempt}),
                    );
                    return StepStatus::Errored { error: e };
                }
            };
            self.journal(
                EventKind::ActionExecuted,
                serde_json::json!({
                    "status": format!("{:?}", result.status),
                    "mechanism": format!("{:?}", result.mechanism),
                    "detail": &result.detail,
                    "attempt": attempt,
                }),
            );

            if !result.status.ok() {
                // Non-success statuses are verdicts, not transient errors —
                // retrying would repeat the same refusal.
                return StepStatus::Failed {
                    reason: format!(
                        "{:?}: {}",
                        result.status,
                        result.detail.unwrap_or_default()
                    ),
                    attempts: attempt,
                };
            }

            let Some(expected) = &step.expect else {
                return StepStatus::Done {
                    result,
                    verification: None,
                    attempts: attempt,
                };
            };

            // Settle, re-observe, verify.
            std::thread::sleep(cfg.verify_delay);
            let scope = ObservationScope {
                app: app.clone(),
                max_elements: cfg.observe_max_elements,
                ..Default::default()
            };
            let obs = match self.driver.observe(&scope) {
                Ok(o) => o,
                Err(e) => {
                    self.journal(
                        EventKind::ActionFailed,
                        serde_json::json!({"error": format!("verify observe: {e}"), "attempt": attempt}),
                    );
                    return StepStatus::Errored { error: e };
                }
            };
            self.journal(
                EventKind::ObservationCreated,
                serde_json::json!({"observation": obs.id.0, "elements": obs.elements.len(), "attempt": attempt}),
            );
            let verification = dexter_verify::verify(&obs, expected);
            self.journal(
                if verification.status == VerificationStatus::Verified {
                    EventKind::VerificationPassed
                } else {
                    EventKind::VerificationFailed
                },
                serde_json::json!({
                    "status": format!("{:?}", verification.status),
                    "checks": &verification.checks,
                    "attempt": attempt,
                }),
            );
            match verification.status {
                VerificationStatus::Verified => {
                    return StepStatus::Done {
                        result,
                        verification: Some(verification),
                        attempts: attempt,
                    };
                }
                other => {
                    last_detail = format!("{:?}: {}", other, verification.checks.join(" | "));
                    last_verification = Some(verification);
                }
            }
        }

        let _ = last_verification;
        StepStatus::Failed {
            reason: format!("verification never reached VERIFIED — last: {last_detail}"),
            attempts: max_attempts,
        }
    }

    /// Run steps in order; stop at the first non-Done outcome.
    pub fn run_scenario(&mut self, steps: &[Step], cfg: &RunConfig) -> ScenarioReport {
        let mut report = ScenarioReport { steps: Vec::new() };
        let started = Instant::now();
        for (i, step) in steps.iter().enumerate() {
            let status = self.run_step(step, cfg);
            let done = status.done();
            report.steps.push((i, status));
            if !done {
                self.journal(
                    EventKind::TaskFailed,
                    serde_json::json!({"step": i, "elapsed_ms": started.elapsed().as_millis() as u64}),
                );
                return report;
            }
        }
        self.journal(
            EventKind::TaskCompleted,
            serde_json::json!({"steps": steps.len(), "elapsed_ms": started.elapsed().as_millis() as u64}),
        );
        report
    }
}
