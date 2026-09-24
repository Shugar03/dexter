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
    ExpectedState, Intrusiveness, Observation, ObservationScope, Rect, SemanticTarget, Sensitivity,
    Target, TargetDescriptor, ValuePredicate, Verification, VerificationStatus,
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

/// The verify-poll's terminal verdict plus the attempts it spent —
/// carried back so the caller shapes `StepStatus` without re-deriving.
#[derive(Debug)]
struct PollOutcome {
    verification: Verification,
    attempts: u32,
}

/// How a step ended.
#[derive(Debug)]
pub enum StepStatus {
    /// Action executed (and expectation verified, when present).
    Done {
        /// The driver's delivery report — `None` when execute errored
        /// but the expectation verified anyway (the side effect landed
        /// despite the broken report; there is no result to relay).
        result: Option<ActionResult>,
        verification: Option<Verification>,
        attempts: u32,
    },
    /// Policy denied the action outright.
    Denied { reason: String },
    /// Policy requires approval and no live grant covers this fingerprint.
    /// `action` is the redacted summary the operator approves — payloads
    /// stay digest tokens.
    NeedsApproval {
        fingerprint: String,
        reason: String,
        action: serde_json::Value,
    },
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
    /// Pin every observation of the run to one window (a `Window::id`
    /// from a prior observe). Multi-window apps otherwise walk every
    /// window's tree per step — the caller that knows the workspace
    /// window pays O(window) instead of O(app). All observes share the
    /// scope so signatures stay comparable.
    pub window_scope: Option<u32>,
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
            window_scope: None,
        }
    }
}

