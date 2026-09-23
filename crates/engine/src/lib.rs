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
    Action, ActionResult, AppSelector, Event, EventKind, ExpectedState, Observation,
    ObservationScope, Rect, Target, Verification, VerificationStatus,
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

/// The audit journal — bounded and shareable. MCP serves
/// `dexter_journal` off this handle so audit reads never contend with
/// the engine lock mid-task.
#[derive(Debug, Default)]
pub struct Journal {
    /// Events in emission order (oldest first).
    pub events: std::collections::VecDeque<Event>,
    /// Events dropped once the cap was hit — audits must know.
    pub dropped: u64,
}

/// Hard cap on retained events — long sessions stay light.
const JOURNAL_CAP: usize = 10_000;

impl Journal {
    fn push(&mut self, ev: Event) {
        if self.events.len() >= JOURNAL_CAP {
            self.events.pop_front();
            self.dropped += 1;
        }
        self.events.push_back(ev);
    }
}

/// The shared runtime. One driver, one policy, one approval store, one
/// journal — CLI, MCP and SDKs all go through this.
pub struct Engine<D: ComputerDriver> {
    driver: D,
    policy: Policy,
    approvals: ApprovalStore,
    journal: std::sync::Arc<std::sync::Mutex<Journal>>,
    /// Live append sink — every journaled event is flushed here
    /// immediately so a presence overlay (or any consumer) can tail the
    /// journal while the task is still running.
    journal_sink: Option<std::io::BufWriter<std::fs::File>>,
}

impl<D: ComputerDriver> Engine<D> {
    pub fn new(driver: D, policy: Policy, approval_ttl: Duration) -> Self {
        Self {
            driver,
            policy,
            approvals: ApprovalStore::new(approval_ttl),
            journal: Default::default(),
            journal_sink: None,
        }
    }

    /// Stream every event to `path` (truncated at open) as it happens —
    /// one JSON line each, flushed. The in-memory journal is unaffected.
    pub fn set_journal_sink(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        self.journal_sink = Some(std::io::BufWriter::new(std::fs::File::create(path)?));
        Ok(())
    }

    pub fn driver(&self) -> &D {
        &self.driver
    }

    /// Snapshot of the journal (cloned — the live store may keep
    /// appending from a running task).
    pub fn events(&self) -> Vec<Event> {
        self.journal
            .lock()
            .unwrap()
            .events
            .iter()
            .cloned()
            .collect()
    }

    /// Shared handle to the bounded journal — readers (e.g.
    /// `dexter_journal`) never contend with the engine lock.
    pub fn journal_handle(&self) -> std::sync::Arc<std::sync::Mutex<Journal>> {
        self.journal.clone()
    }

    /// Pre-grant an approval fingerprint for this session (e.g. a scenario's
    /// `grants` list). Grants remain single-use and time-boxed.
    pub fn grant_approval(&mut self, fingerprint: &str) {
        self.approvals.grant(fingerprint);
    }

    /// Consent to physical input for this engine — fills the policy's
    /// physical default only when the file left it absent. Pair with
    /// `RunConfig::allow_coordinates` (the driver-side second line).
    pub fn permit_physical(&mut self) {
        self.policy.permit_physical();
    }

    fn journal(&mut self, kind: EventKind, data: serde_json::Value) {
        let ev = Event::new(kind, data);
        if let Some(w) = &mut self.journal_sink {
            use std::io::Write;
            if serde_json::to_writer(&mut *w, &ev).is_ok() {
                let _ = w.write_all(b"\n");
                let _ = w.flush(); // live consumers read this immediately
            }
        }
        self.journal.lock().unwrap().push(ev);
    }

    /// Run one step: policy gate → bounded act/verify loop.
    pub fn run_step(&mut self, step: &Step, cfg: &RunConfig) -> StepStatus {
        self.run_step_inner(step, cfg, None)
    }

