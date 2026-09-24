//! `dexter-engine` — the runtime loop every interface shares:
//!
//! ```text
//! driver.plan ─► routes (concrete mechanism + tier + target)
//!       └─► per route: policy.evaluate_route ─┬─ Deny            → Denied
//!                                             ├─ RequireApproval → grant? → else NeedsApproval
//!                                             └─ Allow           → driver.execute (at most once)
//!                                                                    ├─ no expect → Done
//!                                                                    └─ expect    → verify-poll
//!                                                                          ├─ VERIFIED → Done
//!                                                                          └─ else → bounded polls, never re-execute
//! ```
//!
//! `UNCERTAIN` is never success. A mutating action executes **once** per
//! step: a delayed verification polls the world, it never repeats the
//! mutation. `FOREGROUND_REQUIRED` / `UNSUPPORTED` / `PERMISSION_DENIED`
//! are verdicts, not transient failures. Every step writes structured
//! events into the journal — payloads are digested, never plaintext.

use dexter_core::{
    Action, ActionResult, ActionStatus, AppSelector, Event, EventKind, ExecutionRoute,
    ExpectedState, Observation, ObservationScope, Rect, SemanticTarget, Target, ValuePredicate,
    Verification, VerificationStatus,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError, WakeHandle};
use dexter_policy::{ActionContext, ApprovalStore, Policy, PolicyDecision};
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

pub mod presence;

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
    /// Settle after a completed act before the next observe — real apps
    /// propagate state asynchronously, so the `done_when` check can
    /// otherwise run against a world that hasn't updated yet.
    pub post_act_settle: Duration,
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
            post_act_settle: Duration::ZERO,
            allow_coordinates: false,
            approve_all: false,
            observe_max_elements: 4_000,
        }
    }
}

/// What the journal captures. `Audit` (default) redacts payloads and
/// decision contexts — the public trail agents and operators read.
/// `Training` keeps the full decision context (`CandidatesGenerated`
/// `context`, full `DecisionMade`) for the eval harness to replay —
/// a private capture, never the MCP-facing journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TraceMode {
    #[default]
    Audit,
    Training,
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
    trace: TraceMode,
}

impl<D: ComputerDriver> Engine<D> {
    pub fn new(driver: D, policy: Policy, approval_ttl: Duration) -> Self {
        Self {
            driver,
            policy,
            approvals: ApprovalStore::new(approval_ttl),
            journal: Default::default(),
            journal_sink: None,
            trace: TraceMode::Audit,
        }
    }