/// What the journal captures. `Audit` (default) redacts payloads and
/// decision contexts — the public trail agents and operators read.
/// `Training` keeps the full decision context (`CandidatesGenerated`
/// `context`, full `DecisionMade`) for the eval harness to replay —
/// still payload-scrubbed: typed/set/clipboard secrets appear only as
/// digest tokens, in every mode.
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
    /// Fingerprints of stage borrows the human granted this session.
    /// A wake is a repeated side effect — every retry re-activates the
    /// app — so unlike an execute grant (single-use, one act), a granted
    /// stage borrow covers re-borrowing the same app for the session.
    stage_grants: std::collections::HashSet<String>,
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
            stage_grants: Default::default(),
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

    /// Detach the journal sink — a per-act overlay file must not keep
    /// capturing unrelated events once the act that spawned it ends.
    pub fn clear_journal_sink(&mut self) {
        self.journal_sink = None;
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
        // A mutating action without an explicit expect also needs the
        // pre-act world — its derived expectation verifies the effect.
        // Stage-needing actions observe so the wake oracle can tell a
        // genuinely windowless app from "no observation taken".
        let needs_obs = bounds_need_observation(&step.action)
            || (step.expect.is_none() && derives(&step.action))
            || needs_stage(&step.action);
        let obs = if needs_obs {
            let scope = ObservationScope {
                app: step.app.clone().or_else(|| cfg.app.clone()),
                max_elements: cfg.observe_max_elements,
                window: cfg.window_scope,
                ..Default::default()
            };
            match self.observe_scoped(&scope) {
                Ok(o) => Some(o),
                // A failed pre-observation is not silent: any act that
                // proceeds is unverified, and the journal must say why.
                Err(e) => {
                    self.journal(
                        EventKind::ObservationFailed,
                        serde_json::json!({"error": e.to_string(), "context": "pre_act"}),
                    );
                    None
                }
            }
        } else {
            None
        };
        // The stage borrow happens inside run_step_inner: the route
        // about to execute decides whether the app must be frontmost —
        // a background route (a menu AXPress) never steals focus — and
        // the wake itself goes through policy there, as a visible side
        // effect. The handle is restored here so the whole step —
        // verify-poll included — sees the world the wake produced.
        let app = step.app.clone().or_else(|| cfg.app.clone());
        let scope = ObservationScope {
            app: app.clone(),
            max_elements: cfg.observe_max_elements,
            window: cfg.window_scope,
            ..Default::default()
        };
        // An explicit expect wins; otherwise mutating actions verify
        // their effect by derivation — the same contract goal flow has.
        // A second derivation runs inside run_step_inner after a stage
        // borrow, on the world the wake produced.
        let derived;
        let step = if step.expect.is_none() {
            derived = obs
                .as_ref()
                .and_then(|o| derive_expect(&step.action, o, cfg.window_scope.is_some()))
                .map(|e| Step {
                    expect: Some(e),
                    ..step.clone()
                });
            derived.as_ref().unwrap_or(step)
        } else {
            step
        };
        let mut wake = None;
        let status = self.run_step_inner(step, cfg, obs.as_ref(), &scope, &mut wake, None);
        if let Some(h) = wake {
            self.driver.restore(&h);
        }
        status
    }

    /// Authorize and perform a stage borrow — foregrounding `app` is a
    /// visible side effect, so it goes through `evaluate_route` like
    /// any mutation: the route it asks for is the launch-or-activate it
    /// actually performs (`Api` mechanism, `Visual` tier). A policy
    /// `deny` on `launch_app` is a denied stage steal; an unapproved
    /// one surfaces its fingerprint — never an unauthenticated
    /// activation. `Clear` means no activation was needed; `Activated`
    /// carries the restore handle and the refreshed observation.
    fn authorize_stage(
        &mut self,
        app: &AppSelector,
        ctx: &ActionContext,
        cfg: &RunConfig,
        scope: &ObservationScope,
    ) -> StageOutcome {
        let route = stage_route(app);
        let fp = dexter_policy::fingerprint_route(&route, ctx);
        self.journal(
            EventKind::ActionProposed,
            serde_json::json!({
                "action": audit::action_summary(&route.action),
                "app": app,
                "fingerprint": &fp,
                "intrusiveness": route.intrusiveness,
                "mechanism": route.mechanism,
                "stage": "wake",
            }),
        );
        match self.policy.evaluate_route(&route, ctx) {
            PolicyDecision::Allow => {}
            PolicyDecision::Deny { reason } => {
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({"decision": "deny", "reason": &reason, "stage": "wake"}),
                );
                return StageOutcome::Refused { reason };
            }
            PolicyDecision::RequireApproval { reason } => {
                // A granted stage borrow lasts the session: every retry
                // re-activates the app, so single-use consumption here
                // would demand a new grant per retry of the same step.
                let granted = cfg.approve_all
                    || self.stage_grants.contains(&fp)
                    || self.approvals.check_and_consume(&fp);
                if !granted {
                    self.journal(
                        EventKind::HumanApprovalRequired,
                        serde_json::json!({
                            "fingerprint": &fp,
                            "reason": &reason,
                            "action": audit::action_summary(&route.action),
                            "stage": "wake",
                        }),
                    );
                    return StageOutcome::Approval {
                        fingerprint: fp,
                        reason,
                        action: audit::action_summary(&route.action),
                    };
                }
                self.stage_grants.insert(fp);
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({"decision": "approved", "stage": "wake"}),
                );
            }
        }
        let handle = match self.driver.wake(app) {
            Ok(h) => h,
            Err(e) => return StageOutcome::Errored(e),
        };
        if !handle.activated {
            return StageOutcome::Clear;
        }
        // Activation returns before the window tree exists — poll the
        // scoped world until a window shows rather than sleeping a
        // fixed beat. Same 800ms budget, early exit on the common case.
        let mut new_obs = None;
        for _ in 0..10 {
            match self.observe_scoped(scope) {
                Ok(o) => {
                    let windowed = o
                        .elements
                        .iter()
                        .any(|e| e.role.as_deref() == Some("window"));
                    new_obs = Some(o);
                    if windowed {
                        break;
                    }
                }
                Err(e) => {
                    self.journal(
                        EventKind::ObservationFailed,
                        serde_json::json!({"error": e.to_string(), "context": "post_wake"}),
                    );
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        StageOutcome::Activated {
            handle,
            obs: new_obs,
        }
    }

    /// `driver.observe` plus the window-scope guarantee: when the scope
    /// pins a window, the returned observation is bounds-filtered to it.
    /// A driver whose scoped walk degrades to app-wide (macOS
    /// `collect_window` fallback, any driver that ignores `scope.window`)
    /// can't smuggle an unscoped world into verification — signatures and
    /// element checks would otherwise evaluate the whole app. Natively
    /// scoped observations pass through unchanged; a vanished pinned
    /// window is an honest observe error.
    fn observe_scoped(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        let mut scope = scope.clone();
        if scope.window.is_some() {
            // Menu elements are bounds-filtered out of a scoped
            // observation anyway — the menubar walk is pure cost.
            scope.include_menu = false;
        }
        let obs = self.driver.observe(&scope)?;
        match scope.window {
            Some(id) => dexter_world_model::scope_to_window(obs, id).map_err(DriverError::NotFound),
            None => Ok(obs),
        }
    }

    /// Evaluate one route through policy and journal the verdict —
    /// extracted so a route re-authorized on a post-wake world goes
    /// through the identical decision path.
    fn authorize_route(
        &mut self,
        route: &ExecutionRoute,
        fp: &str,
        ctx: &ActionContext,
        cfg: &RunConfig,
    ) -> RouteVerdict {
        match self.policy.evaluate_route(route, ctx) {
            PolicyDecision::Allow => RouteVerdict::Allow,
            PolicyDecision::Deny { reason } => {
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({
                        "decision": "deny",
                        "reason": &reason,
                        "intrusiveness": route.intrusiveness,
                    }),
                );
                RouteVerdict::Denied(reason)
            }
            PolicyDecision::RequireApproval { reason } => {
                if cfg.approve_all {
                    self.journal(
                        EventKind::HumanApprovalRequired,
                        serde_json::json!({
                            "fingerprint": fp,
                            "reason": &reason,
                            "granted": "approve_all",
                        }),
                    );
                } else if !self.approvals.check_and_consume(fp) {
                    self.journal(
                        EventKind::HumanApprovalRequired,
                        serde_json::json!({
                            "fingerprint": fp,
                            "reason": &reason,
                            "action": audit::action_summary(&route.action),
                        }),
                    );
                    return RouteVerdict::Approval {
                        fingerprint: fp.to_string(),
                        reason,
                    };
                }
                self.journal(
                    EventKind::PolicyChecked,
                    serde_json::json!({
                        "decision": "approved",
                        "fingerprint": fp,
                        "intrusiveness": route.intrusiveness,
                    }),
                );
                RouteVerdict::Allow
            }
        }
    }

    /// `obs` is the live observation when one exists (the `run_task` loop);
    /// it lets the journal carry the target's on-screen bounds so an
    /// overlay can render presence without touching the machine.
    /// `wake` is the caller's stage-borrow slot: set when this step
    /// activated an app so the caller restores focus once the whole
    /// step — verify-poll included — is done. `scope` is the
    /// observation scope a post-wake re-observe must honor.
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
        scope: &ObservationScope,
        wake: &mut Option<WakeHandle>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
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
        // The secrets floor rides the same resolution: an element the
        // driver marked sensitive upgrades the route before policy sees
        // it, on every driver.
        for route in &mut routes {
            enrich_descriptor(&mut route.target, obs);
            enforce_sensitivity_floor(route, obs);
            // A route with no mechanism claim can't tell the engine
            // whether it resolves against the stage — the action's own
            // stage need is the honest foreground claim (the legacy
            // compat route and v1 drivers land here).
            if route.mechanism.is_none() && needs_stage(&route.action) {
                route.requires_foreground = true;
            }
        }

        let bounds = target_bounds(&step.action, obs);
        let mut executed: Option<ActionResult> = None;
        let mut exec_error: Option<DriverError> = None;
        let mut last_refusal = String::from("no route executed");
        // The expectation the verify-poll checks — an explicit
        // `step.expect`, or one derived post-wake on the world the act
        // will actually execute in.
        let mut expect = step.expect.clone();
        // A windowless pre-act observation is what makes the stage
        // question real — asked per-route below, only for the route
        // about to execute. An absent observation never justifies one.
        let stage_needed = needs_stage(&step.action) && windowless(obs);
        // The stage question is answered once per step: `true` once a
        // verdict (activation, no-op, or refusal) landed, so a second
        // foreground route doesn't re-ask policy or re-call `wake`.
        // A refusal is remembered because the same fingerprint denies
        // identically — asking again would only duplicate the journal.
        let mut stage_answered = false;
        let mut stage_refusal: Option<String> = None;

        // AUTHORIZE + EXECUTE — each route independently. One approval
        // covers exactly one route's execution: a fallback route is a
        // new authorization question, not a silent escalation.
        for index in 0..routes.len() {
            let mut route = routes[index].clone();
            let mut fp = dexter_policy::fingerprint_route(&route, &ctx);
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

            match self.authorize_route(&route, &fp, &ctx, cfg) {
                RouteVerdict::Allow => {}
                RouteVerdict::Denied(reason) => return StepStatus::Denied { reason },
                RouteVerdict::Approval {
                    fingerprint,
                    reason,
                } => {
                    return StepStatus::NeedsApproval {
                        fingerprint,
                        reason,
                        action: audit::action_summary(&route.action),
                    };
                }
            }

            // STAGE — only the route about to execute decides the
            // borrow: a foreground route on a windowless app asks
            // policy for the activation (a visible side effect, never
            // unauthenticated), re-observes, then re-floors and
            // re-authorizes itself on the world the wake produced. A
            // background route — a menu AXPress — never steals focus.
            if wake.is_none() && stage_needed && route.requires_foreground {
                // A stage denied earlier this step denies every
                // remaining foreground route identically — same
                // fingerprint, same verdict, no duplicate journal.
                if let Some(reason) = &stage_refusal {
                    last_refusal = format!("stage borrow refused: {reason}");
                    continue;
                }
                if !stage_answered {
                    if let Some(sel) = &app {
                        match self.authorize_stage(sel, &ctx, cfg, scope) {
                            StageOutcome::Approval {
                                fingerprint,
                                reason,
                                action,
                            } => {
                                return StepStatus::NeedsApproval {
                                    fingerprint,
                                    reason,
                                    action,
                                };
                            }
                            StageOutcome::Refused { reason } => {
                                stage_answered = true;
                                stage_refusal = Some(reason.clone());
                                last_refusal = format!("stage borrow refused: {reason}");
                                continue;
                            }
                            StageOutcome::Errored(error) => {
                                return StepStatus::Errored { error };
                            }
                            StageOutcome::Clear => {
                                stage_answered = true;
                            }
                            StageOutcome::Activated { handle, obs: new } => {
                                stage_answered = true;
                                *wake = Some(handle);
                                let cur = new.as_ref().or(obs);
                                // The world moved — re-resolve descriptors
                                // and floors for every remaining route…
                                for r in routes.iter_mut() {
                                    enrich_descriptor(&mut r.target, cur);
                                    enforce_sensitivity_floor(r, cur);
                                }
                                route = routes[index].clone();
                                // …then re-authorize this route: the
                                // fingerprint the first verdict bound was
                                // computed on a windowless world.
                                fp = dexter_policy::fingerprint_route(&route, &ctx);
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
                                        "stage": "post_wake",
                                        "target_bounds": bounds,
                                    }),
                                );
                                match self.authorize_route(&route, &fp, &ctx, cfg) {
                                    RouteVerdict::Allow => {}
                                    RouteVerdict::Denied(reason) => {
                                        return StepStatus::Denied { reason };
                                    }
                                    RouteVerdict::Approval {
                                        fingerprint,
                                        reason,
                                    } => {
                                        return StepStatus::NeedsApproval {
                                            fingerprint,
                                            reason,
                                            action: audit::action_summary(&route.action),
                                        };
                                    }
                                }
                                // Re-derive the expectation on the world the
                                // act executes in — the pre-wake observation
                                // saw no window content.
                                if expect.is_none() {
                                    expect = cur.and_then(|o| {
                                        derive_expect(&step.action, o, cfg.window_scope.is_some())
                                    });
                                }
                            }
                        }
                    }
                }
            }

            let result = match self.driver.execute(&route, &act_ctx) {
                Ok(r) => r,
                Err(e) => {
                    self.journal(
                        EventKind::ActionFailed,
                        serde_json::json!({"error": e.to_string(), "route": index}),
                    );
                    // A delivery error can postdate the side effect —
                    // a timed-out reply after the text was typed, a
                    // dropped IPC after the click landed. With an
                    // expectation to check, verify before declaring the
                    // act dead (SDD recovery-v2); without one, the
                    // error is the only verdict there is.
                    if expect.is_some() {
                        exec_error = Some(e);
                        break;
                    }
                    return StepStatus::Errored { error: e };
                }
            };
            // Clipboard content rides `detail` to the caller — it never
            // reaches the journal, in any trace mode.
            let detail: &str = if matches!(
                route.action,
                Action::ReadClipboardText | Action::WriteClipboardText { .. }
            ) {
                "<redacted>"
            } else {
                result.detail.as_deref().unwrap_or("")
            };
            self.journal(
                EventKind::ActionExecuted,
                serde_json::json!({
                    "status": format!("{:?}", result.status),
                    "mechanism": format!("{:?}", result.mechanism),
                    "detail": detail,
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
            // Execute errored with an expectation in hand: run the
            // verify-poll before reporting — the side effect may have
            // landed despite the broken delivery report.
            if let (Some(e), Some(expected)) = (exec_error, expect.as_ref()) {
                return match self.verify_poll(expected, step, cfg, &app, cancel) {
                    Ok(p) if p.verification.status == VerificationStatus::Verified => {
                        StepStatus::Done {
                            result: None,
                            verification: Some(p.verification),
                            attempts: p.attempts,
                        }
                    }
                    _ => StepStatus::Errored { error: e },
                };
            }
            return StepStatus::Failed {
                reason: format!("every planned route refused — last: {last_refusal}"),
                attempts: routes.len() as u32,
            };
        };

        let Some(expected) = &expect else {
            return StepStatus::Done {
                result: Some(result),
                verification: None,
                attempts: 1,
            };
        };

        match self.verify_poll(expected, step, cfg, &app, cancel) {
            Ok(p) if p.verification.status == VerificationStatus::Verified => StepStatus::Done {
                result: Some(result),
                verification: Some(p.verification),
                attempts: p.attempts,
            },
            Ok(p) => StepStatus::Failed {
                reason: format!(
                    "verification never reached VERIFIED — last: {:?}: {}",
                    p.verification.status,
                    p.verification.checks.join(" | ")
                ),
                attempts: p.attempts,
            },
            Err(e) => StepStatus::Errored { error: e },
        }
    }

    /// VERIFY-POLL — the world may need time to reach the expected
    /// state; re-observe, never re-execute. `max_attempts` bounds the
    /// polls after one execute (polling, not action replay).
    /// The first poll runs immediately — most effects land
    /// synchronously (AX updates are not async); the settle delay only
    /// pays between failed polls. Returns the last verdict with its
    /// attempt count; `Err` when re-observation itself failed — no
    /// verdict exists then.
    fn verify_poll(
        &mut self,
        expected: &ExpectedState,
        step: &Step,
        cfg: &RunConfig,
        app: &Option<AppSelector>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<PollOutcome, DriverError> {
        let verify_attempts = step.max_attempts.unwrap_or(cfg.max_attempts).max(1);
        let mut last: Option<Verification> = None;
        for attempt in 1..=verify_attempts {
            // Polls honor the cancel token between attempts — a long
            // budget must not spin after the caller said stop. The
            // act already landed, so the verdict stays whatever the
            // last completed check saw (honestly unverified).
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                self.journal(
                    EventKind::TaskCancelled,
                    serde_json::json!({"stage": "verify_poll", "attempt": attempt}),
                );
                return Ok(PollOutcome {
                    verification: last.unwrap_or_else(|| {
                        Verification::failed(vec!["cancelled before the first poll".into()])
                    }),
                    attempts: attempt.saturating_sub(1),
                });
            }
            if attempt > 1 {
                self.journal(
                    EventKind::RecoveryStarted,
                    serde_json::json!({
                        "strategy": "verify_poll",
                        "trigger": "unverified",
                        "attempt": attempt,
                    }),
                );
                std::thread::sleep(cfg.verify_delay);
            }
            let scope = ObservationScope {
                app: app.clone(),
                max_elements: cfg.observe_max_elements,
                window: cfg.window_scope,
                // Element- and text-referencing expectations can point at
                // menu items; signature-level ones cannot (menus are
                // excluded from the signature), so the re-observe skips
                // the menu-bar walk for those — its dominant cost.
                include_menu: expected_needs_menu(expected),
                ..Default::default()
            };
            let observe_start = std::time::Instant::now();
            let obs = match self.observe_scoped(&scope) {
                Ok(o) => o,
                Err(e) => {
                    self.journal(
                        EventKind::ActionFailed,
                        serde_json::json!({"error": format!("verify observe: {e}"), "attempt": attempt}),
                    );
                    return Err(e);
                }
            };
            let observe_ms = observe_start.elapsed().as_millis() as u64;
            self.journal(
                EventKind::ObservationCreated,
                serde_json::json!({"observation": obs.id.0, "elements": obs.elements.len(), "attempt": attempt, "observe_ms": observe_ms}),
            );
            let verify_start = std::time::Instant::now();
            let verification = dexter_verify::verify(&obs, expected);
            let verify_ms = verify_start.elapsed().as_millis() as u64;
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
                    "verify_ms": verify_ms,
                }),
            );
            if verification.status == VerificationStatus::Verified {
                return Ok(PollOutcome {
                    verification,
                    attempts: attempt,
                });
            }
            last = Some(verification);
        }

        Ok(PollOutcome {
            verification: last
                .unwrap_or_else(|| Verification::failed(vec!["no verification ran".into()])),
            attempts: verify_attempts,
        })
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
            window: cfg.run.window_scope,
            ..Default::default()
        };
        let obs = match self.observe_scoped(&scope) {
            Ok(o) => Some(o),
            Err(e) => {
                self.journal(
                    EventKind::ObservationFailed,
                    serde_json::json!({"error": e.to_string(), "context": "goal_start"}),
                );
                None
            }
        };
        // No upfront stage borrow: the route loop inside each step
        // asks for activation only when the route about to execute
        // declares `requires_foreground` — a menu AXPress never steals
        // focus. The shared slot means the first borrow covers the
        // rest of the task and is restored once, here, so the whole
        // goal — verify-polls included — runs on the woken world.
        let mut wake: Option<WakeHandle> = None;
        // The opening observation doubles as step 1's — observing is
        // not free (drivers tick, AX walks cost), so the loop must not
        // pay for a second one.
        let outcome = self.run_goal_loop(
            goal,
            done,
            generator,
            decider,
            cfg,
            task_started,
            obs,
            &mut wake,
        );
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
        wake: &mut Option<WakeHandle>,
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
            window: cfg.run.window_scope,
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
            let observe_start = std::time::Instant::now();
            let obs = match initial_obs
                .take()
                .map(Ok)
                .unwrap_or_else(|| self.observe_scoped(&scope))
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
            let observe_ms = observe_start.elapsed().as_millis() as u64;
            self.journal(
                EventKind::ObservationCreated,
                serde_json::json!({"observation": obs.id.0, "elements": obs.elements.len(), "step": step, "observe_ms": observe_ms}),
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
                        "context": audit::training_context(&ctx),
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
                        TraceMode::Training => {
                            serde_json::to_value(audit::training_decision(&decision))
                                .unwrap_or_default()
                        }
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
                    // Record the attempt by semantic identity, not the
                    // ephemeral element token: ids are per-observation,
                    // so a token stops meaning anything the moment the
                    // loop re-observes. `already_tried` compares
                    // role+name — the same element suppresses on any
                    // later world, a different element at a recycled id
                    // never does.
                    hist.attempts.push(normalize_attempt(&action, &obs));
                    // Every mutating act earns a derived expectation —
                    // progress is judged by done_when AND by evidence the
                    // act itself landed. Acts the model can't express an
                    // outcome for stay unverified, honestly.
                    let expect = derive_expect(&action, &obs, cfg.run.window_scope.is_some());
                    let status = self.run_step_inner(
                        &Step {
                            note: Some(rationale),
                            action: *action,
                            expect,
                            max_attempts: Some(3),
                            app: cfg.run.app.clone(),
                        },
                        &cfg.run,
                        Some(&obs),
                        &scope,
                        wake,
                        cfg.cancel.as_deref(),
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
                            action,
                        } => {
                            // Policy pause is an outcome, not an error —
                            // the caller grants and retries; spinning
                            // here would burn the step budget denied.
                            self.journal(
                                EventKind::TaskFailed,
                                serde_json::json!({"outcome": "needs_approval", "fingerprint": &fingerprint, "reason": &reason, "action": &action, "step": step}),
                            );
                            return TaskOutcome::NeedsApproval {
                                fingerprint,
                                reason,
                                action,
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
            Action::Click {
                target,
                button,
                count,
            } => {
                serde_json::json!({"type": "click", "target": target, "button": button, "count": count})
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
            Action::Navigate { url } => serde_json::json!({
                "type": "navigate",
                // Origin+path stay operator-legible; the query/fragment
                // — where signed tokens live — is stripped like a
                // payload, and the digest keeps audit correlation to
                // the exact URL the fingerprint binds.
                "url": dexter_core::redact_url(url),
                "url_sha256": dexter_policy::payload_digest(url),
            }),
            Action::Invoke { target, action } => {
                serde_json::json!({"type": "invoke", "target": target, "action": action})
            }
            Action::LaunchApp { app, activate } => {
                serde_json::json!({"type": "launch_app", "app": app, "activate": activate})
            }
            Action::QuitApp { app } => serde_json::json!({"type": "quit_app", "app": app}),
            Action::Window {
                window_id,
                operation,
            } => serde_json::json!({
                "type": "window",
                "window_id": window_id,
                "operation": operation,
            }),
            Action::ReadClipboardText => serde_json::json!({"type": "clipboard_read"}),
            // The payload is a secret in flight — digested, never plain.
            Action::WriteClipboardText { text } => serde_json::json!({
                "type": "clipboard_write",
                "payload": payload("text", text),
            }),
            Action::Drag {
                from,
                to,
                duration_ms,
            } => serde_json::json!({
                "type": "drag",
                "from": from,
                "to": to,
                "duration_ms": duration_ms,
            }),
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

    /// Replace a payload string with its digest token — the action
    /// keeps its serde shape so the context still deserializes and
    /// replays, while the secret never lands on disk. Identical
    /// payloads produce identical tokens, so training rows keep their
    /// equality signal.
    pub fn scrub_action_payload(action: &mut Action) {
        let token = |text: &mut String| {
            *text = format!(
                "[redacted len={} sha256={}]",
                text.len(),
                dexter_policy::payload_digest(text)
            );
        };
        match action {
            Action::TypeText { text, .. } => token(text),
            Action::SetValue { value, .. } => token(value),
            Action::WriteClipboardText { text } => token(text),
            // Not a free-text payload, but query strings carry the
            // same token class — keep origin+path (a useful training
            // signal), strip the rest like the journal does.
            Action::Navigate { url } => *url = dexter_core::redact_url(url),
            _ => {}
        }
    }

    /// A `DecisionContext` with candidate payloads scrubbed — the
    /// Training journal keeps the full replayable structure (the eval
    /// harness re-feeds it to a `DecisionEngine`) but typed/set/
    /// clipboard secrets never leave the process.
    pub fn training_context(
        ctx: &dexter_decision::DecisionContext,
    ) -> dexter_decision::DecisionContext {
        let mut ctx = ctx.clone();
        for c in &mut ctx.candidates {
            scrub_action_payload(&mut c.action);
        }
        ctx
    }

    /// A `Decision` with its action payload scrubbed — same serde
    /// shape, so `rows_from_events` still reads `candidate_index`.
    pub fn training_decision(decision: &dexter_decision::Decision) -> dexter_decision::Decision {
        let mut d = decision.clone();
        if let dexter_decision::Decision::Act { action, .. } = &mut d {
            scrub_action_payload(action);
        }
        d
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
        | Action::Invoke { target, .. }
        | Action::SetValue { target, .. } => Some(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        // Presence lands where the press does — the drag's source.
        Action::Drag { from, .. } => Some(from),
        _ => None,
    };
    matches!(target, Some(t) if !matches!(t, Target::Point { .. }))
}

fn target_bounds(action: &Action, obs: Option<&Observation>) -> Option<Rect> {
    let target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::Invoke { target, .. }
        | Action::SetValue { target, .. } => Some(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        // The overlay draws where the press lands — the drag's source.
        Action::Drag { from, .. } => Some(from),
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
    /// `action` is the redacted summary the operator approves.
    NeedsApproval {
        fingerprint: String,
        reason: String,
        action: serde_json::Value,
    },
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
    let Some(obs) = obs else { return };
    // A focus-bound descriptor resolves to the focused element —
    // `rule.target` matchers and the grant fingerprint see its
    // identity, not a bare `focused` flag.
    if desc.focused {
        if let Some(el) = obs.elements.iter().find(|e| e.focused) {
            if desc.role.is_none() {
                desc.role = el.role.clone();
            }
            if desc.subrole.is_none() {
                desc.subrole = el.subrole.clone();
            }
            if desc.name.is_none() {
                desc.name = el.name.clone();
            }
            if desc.identifier.is_none() {
                desc.identifier = el.identifier.clone();
            }
        }
        return;
    }
    let (Some(el_id), Some(obs_id)) = (desc.element, desc.observation) else {
        return;
    };
    if obs.id != obs_id {
        return;
    }
    if let Some(el) = obs.elements.iter().find(|e| e.id == el_id) {
        if desc.role.is_none() {
            desc.role = el.role.clone();
        }
        if desc.subrole.is_none() {
            desc.subrole = el.subrole.clone();
        }
        if desc.name.is_none() {
            desc.name = el.name.clone();
        }
        if desc.identifier.is_none() {
            desc.identifier = el.identifier.clone();
        }
    }
}

/// The semantic target an action carries, if any — extracted from the
/// route's concrete action so the floor can resolve it against the
/// pre-act observation (a `name`-only query gives the descriptor no
/// role to check).
fn semantic_target_of(action: &Action) -> Option<&SemanticTarget> {
    let target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. }
        | Action::Invoke { target, .. } => Some(target),
        Action::Drag { from, .. } => Some(from),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        _ => None,
    }?;
    match target {
        Target::Semantic(st) => Some(st),
        _ => None,
    }
}

/// The secrets floor is driver-agnostic: an element-bound route whose
/// resolved element (or declared role/subrole) is a secure/password
/// field upgrades to `Secrets` here — a standard-scope grant can
/// never silently cover a sensitive target, whatever the driver
/// declared. Focus-bound routes (`type_text` with no target, `key`,
/// explicit `Target::Focused`) resolve the focused element: typing a
/// secret into a focused password field gets the floor on every
/// driver. Element tokens minted under a foreign observation resolve
/// through the driver's own obs cache — the descriptor enrichment
/// carries role *and* subrole, so a `text_field`/`password` is still
/// caught. Semantic targets resolve here against the pre-act
/// observation — the same world the act will resolve them in.
fn enforce_sensitivity_floor(route: &mut ExecutionRoute, obs: Option<&Observation>) {
    let desc = &route.target;
    let sensitive = desc
        .role
        .as_deref()
        .is_some_and(dexter_core::is_sensitive_role)
        || desc
            .subrole
            .as_deref()
            .is_some_and(dexter_core::is_sensitive_role)
        || desc
            .element
            .zip(desc.observation)
            .and_then(|(el_id, obs_id)| {
                obs.filter(|o| o.id == obs_id)
                    .and_then(|o| o.elements.iter().find(|e| e.id == el_id))
            })
            .is_some_and(|e| e.is_sensitive())
        || (desc.focused
            && obs
                .and_then(|o| o.elements.iter().find(|e| e.focused))
                .is_some_and(|e| e.is_sensitive()))
        // A semantic target resolves only at act time — but the
        // pre-act observation is the same world it will resolve in.
        // Any sensitive candidate gets the floor: the act could land
        // on it, so fail closed. `is_sensitive()` checks role, subrole
        // and raw role — a `text_field`/`password` needs no help.
        || semantic_target_of(&route.action)
            .is_some_and(|st| {
                obs.is_some_and(|o| {
                    dexter_world_model::find_elements(o, st)
                        .iter()
                        .any(|e| e.is_sensitive())
                })
            });
    if sensitive {
        route.sensitivity = Sensitivity::Secrets;
    }
    // Destructiveness is action-shaped, not target-shaped: a driver
    // that forgets to declare it must not slip past the floor — the
    // same seam-enforcement the secrets floor gets.
    if route.sensitivity == Sensitivity::Standard
        && matches!(
            route.action,
            Action::QuitApp { .. }
                | Action::Window {
                    operation: dexter_core::WindowOperation::Close,
                    ..
                }
        )
    {
        route.sensitivity = Sensitivity::Destructive;
    }
    // Clipboard is action-shaped sensitivity too — the v1
    // `Policy::evaluate` mapping declares it; the floor must match so
    // a driver whose plan is empty (legacy route, `Standard`) can't
    // slip a clipboard read/write past the secrets gate.
    if route.sensitivity == Sensitivity::Standard
        && matches!(
            route.action,
            Action::ReadClipboardText | Action::WriteClipboardText { .. }
        )
    {
        route.sensitivity = Sensitivity::Secrets;
    }
}

/// The label an attempted action resolved to on this observation —
/// element targets carry no name, so the engine resolves it while the
/// world that produced the candidate is still at hand.
fn attempt_label(action: &Action, obs: &Observation) -> Option<String> {
    let target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. }
        | Action::Invoke { target, .. } => Some(target),
        Action::Drag { from, .. } => Some(from),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_ref(),
        _ => None,
    }?;
    dexter_world_model::resolve_element(obs, target)
        .ok()
        .and_then(|e| e.label().map(str::to_string))
}

/// Actions whose effect is derivable — the subset of mutating actions
/// `derive_expect` can express. Used to decide when a pre-act
/// observation is worth taking. A coordinate click derives nothing —
/// don't pay an observation for a check that will be `None`.
fn derives(action: &Action) -> bool {
    match action {
        Action::Click { target, .. } => !matches!(target, Target::Point { .. }),
        _ => matches!(
            action,
            Action::SetValue { .. }
                | Action::TypeText { .. }
                | Action::Focus { .. }
                | Action::Invoke { .. }
                | Action::Drag { .. }
                | Action::LaunchApp { .. }
                | Action::QuitApp { .. }
        ),
    }
}

/// The observation shows no window content — the precondition that
/// makes a stage borrow worth asking for at all. An absent
/// observation is *not* windowless evidence: no observation, no wake.
fn windowless(obs: Option<&Observation>) -> bool {
    obs.is_some_and(|o| {
        !o.elements
            .iter()
            .any(|e| e.role.as_deref() == Some("window"))
    })
}

/// The route a stage borrow asks policy for — launch-or-activate on
/// the target app (`Api` mechanism, `Visual` tier). Public so callers
/// that pre-compute grants (scenario harnesses, operators seeding an
/// `ApprovalStore`) can reproduce the exact fingerprint the engine
/// will request.
pub fn stage_route(app: &AppSelector) -> ExecutionRoute {
    let action = Action::LaunchApp {
        app: app.clone(),
        activate: true,
    };
    ExecutionRoute {
        target: TargetDescriptor::from_action(&action),
        action,
        mechanism: Some(dexter_core::Mechanism::Api),
        intrusiveness: Intrusiveness::Visual,
        sensitivity: Sensitivity::Standard,
        requires_foreground: false,
    }
}

/// What `authorize_stage` concluded — the wake is a side effect with
/// its own policy verdict, not a silent precondition.
enum StageOutcome {
    /// No activation happened — the app was already on stage, or the
    /// wake was a no-op.
    Clear,
    /// The app was activated; `obs` is the world the wake produced
    /// (`None` when the post-wake observe failed).
    Activated {
        handle: WakeHandle,
        obs: Option<Observation>,
    },
    /// Policy denied the activation — a route needing the stage
    /// cannot honestly execute.
    Refused { reason: String },
    /// The borrow needs an approval — surface the fingerprint; the
    /// caller stops rather than activating unauthenticated.
    Approval {
        fingerprint: String,
        reason: String,
        action: serde_json::Value,
    },
    /// The wake call itself failed.
    Errored(DriverError),
}

/// One route's policy verdict — extracted so a post-wake world can be
/// re-authorized with the same journaling.
enum RouteVerdict {
    Allow,
    Denied(String),
    Approval { fingerprint: String, reason: String },
}

/// Whether the action needs the app on stage (window-layer element
/// resolution or foreground input) — the honest gate for the stage
/// borrow.
/// Lifecycle, waits, clipboard, navigation and window ops never do:
/// waking for them would steal focus for no reason. A point click
/// doesn't either — the coordinate is the target.
fn needs_stage(action: &Action) -> bool {
    match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. } => !matches!(target, Target::Point { .. }),
        Action::Invoke { .. } | Action::Drag { .. } | Action::Key { .. } => true,
        Action::TypeText { .. } => true,
        Action::Scroll { target, .. } => target.is_some(),
        _ => false,
    }
}