    /// `obs` is the live observation when one exists (the `run_task` loop);
    /// it lets the journal carry the target's on-screen bounds so an
    /// overlay can render presence without touching the machine.
    fn run_step_inner(
        &mut self,
        step: &Step,
        cfg: &RunConfig,
        obs: Option<&Observation>,
    ) -> StepStatus {
        let app = step.app.clone().or_else(|| cfg.app.clone());
        let ctx = ActionContext {
            app: app.clone(),
            target_hint: None,
        };
        let fp = dexter_policy::fingerprint(&step.action, &ctx);
        let intrusiveness = step.action.intrusiveness();
        let bounds = target_bounds(&step.action, obs);
        self.journal(
            EventKind::ActionProposed,
            serde_json::json!({
                "action": &step.action,
                "app": &app,
                "fingerprint": &fp,
                "intrusiveness": intrusiveness,
                "target_bounds": bounds,
            }),
        );

        match self.policy.evaluate(&step.action, &ctx) {
            PolicyDecision::Allow => {}
            PolicyDecision::Deny { reason } => {
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({
                        "decision": "deny",
                        "reason": &reason,
                        "intrusiveness": intrusiveness,
                    }),
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
                    serde_json::json!({
                        "decision": "approved",
                        "fingerprint": &fp,
                        "intrusiveness": intrusiveness,
                    }),
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
                    reason: format!("{:?}: {}", result.status, result.detail.unwrap_or_default()),
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

    /// Closed-loop task runner: observe → done? → candidates → decide →
    /// act → repeat, bounded by `max_steps`. `done_when` is checked
    /// against every fresh observation — reaching VERIFIED completes the
    /// task regardless of what any engine believed.
    pub fn run_task(
        &mut self,
        goal: &str,
        generator: &dyn dexter_decision::CandidateGenerator,
        decider: &dyn dexter_decision::DecisionEngine,
        cfg: &TaskConfig,
    ) -> TaskOutcome {
        use dexter_decision::{Decision, DecisionContext, GenHistory, Route};
        let started = Instant::now();
        let mut last_error: Option<String> = None;
        let mut hist = GenHistory::default();
        let scope = ObservationScope {
            app: cfg.run.app.clone(),
            max_elements: cfg.run.observe_max_elements,
            ..Default::default()
        };

        for step in 1..=cfg.max_steps {
            let obs = match self.driver.observe(&scope) {
                Ok(o) => o,
                Err(e) => {
                    self.journal(
                        EventKind::TaskFailed,
                        serde_json::json!({"error": e.to_string(), "step": step}),
                    );
                    return TaskOutcome::Failed {
                        reason: format!("observe: {e}"),
                    };
                }
            };
            self.journal(
                EventKind::ObservationCreated,
                serde_json::json!({"observation": obs.id.0, "elements": obs.elements.len(), "step": step}),
            );

            // Done? Structural check — never the engine's word.
            let check = dexter_verify::verify(&obs, &cfg.done_when);
            if check.status == VerificationStatus::Verified {
                self.journal(
                    EventKind::TaskCompleted,
                    serde_json::json!({"steps": step - 1, "elapsed_ms": started.elapsed().as_millis() as u64}),
                );
                return TaskOutcome::Completed { steps: step - 1 };
            }

            hist.last_error = last_error.clone();
            let candidates = generator.generate(&obs, goal, &hist);
            let ctx = DecisionContext {
                goal: goal.to_string(),
                // Engines get a context-window-safe digest; the full one
                // stays on the observation for the audit trail.
                state_digest: dexter_world_model::digest_budget(&obs, STATE_BUDGET),
                candidates,
                last_error: last_error.clone(),
                step,
            };
            // The full decision context is journaled — this is what makes
            // a trace replayable offline (eval harness re-feeds it to any
            // DecisionEngine without touching the machine).
            self.journal(
                EventKind::CandidatesGenerated,
                serde_json::json!({
                    "count": ctx.candidates.len(),
                    "step": step,
                    "context": &ctx,
                }),
            );
            let decision = match decider.decide(&ctx) {
                Ok(d) => d,
                Err(e) => {
                    self.journal(
                        EventKind::TaskFailed,
                        serde_json::json!({"error": e.to_string(), "step": step}),
                    );
                    return TaskOutcome::Failed {
                        reason: e.to_string(),
                    };
                }
            };
            self.journal(
                EventKind::DecisionMade,
                serde_json::json!({
                    "engine": decider.name(),
                    "decision": &decision,
                    "step": step,
                }),
            );

            match decision {
                Decision::Act {
                    action, rationale, ..
                } => {
                    hist.attempts.push(action.clone());
                    let status = self.run_step_inner(
                        &Step {
                            note: Some(rationale),
                            action,
                            expect: None, // progress is judged by done_when
                            max_attempts: Some(1),
                            app: cfg.run.app.clone(),
                        },
                        &cfg.run,
                        Some(&obs),
                    );
                    match status {
                        StepStatus::Done { .. } => last_error = None,
                        other => {
                            last_error = Some(format!("{other:?}"));
                        }
                    }
                }
                Decision::Route { route, rationale } => match route {
                    Route::Wait { millis } => {
                        std::thread::sleep(Duration::from_millis(millis.min(10_000)))
                    }
                    Route::Retry | Route::Reobserve => {}
                    Route::Abstain => {
                        self.journal(
                            EventKind::TaskFailed,
                            serde_json::json!({"outcome": "abstain", "reason": &rationale, "step": step}),
                        );
                        return TaskOutcome::Abstained { reason: rationale };
                    }
                    Route::EscalateLlm | Route::EscalateHuman => {
                        self.journal(
                            EventKind::TaskFailed,
                            serde_json::json!({"outcome": "escalated", "reason": &rationale, "step": step}),
                        );
                        return TaskOutcome::Escalated {
                            route,
                            reason: rationale,
                        };
                    }
                },
            }
            hist.prev = Some(obs);
        }

        self.journal(
            EventKind::TaskFailed,
            serde_json::json!({"outcome": "max_steps", "max_steps": cfg.max_steps}),
        );
        TaskOutcome::MaxSteps
    }
}

/// Char budget for the digest handed to decision engines — sized so
/// Laya-class encoders (8192 tokens) never overflow on big AX trees.
const STATE_BUDGET: usize = 14_000;

/// The on-screen rect an action will land on — the presence contract an
/// overlay renders from. Resolved against the live observation for
/// element targets; a `Point` is its own (1×1) rect; `Navigate` has none.
fn target_bounds(action: &Action, obs: Option<&Observation>) -> Option<Rect> {
    let target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. } => Some(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        _ => None,
    }?;
    match target {
        Target::Point { x, y } => Some(Rect {
            x: *x,
            y: *y,
            w: 1.0,
            h: 1.0,
        }),
        Target::Window { window_id } => obs?
            .windows
            .iter()
            .find(|w| w.id == *window_id)
            .map(|w| w.bounds),
        _ => obs
            .and_then(|o| dexter_world_model::resolve_element(o, target).ok())
            .and_then(|e| e.bounds),
    }
}

/// How a `run_task` call ended.
#[derive(Debug)]
pub enum TaskOutcome {
    /// `done_when` verified after this many steps.
    Completed { steps: u32 },
    /// The decision engine declined to act.
    Abstained { reason: String },
    /// The decision engine escalated (LLM or human).
    Escalated {
        route: dexter_decision::Route,
        reason: String,
    },
    /// Runtime/driver/decision failure.
    Failed { reason: String },
    /// Step bound reached without `done_when` verifying.
    MaxSteps,
}

/// `run_task` parameters.
pub struct TaskConfig {
    pub run: RunConfig,
    /// Hard bound on decide/act iterations.
    pub max_steps: u32,
    /// Structural goal check — verified against every observation.
    pub done_when: ExpectedState,
}