    /// Switch the journal to full-context capture (`Training`) — the
    /// eval harness uses it to replay decision contexts; the default
    /// `Audit` mode redacts payloads and contexts.
    pub fn set_trace_mode(&mut self, mode: TraceMode) {
        self.trace = mode;
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

    /// Emit a caller-level event through the same journal + live sink
    /// path as engine events — used to mark terminal state for runs
    /// that don't go through `run_plan` (e.g. single-act commands).
    pub fn emit(&mut self, kind: EventKind, data: serde_json::Value) {
        self.journal(kind, data);
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
        // The journal carries the target's on-screen bounds so the
        // presence overlay can draw the cursor where the act lands.
        // run_plan already holds a live observation; single steps take
        // one here — cosmetic only, a failed observe never blocks the act.
        let mut obs = if bounds_need_observation(&step.action) {
            let scope = ObservationScope {
                app: step.app.clone().or_else(|| cfg.app.clone()),
                max_elements: cfg.observe_max_elements,
                ..Default::default()
            };
            self.driver.observe(&scope).ok()
        } else {
            None
        };
        // Windowless borrow — macOS only surfaces an app's AX window
        // tree while it is frontmost. Centralized here so CLI, MCP and
        // SDK paths share the contract; `WakeGuard` restores focus on
        // every return path.
        let app = step.app.clone().or_else(|| cfg.app.clone());
        let scope = ObservationScope {
            app: app.clone(),
            max_elements: cfg.observe_max_elements,
            ..Default::default()
        };
        let wake = self.maybe_wake(app.as_ref(), &mut obs, &scope);
        let status = self.run_step_inner(step, cfg, obs.as_ref());
        if let Some(h) = wake {
            self.driver.restore(&h);
        }
        status
    }

    /// Borrow the stage once when a scoped observation shows no window
    /// content — some platforms (macOS) only expose an app's AX window
    /// tree while it is frontmost. On wake the observation is refreshed
    /// in place so callers act on the world the wake produced. The
    /// caller restores the returned handle — every terminal path
    /// funnels through a single point that hands focus back.
    fn maybe_wake(
        &self,
        app: Option<&AppSelector>,
        obs: &mut Option<Observation>,
        scope: &ObservationScope,
    ) -> Option<WakeHandle> {
        let app = app?;
        let windowed = obs.as_ref().is_some_and(|o| {
            o.elements
                .iter()
                .any(|e| e.role.as_deref() == Some("window"))
        });
        if windowed {
            return None;
        }
        let handle = self.driver.wake(app).ok()?;
        if !handle.activated {
            return None;
        }
        std::thread::sleep(Duration::from_millis(800));
        *obs = self.driver.observe(scope).ok().or_else(|| obs.take());
        Some(handle)
    }

    /// `obs` is the live observation when one exists (the `run_task` loop);
    /// it lets the journal carry the target's on-screen bounds so an
    /// overlay can render presence without touching the machine.
    ///
    /// v2 execution contract: plan → authorize the concrete route →
    /// execute **once** → verify by polling. A slow world never causes
    /// a second mutation; a second mutation requires a fresh plan and a
    /// fresh grant.
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
        let act_ctx = ActContext {
            app: app.clone(),
            allow_coordinates: cfg.allow_coordinates,
        };

        // PLAN — read-only: the driver declares the route space, each
        // route carrying its real mechanism and tier. No routes is an
        // honest "unsupported"; the verdict still belongs to policy, so
        // evaluate the action's declared tier rather than erroring blind.
        let plan = match self.driver.plan(&step.action, &act_ctx) {
            Ok(p) => p,
            Err(e) => {
                self.journal(
                    EventKind::ActionFailed,
                    serde_json::json!({"error": e.to_string(), "stage": "plan"}),
                );
                return StepStatus::Errored { error: e };
            }
        };
        let mut routes: Vec<ExecutionRoute> = if plan.routes.is_empty() {
            vec![ExecutionRoute::legacy(&step.action)]
        } else {
            plan.routes
        };
        // Element-bound routes carry only ids unless the driver
        // resolved them — fill role/name/identifier from the live
        // observation so policy `target` matchers and the grant
        // fingerprint see semantic identity, not ephemeral handles.
        for route in &mut routes {
            enrich_descriptor(&mut route.target, obs);
        }

        let bounds = target_bounds(&step.action, obs);
        let mut executed: Option<ActionResult> = None;
        let mut last_refusal = String::from("no route executed");

        // AUTHORIZE + EXECUTE — each route independently. One approval
        // covers exactly one route's execution: a fallback route is a
        // new authorization question, not a silent escalation.
        for (index, route) in routes.iter().enumerate() {
            let fp = dexter_policy::fingerprint_route(route, &ctx);
            self.journal(
                EventKind::ActionProposed,
                serde_json::json!({
                    "action": audit::action_summary(&route.action),
                    "app": &app,
                    "fingerprint": &fp,
                    "intrusiveness": route.intrusiveness,
                    "mechanism": route.mechanism,
                    "route": index,
                    "of": routes.len(),
                    "target_bounds": bounds,
                }),
            );

            match self.policy.evaluate_route(route, &ctx) {
                PolicyDecision::Allow => {}
                PolicyDecision::Deny { reason } => {
                    self.journal(
                        EventKind::PolicyChecked,
                        serde_json::json!({
                            "decision": "deny",
                            "reason": &reason,
                            "intrusiveness": route.intrusiveness,
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
                            "intrusiveness": route.intrusiveness,
                        }),
                    );
                }
            }

            let result = match self.driver.execute(route, &act_ctx) {
                Ok(r) => r,
                Err(e) => {
                    self.journal(
                        EventKind::ActionFailed,
                        serde_json::json!({"error": e.to_string(), "route": index}),
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
                    "route": index,
                }),
            );

            // A declared mechanism is a promise: if the driver used a
            // different one, the act that ran is not the act that was
            // authorized — report the mismatch instead of laundering it.
            if let Some(declared) = route.mechanism {
                if result.mechanism != declared {
                    self.journal(
                        EventKind::ActionFailed,
                        serde_json::json!({
                            "error": format!(
                                "authorized mechanism {declared:?} but driver used {:?}",
                                result.mechanism
                            ),
                            "route": index,
                        }),
                    );
                    return StepStatus::Failed {
                        reason: format!(
                            "mechanism mismatch: authorized {declared:?}, executed {:?}",
                            result.mechanism
                        ),
                        attempts: (index + 1) as u32,
                    };
                }
            }

            if result.status.ok() {
                executed = Some(result);
                break;
            }
            last_refusal = format!("{:?}: {}", result.status, result.detail.unwrap_or_default());
            // Unsupported is the one verdict another route may fix —
            // anything else is final.
            if result.status == ActionStatus::Unsupported && index + 1 < routes.len() {
                self.journal(
                    EventKind::RecoveryStarted,
                    serde_json::json!({
                        "strategy": "next_route",
                        "trigger": "unsupported",
                        "route": index + 1,
                    }),
                );
                continue;
            }
            return StepStatus::Failed {
                reason: last_refusal,
                attempts: (index + 1) as u32,
            };
        }

        let Some(result) = executed else {
            return StepStatus::Failed {
                reason: format!("every planned route refused — last: {last_refusal}"),
                attempts: routes.len() as u32,
            };
        };

        let Some(expected) = &step.expect else {
            return StepStatus::Done {
                result,
                verification: None,
                attempts: 1,
            };
        };

        // VERIFY-POLL — the world may need time to reach the expected
        // state; re-observe, never re-execute. `max_attempts` is the v1
        // alias for the poll bound (renamed `verify_attempts` upstream).
        let verify_attempts = step.max_attempts.unwrap_or(cfg.max_attempts).max(1);
        let mut last_detail = String::new();
        for attempt in 1..=verify_attempts {
            if attempt > 1 {
                self.journal(
                    EventKind::RecoveryStarted,
                    serde_json::json!({
                        "strategy": "verify_poll",
                        "trigger": "unverified",
                        "attempt": attempt,
                    }),
                );
            }
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
            // The effect taxonomy rides the journal: VERIFIED is a
            // confirmed effect, an unchanged signature is the suspected
            // no-op, and everything else the engine couldn't prove is
            // honestly unverifiable.
            let effect = match verification.status {
                VerificationStatus::Verified => "confirmed",
                VerificationStatus::Uncertain => "unverifiable",
                VerificationStatus::Failed => {
                    let noop = verification
                        .checks
                        .iter()
                        .any(|c| c.starts_with("world_changed") && c.ends_with(": false"));
                    if noop {
                        "suspected_noop"
                    } else {
                        "unverifiable"
                    }
                }
            };
            self.journal(
                if verification.status == VerificationStatus::Verified {
                    EventKind::VerificationPassed
                } else {
                    EventKind::VerificationFailed
                },
                serde_json::json!({
                    "status": format!("{:?}", verification.status),
                    "effect": effect,
                    "unknown_reason": verification.unknown_reason,
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
                }
            }
        }

        StepStatus::Failed {
            reason: format!("verification never reached VERIFIED — last: {last_detail}"),
            attempts: verify_attempts,
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
    /// Run one goal — kept for callers that want a single-intent task.
    /// Equivalent to `run_plan` with one structural subgoal.
    pub fn run_task(
        &mut self,
        goal: &str,
        generator: &dyn dexter_decision::CandidateGenerator,
        decider: &dyn dexter_decision::DecisionEngine,
        cfg: &TaskConfig,
    ) -> TaskOutcome {
        self.run_plan(
            &[Subgoal {
                goal: goal.to_string(),
                done_when: Some(cfg.done_when.clone()),
            }],
            generator,
            decider,
            cfg,
        )
        .unwrap_single()
    }

    /// Run ordered subgoals through the same observe → generate → decide
    /// → act → verify loop. Each subgoal gets fresh history (repeat
    /// penalties must not leak across intents) and shares the journal —
    /// `SubgoalStarted/Completed/Failed` events carry `index`/`of` so
    /// partial progress is auditable and failures attributable.
    ///
    /// A subgoal with `done_when: None` auto-completes after the first
    /// mutating act that verifiably changed the world — act success
    /// alone is never trusted, the next observation must differ.
    pub fn run_plan(
        &mut self,
        subgoals: &[Subgoal],
        generator: &dyn dexter_decision::CandidateGenerator,
        decider: &dyn dexter_decision::DecisionEngine,
        cfg: &TaskConfig,
    ) -> PlanOutcome {
        let started = Instant::now();
        let mut total_steps = 0u32;
        for (index, sub) in subgoals.iter().enumerate() {
            self.journal(
                EventKind::SubgoalStarted,
                serde_json::json!({"index": index, "of": subgoals.len(), "goal": sub.goal}),
            );
            let done = match &sub.done_when {
                Some(d) => Completion::Structural(d.clone()),
                None => Completion::FirstVerifiedAct,
            };
            let outcome = self.run_goal(&sub.goal, &done, generator, decider, cfg, started);
            match outcome {
                TaskOutcome::Completed { steps } => {
                    total_steps += steps;
                    self.journal(
                        EventKind::SubgoalCompleted,
                        serde_json::json!({"index": index, "of": subgoals.len(), "steps": steps}),
                    );
                }
                other => {
                    self.journal(
                        EventKind::SubgoalFailed,
                        serde_json::json!({"index": index, "of": subgoals.len(), "goal": sub.goal, "outcome": format!("{other:?}")}),
                    );
                    return PlanOutcome::Failed {
                        index,
                        goal: sub.goal.clone(),
                        inner: Box::new(other),
                        completed: index,
                    };
                }
            }
        }
        PlanOutcome::Completed {
            subgoals: subgoals.len(),
            steps: total_steps,
        }
    }

    /// The per-goal closed loop shared by `run_task` and `run_plan`.
    /// `task_started` is the plan-level clock for `max_duration`.
    /// Borrows the stage once when the app's first observation is
    /// windowless and hands focus back on every outcome — the one-path
    /// wake/restore contract for CLI, MCP and SDK callers.
    fn run_goal(
        &mut self,
        goal: &str,
        done: &Completion,
        generator: &dyn dexter_decision::CandidateGenerator,
        decider: &dyn dexter_decision::DecisionEngine,
        cfg: &TaskConfig,
        task_started: Instant,
    ) -> TaskOutcome {
        let scope = ObservationScope {
            app: cfg.run.app.clone(),
            max_elements: cfg.run.observe_max_elements,
            ..Default::default()
        };
        let mut obs = self.driver.observe(&scope).ok();
        let wake = self.maybe_wake(cfg.run.app.as_ref(), &mut obs, &scope);
        // The wake-check observation doubles as step 1's — observing is
        // not free (drivers tick, AX walks cost), so the loop must not
        // pay for a second one.
        let outcome = self.run_goal_loop(goal, done, generator, decider, cfg, task_started, obs);
        if let Some(h) = wake {
            self.driver.restore(&h);
        }
        outcome
    }

    #[allow(clippy::too_many_arguments)] // single call site; a params struct is noise
    fn run_goal_loop(
        &mut self,
        goal: &str,
        done: &Completion,
        generator: &dyn dexter_decision::CandidateGenerator,
        decider: &dyn dexter_decision::DecisionEngine,
        cfg: &TaskConfig,
        task_started: Instant,
        mut initial_obs: Option<Observation>,
    ) -> TaskOutcome {
        use dexter_decision::{Decision, DecisionContext, GenHistory, Route};
        let started = task_started;
        let mut last_error: Option<String> = None;
        let mut hist = GenHistory::default();
        // Auto-completion state: the world signature at the moment the
        // last mutating act was decided on. If the next observation
        // differs, the act moved the world — the subgoal is done.
        let mut pending_sig: Option<u64> = None;
        let scope = ObservationScope {
            app: cfg.run.app.clone(),
            max_elements: cfg.run.observe_max_elements,
            ..Default::default()
        };

        for step in 1..=cfg.max_steps {
            // Cooperative stop + wall-clock bound — checked before each
            // observe so a cancelled/overtime task never takes another
            // action.
            if cfg
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            {
                self.journal(
                    EventKind::TaskCancelled,
                    serde_json::json!({"step": step - 1, "elapsed_ms": started.elapsed().as_millis() as u64}),
                );
                return TaskOutcome::Cancelled;
            }
            if let Some(deadline) = cfg.max_duration {
                if started.elapsed() > deadline {
                    self.journal(
                        EventKind::TaskTimedOut,
                        serde_json::json!({"step": step - 1, "elapsed_ms": started.elapsed().as_millis() as u64, "budget_ms": deadline.as_millis() as u64}),
                    );
                    return TaskOutcome::TimedOut {
                        elapsed: started.elapsed(),
                    };
                }
            }
            let obs = match initial_obs
                .take()
                .map(Ok)
                .unwrap_or_else(|| self.driver.observe(&scope))
            {
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
            match done {
                Completion::Structural(expected) => {
                    let check = dexter_verify::verify(&obs, expected);
                    if check.status == VerificationStatus::Verified {
                        self.journal(
                            EventKind::TaskCompleted,
                            serde_json::json!({"steps": step - 1, "elapsed_ms": started.elapsed().as_millis() as u64}),
                        );
                        return TaskOutcome::Completed { steps: step - 1 };
                    }
                }
                Completion::FirstVerifiedAct => {
                    // Auto-completion: the world must have changed since
                    // the act that claimed it. No pending act, no claim.
                    if let Some(sig) = pending_sig {
                        if dexter_world_model::signature(&obs) != sig {
                            self.journal(
                                EventKind::TaskCompleted,
                                serde_json::json!({"steps": step - 1, "mode": "first_verified_act", "elapsed_ms": started.elapsed().as_millis() as u64}),
                            );
                            return TaskOutcome::Completed { steps: step - 1 };
                        }
                    } else if dexter_decision::goal_already_satisfied(&obs, goal) {
                        // Inherited state: the destination control is
                        // already selected — the intent held before we
                        // acted, so pressing it would stall on a no-op.
                        self.journal(
                            EventKind::TaskCompleted,
                            serde_json::json!({"steps": step - 1, "mode": "already_satisfied", "elapsed_ms": started.elapsed().as_millis() as u64}),
                        );
                        return TaskOutcome::Completed { steps: step - 1 };
                    }
                }
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
            // The full decision context is journaled in Training mode —
            // this is what makes a trace replayable offline (eval harness
            // re-feeds it to any DecisionEngine without touching the
            // machine). The public audit trail gets digests instead.
            self.journal(
                EventKind::CandidatesGenerated,
                match self.trace {
                    TraceMode::Training => serde_json::json!({
                        "count": ctx.candidates.len(),
                        "step": step,
                        "context": &ctx,
                    }),
                    TraceMode::Audit => audit::candidates_summary(&ctx),
                },
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
                    "decision": match self.trace {
                        TraceMode::Training => serde_json::to_value(&decision).unwrap_or_default(),
                        TraceMode::Audit => audit::decision_summary(&decision),
                    },
                    "step": step,
                }),
            );

            match decision {
                Decision::Act {
                    action, rationale, ..
                } => {
                    let mutating = is_mutating(&action);
                    hist.attempt_names.push(attempt_label(&action, &obs));
                    hist.attempts.push(action.clone());
                    // Every mutating act earns a derived expectation —
                    // progress is judged by done_when AND by evidence the
                    // act itself landed. Acts the model can't express an
                    // outcome for stay unverified, honestly.
                    let expect = derive_expect(&action, &obs);
                    let status = self.run_step_inner(
                        &Step {
                            note: Some(rationale),
                            action,
                            expect,
                            max_attempts: Some(3),
                            app: cfg.run.app.clone(),
                        },
                        &cfg.run,
                        Some(&obs),
                    );
                    match status {
                        StepStatus::Done { .. } => {
                            last_error = None;
                            // Auto-completion bookkeeping: a successful
                            // mutating act claims the subgoal — the next
                            // observation decides whether the world
                            // actually moved.
                            if matches!(done, Completion::FirstVerifiedAct) && mutating {
                                pending_sig = Some(dexter_world_model::signature(&obs));
                            }
                        }
                        StepStatus::NeedsApproval {
                            fingerprint,
                            reason,
                        } => {
                            // Policy pause is an outcome, not an error —
                            // the caller grants and retries; spinning
                            // here would burn the step budget denied.
                            self.journal(
                                EventKind::TaskFailed,
                                serde_json::json!({"outcome": "needs_approval", "fingerprint": &fingerprint, "reason": &reason, "step": step}),
                            );
                            return TaskOutcome::NeedsApproval {
                                fingerprint,
                                reason,
                            };
                        }
                        StepStatus::Denied { reason } => {
                            self.journal(
                                EventKind::TaskFailed,
                                serde_json::json!({"outcome": "denied", "reason": &reason, "step": step}),
                            );
                            return TaskOutcome::Denied { reason };
                        }
                        other => {
                            last_error = Some(format!("{other:?}"));
                        }
                    }
                    // Let a real app's state propagate before the next
                    // observe judges done_when — live UI is async.
                    if !cfg.run.post_act_settle.is_zero() {
                        std::thread::sleep(cfg.run.post_act_settle);
                    }
                }
                Decision::Route { route, rationale } => match route {
                    Route::Wait { millis } => {
                        // Interruptible wait — poll the cancel token in
                        // 25ms slices so a long wait still stops fast.
                        let total = millis.min(10_000);
                        let mut slept = 0u64;
                        while slept < total {
                            if cfg
                                .cancel
                                .as_ref()
                                .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
                            {
                                self.journal(
                                    EventKind::TaskCancelled,
                                    serde_json::json!({"step": step, "elapsed_ms": started.elapsed().as_millis() as u64}),
                                );
                                return TaskOutcome::Cancelled;
                            }
                            let slice = (total - slept).min(25);
                            std::thread::sleep(Duration::from_millis(slice));
                            slept += slice;
                        }
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

/// Journal payload shaping — the audit contract is that typed values,
/// set values and secrets never appear in any event, in any mode.
/// `Training` keeps *contexts* (candidate lists, digests) for replay;
/// action payloads are digested even there.
pub(crate) mod audit {
    use dexter_core::Action;

    /// The journal-visible shape of an action: structure and target are
    /// kept; payload content becomes `{len, sha256}` so approvals and
    /// audits can reference it without ever exposing it.
    pub fn action_summary(action: &Action) -> serde_json::Value {
        let payload = |field: &str, text: &str| {
            serde_json::json!({
                field: {
                    "len": text.len(),
                    "sha256": dexter_policy::payload_digest(text),
                }
            })
        };
        match action {
            Action::Click { target, button } => {
                serde_json::json!({"type": "click", "target": target, "button": button})
            }
            Action::TypeText { text, target } => serde_json::json!({
                "type": "type_text",
                "target": target,
                "payload": payload("text", text),
            }),
            Action::Key { chord } => serde_json::json!({"type": "key", "chord": chord}),
            Action::Scroll { delta, target } => {
                serde_json::json!({"type": "scroll", "delta": delta, "target": target})
            }
            Action::Focus { target } => {
                serde_json::json!({"type": "focus", "target": target})
            }
            Action::SetValue { target, value } => serde_json::json!({
                "type": "set_value",
                "target": target,
                "payload": payload("value", value),
            }),
            Action::Observe => serde_json::json!({"type": "observe"}),
            Action::Wait { millis } => serde_json::json!({"type": "wait", "millis": millis}),
            Action::Navigate { url } => serde_json::json!({"type": "navigate", "url": url}),
        }
    }

    /// A decision with its action redacted — same outer shape as the
    /// `Decision` serde (`{"type": ...}`) so overlay consumers read it
    /// identically.
    pub fn decision_summary(decision: &dexter_decision::Decision) -> serde_json::Value {
        match decision {
            dexter_decision::Decision::Act {
                action,
                candidate_index,
                rationale,
            } => serde_json::json!({
                "type": "act",
                "action": action_summary(action),
                "candidate_index": candidate_index,
                "rationale": rationale,
            }),
            dexter_decision::Decision::Route { route, rationale } => serde_json::json!({
                "type": "route",
                "route": route,
                "rationale": rationale,
            }),
        }
    }

    /// The `CandidatesGenerated` payload for the public audit trail:
    /// counts and digests, no world contents and no candidate payloads.
    pub fn candidates_summary(ctx: &dexter_decision::DecisionContext) -> serde_json::Value {
        serde_json::json!({
            "goal": ctx.goal,
            "step": ctx.step,
            "count": ctx.candidates.len(),
            "state_digest_sha256": dexter_policy::payload_digest(&ctx.state_digest),
            "candidates": ctx.candidates.iter().map(|c| serde_json::json!({
                "action": action_summary(&c.action),
                "rationale": c.rationale,
                "prior": c.prior,
            })).collect::<Vec<_>>(),
            "last_error": ctx.last_error,
        })
    }
}

/// Char budget for the digest handed to decision engines — sized so
/// Laya-class encoders (8192 tokens) never overflow on big AX trees.
const STATE_BUDGET: usize = 14_000;

/// The on-screen rect an action will land on — the presence contract an
/// overlay renders from. Resolved against the live observation for
/// element targets; a `Point` is its own (1×1) rect; `Navigate` has none.
/// Does this action's target need a live observation to find bounds?
/// Points carry their own coordinates; semantic/element/window targets
/// resolve against the observed tree.
fn bounds_need_observation(action: &Action) -> bool {
    let target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. } => Some(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        _ => None,
    };
    matches!(target, Some(t) if !matches!(t, Target::Point { .. }))
}

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
    /// A step required human approval — the task pauses instead of
    /// burning its step budget. Grant the fingerprint and retry.
    NeedsApproval { fingerprint: String, reason: String },
    /// Policy denied a step outright — a verdict, not a transient error.
    Denied { reason: String },
    /// Runtime/driver/decision failure.
    Failed { reason: String },
    /// Step bound reached without `done_when` verifying.
    MaxSteps,
    /// Cooperative cancellation (`TaskConfig::cancel` set mid-run).
    Cancelled,
    /// Wall-clock budget exceeded (`TaskConfig::max_duration`).
    TimedOut { elapsed: Duration },
}

/// One ordered intent in a `run_plan` sequence.
#[derive(Debug, Clone)]
pub struct Subgoal {
    /// Goal text handed to the generator and decider.
    pub goal: String,
    /// Structural completion check. `None` = auto-complete after the
    /// first mutating act that verifiably changed the world.
    pub done_when: Option<ExpectedState>,
}

/// How a subgoal knows it's done — internal to the loop.
enum Completion {
    /// `ExpectedState` verified against every fresh observation.
    Structural(ExpectedState),
    /// First mutating act that provably moved the world.
    FirstVerifiedAct,
}

/// Result of `run_plan` — unlike `TaskOutcome`, failures carry which
/// subgoal failed and how many completed before it.
#[derive(Debug)]
pub enum PlanOutcome {
    /// Every subgoal completed.
    Completed { subgoals: usize, steps: u32 },
    /// Subgoal `index` failed; `completed` subgoals ran clean before it.
    /// `inner` is that subgoal's own outcome (Abstained, MaxSteps, ...).
    Failed {
        index: usize,
        goal: String,
        inner: Box<TaskOutcome>,
        completed: usize,
    },
}

impl PlanOutcome {
    /// Single-subgoal plans unwrap to the inner `TaskOutcome` unchanged —
    /// `run_task` callers keep their original result shape.
    fn unwrap_single(self) -> TaskOutcome {
        match self {
            PlanOutcome::Completed { steps, .. } => TaskOutcome::Completed { steps },
            PlanOutcome::Failed { inner, .. } => *inner,
        }
    }
}

/// Fill a route descriptor's semantic fields from the element the
/// target ids point at — only when they belong to *this* observation
/// (foreign ids are never resolved across worlds). Drivers that already
/// resolved keep their values.
fn enrich_descriptor(desc: &mut dexter_core::TargetDescriptor, obs: Option<&Observation>) {
    let (Some(el_id), Some(obs_id)) = (desc.element, desc.observation) else {
        return;
    };
    let Some(obs) = obs else { return };
    if obs.id != obs_id {
        return;
    }
    if let Some(el) = obs.elements.iter().find(|e| e.id == el_id) {
        if desc.role.is_none() {
            desc.role = el.role.clone();
        }
        if desc.name.is_none() {
            desc.name = el.name.clone();
        }
        if desc.identifier.is_none() {
            desc.identifier = el.identifier.clone();
        }
    }
}

/// The label an attempted action resolved to on this observation —
/// element targets carry no name, so the engine resolves it while the
/// world that produced the candidate is still at hand.
fn attempt_label(action: &Action, obs: &Observation) -> Option<String> {
    let target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. } => Some(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        _ => None,
    }?;
    dexter_world_model::resolve_element(obs, target)
        .ok()
        .and_then(|e| e.label().map(str::to_string))
}

/// Does this action mutate the world (as opposed to observing or
/// positioning)? Auto-completion only credits mutating acts.
fn is_mutating(action: &Action) -> bool {
    matches!(
        action,
        Action::Click { .. }
            | Action::SetValue { .. }
            | Action::TypeText { .. }
            | Action::Key { .. }
            | Action::Navigate { .. }
    )
}

/// The semantic identity an act's target refers to — for deriving a
/// checkable expectation. `Element`/`Focused` resolve through the
/// observation to role+name+identifier; `Semantic` passes through.
/// `Point`/`Window` have no semantic identity → `None`.
fn semantic_for(target: &Target, obs: &Observation) -> Option<SemanticTarget> {
    match target {
        Target::Semantic(s) => Some(s.clone()),
        Target::Element { element, .. } => {
            let el = obs.elements.iter().find(|e| e.id == *element)?;
            Some(SemanticTarget {
                role: el.role.clone(),
                name: el.name.clone(),
                identifier: el.identifier.clone(),
                ..Default::default()
            })
        }
        Target::Focused => {
            let el = obs.elements.iter().find(|e| e.focused)?;
            Some(SemanticTarget {
                role: el.role.clone(),
                name: el.name.clone(),
                identifier: el.identifier.clone(),
                ..Default::default()
            })
        }
        Target::Point { .. } | Target::Window { .. } => None,
    }
}

/// The minimal check a mutating act must survive before the loop may
/// claim progress — the per-act half of "verify every act". Clicks get
/// the signature-diff catch-all (their effect can't be predicted);
/// value acts get read-back predicates; `Focus` checks focus landed.
/// Acts whose effect the world model cannot express — Scroll, Key,
/// Navigate, Wait, Observe, point clicks — return `None` and stay on
/// the unverified path rather than carrying a check that cannot fail
/// honestly.
fn derive_expect(action: &Action, obs: &Observation) -> Option<ExpectedState> {
    match action {
        Action::Click { target, .. } => match target {
            Target::Point { .. } => None,
            _ => Some(ExpectedState::WorldChanged {
                from: dexter_world_model::signature(obs),
            }),
        },
        Action::TypeText { text, target } => {
            let t = target.clone().unwrap_or(Target::Focused);
            semantic_for(&t, obs).map(|st| ExpectedState::ElementValue {
                target: st,
                predicate: ValuePredicate::Contains(text.clone()),
            })
        }
        Action::SetValue { target, value } => {
            semantic_for(target, obs).map(|st| ExpectedState::ElementValue {
                target: st,
                predicate: ValuePredicate::Equals(value.clone()),
            })
        }
        Action::Focus { target } => {
            semantic_for(target, obs).map(|st| ExpectedState::FocusedElement { target: st })
        }
        _ => None,
    }
}

/// `run_task` parameters.
pub struct TaskConfig {
    pub run: RunConfig,
    /// Hard bound on decide/act iterations — per subgoal in `run_plan`.
    pub max_steps: u32,
    /// Optional wall-clock bound — checked per step, alongside
    /// `max_steps`. `None` = unbounded (steps still apply).
    pub max_duration: Option<Duration>,
    /// Cooperative cancellation token — whoever holds a clone can stop
    /// the task between steps (checked before each observe).
    pub cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Structural goal check — verified against every observation.
    pub done_when: ExpectedState,
}