/// Does this action mutate the world (as opposed to observing or
/// positioning)? Auto-completion only credits mutating acts.
fn is_mutating(action: &Action) -> bool {
    matches!(
        action,
        Action::Click { .. }
            | Action::Focus { .. }
            | Action::SetValue { .. }
            | Action::TypeText { .. }
            | Action::Key { .. }
            | Action::Navigate { .. }
            | Action::Invoke { .. }
            | Action::LaunchApp { .. }
            | Action::QuitApp { .. }
            | Action::Window { .. }
            | Action::WriteClipboardText { .. }
            | Action::Drag { .. }
    )
}

/// Whether the target resolves to a sensitive (secure/password)
/// element — the same definition the collectors redact values by, so
/// a value-based expectation can never be honest for it.
///
/// An unresolvable target (e.g. a foreign-observation element token)
/// reads "not sensitive" here, which is safe only because
/// [`semantic_for`] returns `None` for the same token — no value
/// expectation can be derived either way, so the act stays on the
/// unverified path rather than failing on a redacted value.
fn target_is_sensitive(target: &Target, obs: &Observation) -> bool {
    dexter_world_model::resolve_element(obs, target)
        .ok()
        .is_some_and(|e| e.is_sensitive())
}

/// Rewrite an action's element targets to their semantic identity while
/// the observation that minted the tokens is still at hand — what
/// `GenHistory::attempts` stores, since element ids are per-observation
/// and mean nothing once the loop re-observes. Unresolvable (foreign)
/// tokens pass through untouched.
fn normalize_attempt(action: &Action, obs: &Observation) -> Action {
    let mut a = action.clone();
    let rewrite = |t: &mut Target| {
        if matches!(t, Target::Element { .. } | Target::Focused) {
            if let Some(st) = semantic_for(t, obs) {
                *t = Target::Semantic(st);
            }
        }
    };
    match &mut a {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. }
        | Action::Invoke { target, .. } => rewrite(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => {
            if let Some(t) = target {
                rewrite(t);
            }
        }
        Action::Drag { from, to, .. } => {
            rewrite(from);
            rewrite(to);
        }
        _ => {}
    }
    a
}

/// The semantic identity an act's target refers to — for deriving a
/// checkable expectation. `Element`/`Focused` resolve through the
/// observation to role+name+identifier; `Semantic` passes through.
/// `Point`/`Window` have no semantic identity → `None`.
fn semantic_for(target: &Target, obs: &Observation) -> Option<SemanticTarget> {
    match target {
        Target::Semantic(s) => Some(s.clone()),
        // An element token is bound to the observation that minted it —
        // `resolve_element` enforces that qualifier, and a bare id
        // lookup would bind whichever element now sits at that
        // position. A foreign token is unresolvable here → `None` →
        // the act runs unverified rather than verified against the
        // wrong element (or false-failed on a secure field's redacted
        // value — which a retry would append a second time). `Focused`
        // resolves through the same path (the focused element of this
        // observation).
        Target::Element { .. } | Target::Focused => {
            let el = dexter_world_model::resolve_element(obs, target).ok()?;
            let mut st = SemanticTarget {
                role: el.role.clone(),
                name: el.name.clone(),
                identifier: el.identifier.clone(),
                ..Default::default()
            };
            // Duplicates share role+name — pin the element's tree-order
            // ordinal so the identity still discriminates (a recorded
            // attempt on "Guardar"[preview] must not suppress
            // "Guardar"[main]). Single matches keep `None`: exact
            // semantics, fail-closed.
            let matches = dexter_world_model::find_elements(obs, &st);
            if matches.len() > 1 {
                st.index = matches.iter().position(|e| e.id == el.id);
            }
            Some(st)
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
fn derive_expect(action: &Action, obs: &Observation, window_scoped: bool) -> Option<ExpectedState> {
    // A target is honestly unverifiable in this observation when its
    // effect cannot reach the observed world. Menu elements are
    // signature-excluded, so a press that mutates only menu state (a
    // checkmark toggle, a silent command) leaves the signature
    // identical — `WorldChanged` would false-fail the success and, in
    // a goal loop, invite a retry that re-mutates. And under a pinned
    // window scope anything resolving *outside* the scope cannot be
    // verified in it — menu items concretely: they are boundless, so
    // a scoped observation can never contain one.
    let unverifiable_target = |t: &Target| -> bool {
        match dexter_world_model::resolve_element(obs, t) {
            Ok(el) => dexter_world_model::is_menu_element(el),
            Err(_) => window_scoped,
        }
    };
    match action {
        Action::Click { target, .. } => match target {
            Target::Point { .. } => None,
            t if unverifiable_target(t) => None,
            _ => Some(ExpectedState::WorldChanged {
                from: dexter_world_model::signature(obs),
            }),
        },
        Action::TypeText { text, target } => {
            let t = target.clone().unwrap_or(Target::Focused);
            // A secure field's value is redacted at collection — an
            // ElementValue check could only fail, then a retry would
            // append the secret a second time. Honestly unverifiable.
            if target_is_sensitive(&t, obs) {
                return None;
            }
            semantic_for(&t, obs).map(|st| ExpectedState::ElementValue {
                target: st,
                predicate: ValuePredicate::Contains(text.clone()),
            })
        }
        Action::SetValue { target, value } => {
            if target_is_sensitive(target, obs) {
                return None;
            }
            semantic_for(target, obs).map(|st| ExpectedState::ElementValue {
                target: st,
                predicate: ValuePredicate::Equals(value.clone()),
            })
        }
        Action::Focus { target } => {
            if unverifiable_target(target) {
                return None;
            }
            semantic_for(target, obs).map(|st| ExpectedState::FocusedElement { target: st })
        }
        // Invoke/Drag: effect unpredictable — the world must change.
        Action::Invoke { target, .. } if unverifiable_target(target) => None,
        Action::Drag { from, .. } if unverifiable_target(from) => None,
        Action::Invoke { .. } | Action::Drag { .. } => Some(ExpectedState::WorldChanged {
            from: dexter_world_model::signature(obs),
        }),
        // Window-set changes can't be observed under a pinned window:
        // verification sees only the pinned window's title set and
        // subtree, so `AppRunning` and `WorldChanged` alike poll for
        // what the scope filters out. Honestly unverifiable — a quit
        // of the pinned app itself surfaces as an honest observe
        // error: the window is gone.
        Action::LaunchApp { .. } | Action::QuitApp { .. } if window_scoped => None,
        // AppRunning checks window *names* — a bundle/pid selector can't
        // match them, so non-name launches verify by the signature
        // (a new window changes the title set; a quit removes one).
        Action::LaunchApp { app, .. } => match app {
            // "Launched" = a window owned by the requested name OR the
            // world changed at all — `open -a Calculator` may surface
            // the localized name ("Calculadora"), so a pure name match
            // would false-fail a real launch.
            AppSelector::Name(name) => Some(ExpectedState::Any {
                any: vec![
                    ExpectedState::AppRunning { name: name.clone() },
                    ExpectedState::WorldChanged {
                        from: dexter_world_model::signature(obs),
                    },
                ],
            }),
            _ => Some(ExpectedState::WorldChanged {
                from: dexter_world_model::signature(obs),
            }),
        },
        Action::QuitApp { app } => match app {
            // "Quit" = its windows are gone OR the world changed (a
            // window closed). Same localized-name gap as launch.
            AppSelector::Name(name) => Some(ExpectedState::Any {
                any: vec![
                    ExpectedState::Not {
                        not: Box::new(ExpectedState::AppRunning { name: name.clone() }),
                    },
                    ExpectedState::WorldChanged {
                        from: dexter_world_model::signature(obs),
                    },
                ],
            }),
            _ => Some(ExpectedState::WorldChanged {
                from: dexter_world_model::signature(obs),
            }),
        },
        // Window ops / clipboard / read: the world model can't express
        // their effect — honestly unverified.
        _ => None,
    }
}

/// Whether a verification re-observe needs the menu-bar subtree walked.
/// Element- and text-referencing expectations can point at menu items;
/// world-level ones (`WorldChanged`, `AppRunning`, `WindowTitleContains`)
/// cannot — the signature excludes menu elements — so those polls skip
/// the walk, which dominates observe cost on real apps.
fn expected_needs_menu(expected: &ExpectedState) -> bool {
    use ExpectedState as E;
    match expected {
        E::ElementExists { .. }
        | E::ElementAbsent { .. }
        | E::ElementValue { .. }
        | E::FocusedElement { .. }
        | E::TextPresent { .. } => true,
        E::WorldChanged { .. } | E::AppRunning { .. } | E::WindowTitleContains { .. } => false,
        E::All { all } => all.iter().any(expected_needs_menu),
        E::Any { any } => any.iter().any(expected_needs_menu),
        E::Not { not } => expected_needs_menu(not),
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
