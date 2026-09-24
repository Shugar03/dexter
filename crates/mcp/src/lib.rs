//! Dexter MCP server — exposes the runtime to MCP agents over stdio.
//!
//! Every tool call goes through `Engine` — the same policy/act/verify
//! path as the CLI. The server is a persistent process, so approval
//! grants are session-scoped for real: `dexter_act` returning
//! `needs_approval` gives the agent a fingerprint a human grants via
//! `dexter_grant`, and the retry then runs.

use dexter_core::{Action, AppSelector, ExpectedState, ObservationScope};
use dexter_decision::{CandidateGenerator, DecisionEngine, HeuristicGenerator};
use dexter_driver::ComputerDriver;
use dexter_engine::{Engine, RunConfig, Step, StepStatus, TaskConfig, TaskOutcome};
use dexter_policy::Policy;
use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, Json, ServerHandler};
use schemars::JsonSchema;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Shared runtime behind the MCP surface.
pub struct DexterRuntime {
    engine: Mutex<Engine<Box<dyn ComputerDriver>>>,
    generator: HeuristicGenerator,
    /// Decider for `dexter_task` — RuleBased unless the host configured
    /// another engine (e.g. laya) at server start.
    decider: Box<dyn DecisionEngine>,
    /// Operator-set trust level — applies to every tool call.
    config: ServerConfig,
    /// Shared journal — `dexter_journal` reads it without taking the
    /// engine lock, so audit stays live while a task runs.
    journal: Arc<Mutex<dexter_engine::Journal>>,
    /// Cooperative-cancel token for the in-flight `dexter_task`.
    task_cancel: Mutex<Option<std::sync::Arc<std::sync::atomic::AtomicBool>>>,
    /// Driver capabilities, snapshotted at construction — `dexter_status`
    /// reads them without the engine lock so the probe stays live
    /// while a task runs.
    caps: dexter_driver::DriverCapabilities,
    /// Session accounting — what this session cost the host agent in
    /// round-trips and returned payload volume. Counted at the response
    /// boundary, so the numbers describe work actually delivered.
    metrics: Mutex<SessionMetrics>,
}

/// Per-session agent-cost accounting — the measurable half of Dexter's
/// thesis ("the runtime absorbs the loop so the model doesn't pay for
/// it"). Every `dexter_*` call is one agent round-trip; every byte
/// returned is context the model ingests.
#[derive(Debug, Default)]
pub struct SessionMetrics {
    /// Successful tool responses, by tool name.
    pub calls: std::collections::BTreeMap<String, u64>,
    /// Serialized JSON bytes returned across all responses.
    pub response_bytes: u64,
    /// Steps executed inside `dexter_task` — each is a full
    /// observe/decide/act/verify cycle that cost the agent zero calls.
    /// Comparing this against `calls` totals is the avoided-cost number.
    pub task_internal_steps: u64,
}

impl SessionMetrics {
    /// Crude token estimate (bytes/4) — order of magnitude, not billing.
    pub fn est_response_tokens(&self) -> u64 {
        self.response_bytes / 4
    }
}

impl DexterRuntime {
    pub fn new(policy: Policy, driver: Box<dyn ComputerDriver>, config: ServerConfig) -> Self {
        let mut engine = Engine::new(driver, policy, Duration::from_secs(300));
        // Operator opted into coordinates: open the physical tier unless
        // the policy file explicitly denies it.
        if config.allow_coords {
            engine.permit_physical();
        }
        let journal = engine.journal_handle();
        let caps = engine.driver().capabilities();
        Self {
            engine: Mutex::new(engine),
            generator: HeuristicGenerator::default(),
            decider: Box::new(dexter_decision::RuleBased::default()),
            config,
            journal,
            task_cancel: Mutex::new(None),
            caps,
            metrics: Mutex::new(SessionMetrics::default()),
        }
    }

    /// Swap the decider `dexter_task` uses (spawned once, kept warm).
    pub fn with_decider(mut self, d: Box<dyn DecisionEngine>) -> Self {
        self.decider = d;
        self
    }
}

/// Serialized byte length without materializing the JSON — the
/// payload-bound checks only need the count.
fn json_len(v: &serde_json::Value) -> Result<u64, McpError> {
    struct Count(u64);
    impl std::io::Write for Count {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0 += b.len() as u64;
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = Count(0);
    serde_json::to_writer(&mut w, v).map_err(|e| err(format!("serialize: {e}")))?;
    Ok(w.0)
}

fn run_cfg(app: Option<String>, cfg: ServerConfig) -> RunConfig {
    RunConfig {
        app: app.as_deref().map(AppSelector::parse),
        max_attempts: 3,
        verify_delay: Duration::from_millis(250),
        post_act_settle: Duration::ZERO,
        allow_coordinates: cfg.allow_coords,
        approve_all: cfg.approve_all,
        observe_max_elements: 4_000,
        window_scope: None,
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ObserveParams {
    /// Scope to an app: name, `com.bundle.id` or pid.
    pub app: Option<String>,
    /// Cap on flattened elements.
    pub max_elements: Option<usize>,
    /// Narrow to one window id (from a previous observe's `windows`).
    /// Elements are filtered by bounds intersection; menubar-style
    /// unpositioned elements don't belong to a window and are dropped.
    pub window: Option<u32>,
    /// Opt-in OCR fallback. When the accessibility tree is limited/empty
    /// (or `window` is set), text in the window capture is recognized
    /// on-device and appended as inert `source: "ocr"` elements — marked
    /// `[ocr]` in the digest. They have no live handle: acting on them
    /// means targeting their bounds center as a point, which stays
    /// approval/policy-gated.
    pub vision: Option<bool>,
    /// Walk the app's menu-bar subtree (default true). Menu items often
    /// outnumber window elements ~10:1 — pass `false` when you only need
    /// window controls. Menu chords (`key` actions) resolve the menu
    /// live and are unaffected.
    pub include_menu: Option<bool>,
}

/// Server-level trust configuration — set by the operator at startup,
/// never by the agent per call. This is what keeps `needs_approval`
/// meaningful: the model can't raise its own privileges mid-session.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerConfig {
    /// Treat every RequireApproval as granted up front.
    pub approve_all: bool,
    /// Permit physical-tier (coordinate/keyboard) input.
    pub allow_coords: bool,
    /// Show the presence overlay while tools act — the operator sees
    /// the cursor fly on every dexter_act/dexter_task.
    pub presence: bool,
    /// Remove `dexter_grant` — the same channel that returns a
    /// needs_approval fingerprint can otherwise grant it back, so an
    /// autonomous agent could serve its own human-in-the-loop hook.
    /// Set this when approvals must come from outside the agent's
    /// reach (e.g. a separate operator channel).
    pub no_grants: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ActParams {
    /// The action as core JSON: {"type":"click","target":{...},...}
    /// (core Action enum — see docs/agent-computer-runtime.md §actions)
    pub action: serde_json::Value,
    /// Scope to an app.
    pub app: Option<String>,
    /// Post-condition ExpectedState JSON.
    pub expect: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct GrantParams {
    /// The fingerprint returned by a needs_approval response.
    pub fingerprint: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct VerifyParams {
    /// ExpectedState JSON to check against a fresh observation.
    pub expected: serde_json::Value,
    /// Scope to an app.
    pub app: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct MapParams {
    /// Scope to an app: name, `com.bundle.id` or pid. Required — a map
    /// is per-application, the whole screen is not a meaningful unit.
    pub app: String,
    /// Wake the app once if AX exposes no window content (background
    /// or lazy launch), then hand focus back. Default true; pass false
    /// for a pure read that never disturbs the user.
    pub wake: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CandidatesParams {
    /// The goal, verbatim — candidates are ranked against it.
    pub goal: String,
    /// Scope to an app.
    pub app: Option<String>,
    /// Cap on returned candidates (default 20).
    pub max: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TaskParams {
    /// The goal, verbatim.
    pub goal: String,
    /// ExpectedState JSON marking completion.
    pub done: serde_json::Value,
    /// Scope to an app.
    pub app: Option<String>,
    /// Max decide/act iterations (default 10, hard cap 200).
    pub max_steps: Option<u32>,
    /// Wall-clock budget in seconds (default none, hard cap 3600).
    pub max_secs: Option<u64>,
    /// Pin every observation to one window id (from `dexter_observe`'s
    /// windows) — O(window) per step instead of O(app) on multi-window
    /// apps.
    pub window: Option<u32>,
}

#[derive(Clone)]
pub struct DexterMcp {
    runtime: Arc<DexterRuntime>,
    // Read by the generated ServerHandler impl.
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

fn err(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Stamp the v2 contract version on a tool response object.
fn v2(mut v: serde_json::Value) -> Json<serde_json::Value> {
    v["contract_version"] = 2.into();
    Json(v)
}

#[tool_router]
impl DexterMcp {
    pub fn new(policy: Policy, driver: Box<dyn ComputerDriver>) -> Self {
        Self::with_decider(policy, driver, None, ServerConfig::default())
    }

    /// Same as `new`, plus the decider `dexter_task` should use
    /// (`None` = rule-based) and the operator trust config.
    pub fn with_decider(
        policy: Policy,
        driver: Box<dyn ComputerDriver>,
        decider: Option<Box<dyn DecisionEngine>>,
        config: ServerConfig,
    ) -> Self {
        let mut runtime = DexterRuntime::new(policy, driver, config);
        if let Some(d) = decider {
            runtime = runtime.with_decider(d);
        }
        Self {
            runtime: Arc::new(runtime),
            tool_router: Self::tool_router(),
        }
    }

    /// Count a successful tool response — one agent round-trip plus
    /// its payload volume — then stamp the contract version.
    fn respond(&self, tool: &'static str, v: serde_json::Value) -> Json<serde_json::Value> {
        if let (Ok(mut m), Ok(bytes)) = (self.runtime.metrics.lock(), json_len(&v)) {
            *m.calls.entry(tool.to_string()).or_insert(0) += 1;
            m.response_bytes += bytes;
        }
        v2(v)
    }

    /// Observe the current world: window list + element count + text
    /// digest. This is what a decision layer should read first.
    #[tool(
        name = "dexter_observe",
        description = "Observe the world: windows, elements, text digest. Pass `window` (an id from `windows[]`) to scope to one window."
    )]
    async fn dexter_observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        // Payload bounds — the AX walk cost scales with max_elements.
        let max_elements = params.max_elements.unwrap_or(4_000).min(10_000);
        let window = params.window;
        let scope = ObservationScope {
            app: params.app.as_deref().map(AppSelector::parse),
            max_elements,
            window,
            vision: params.vision.unwrap_or(false),
            // Menu elements are bounds-filtered out of a scoped
            // observation — the menubar walk is pure cost under
            // `window` (the same rule `Engine::observe_scoped`
            // applies), even if the caller asked for it.
            include_menu: window.is_none() && params.include_menu.unwrap_or(true),
            ..Default::default()
        };
        let max_out = max_elements.min(500);
        let runtime = self.runtime.clone();
        let obs = tokio::task::spawn_blocking(move || {
            let obs = runtime
                .engine
                .lock()
                .map_err(err)?
                .driver()
                .observe(&scope)
                .map_err(err)?;
            match window {
                Some(id) => dexter_world_model::scope_to_window(obs, id).map_err(err),
                None => Ok(obs),
            }
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        // Structured element list alongside the digest — agents target
        // `element` ids programmatically instead of parsing text. Capped
        // to keep the payload sane; the digest always stays complete-ish.
        let total_worthy = obs
            .elements
            .iter()
            .filter(|e| dexter_world_model::digest_worthy(e))
            .count();
        let elements: Vec<serde_json::Value> = obs
            .elements
            .iter()
            .filter(|e| dexter_world_model::digest_worthy(e))
            .take(max_out)
            .map(|e| {
                serde_json::json!({
                    "id": e.id.to_string(),
                    "role": e.role,
                    "source": e.source,
                    "name": e.name,
                    "value": e.value,
                    "enabled": e.enabled,
                    "focused": e.focused,
                    "actions": e.actions,
                    "bounds": e.bounds,
                })
            })
            .collect();
        let windows: Vec<serde_json::Value> = obs
            .windows
            .iter()
            .map(|w| {
                serde_json::json!({
                    "id": w.id,
                    "app": w.app,
                    "title": w.title,
                    "bounds": w.bounds,
                    "on_screen": w.on_screen,
                })
            })
            .collect();
        let elements_returned = elements.len();
        Ok(self.respond(
            "dexter_observe",
            serde_json::json!({
                "observation": obs.id.0,
                "windows": windows,
                "element_count": obs.elements.len(),
                "elements_returned": elements_returned,
                "elements_output_truncated": total_worthy > elements_returned,
                "elements": elements,
                "elements_truncated": obs.elements_truncated,
                "ax_limited": obs.ax_limited,
                "digest": obs.digest,
            }),
        ))
    }

    /// Application map: what this app IS and what it can DO — windows,
    /// control clusters, editable fields, navigation surfaces, menubar
    /// verbs and inferred capabilities, from one observation. The cheap
    /// first call for an unfamiliar app; pair with dexter_observe for
    /// element ids.
    #[tool(
        name = "dexter_map",
        description = "Map an app's interface: windows, controls, editable fields, navigation, menu verbs, inferred capabilities. `app` is required. `wake` (default true) briefly foregrounds the app if AX exposes no window content, then restores focus."
    )]
    async fn dexter_map(
        &self,
        Parameters(params): Parameters<MapParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        let selector = AppSelector::parse(&params.app);
        let wake = params.wake.unwrap_or(true);
        let scope = ObservationScope {
            app: Some(selector.clone()),
            ..Default::default()
        };
        let runtime = self.runtime.clone();
        let approve_all = self.runtime.config.approve_all;
        let map = tokio::task::spawn_blocking(move || {
            let mut engine = runtime.engine.lock().map_err(err)?;
            let mut obs = engine.driver().observe(&scope).map_err(err)?;
            // Window content only exists while the app is frontmost —
            // the borrow goes through policy like any visible side
            // effect: a denied/unapproved activation means the map is
            // built from the windowless world and says so, it never
            // activates unauthenticated.
            let mut stage = serde_json::json!("not_needed");
            if wake
                && !obs
                    .elements
                    .iter()
                    .any(|e| e.role.as_deref() == Some("window"))
            {
                let cfg = RunConfig {
                    approve_all,
                    ..Default::default()
                };
                match engine.borrow_stage(&selector, &cfg, &scope) {
                    Ok(dexter_engine::StageBorrow::Activated { handle, obs: new }) => {
                        if let Some(o) = new {
                            obs = o;
                        }
                        stage = serde_json::json!("activated");
                        engine.driver().restore(&handle);
                    }
                    Ok(dexter_engine::StageBorrow::Denied { reason }) => {
                        stage = serde_json::json!({"denied": reason});
                    }
                    Ok(dexter_engine::StageBorrow::NeedsApproval {
                        fingerprint,
                        reason,
                    }) => {
                        stage = serde_json::json!({
                            "needs_approval": reason,
                            "fingerprint": fingerprint,
                        });
                    }
                    Ok(dexter_engine::StageBorrow::Clear) => {
                        stage = serde_json::json!("clear");
                    }
                    Err(e) => {
                        stage = serde_json::json!({"error": e.to_string()});
                    }
                }
            }
            let mut map = serde_json::to_value(dexter_world_model::app_map(&obs)).map_err(err)?;
            map["stage"] = stage;
            Ok::<_, McpError>(map)
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        Ok(self.respond("dexter_map", map))
    }

    /// Ranked menu of plausible actions for a goal — the agent stays the
    /// decider, Dexter supplies what the world currently affords. Each
    /// candidate's action is ready to pass straight to dexter_act.
    #[tool(
        name = "dexter_candidates",
        description = "Ranked plausible actions for a goal (action JSON + rationale + heuristic prior)"
    )]
    async fn dexter_candidates(
        &self,
        Parameters(params): Parameters<CandidatesParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        let scope = ObservationScope {
            app: params.app.as_deref().map(AppSelector::parse),
            max_elements: 4_000,
            ..Default::default()
        };
        let runtime = self.runtime.clone();
        let goal = params.goal.clone();
        let (obs, cands) = tokio::task::spawn_blocking(move || {
            let engine = runtime.engine.lock().map_err(err)?;
            let obs = engine.driver().observe(&scope).map_err(err)?;
            let cands =
                runtime
                    .generator
                    .generate(&obs, &goal, &dexter_decision::GenHistory::default());
            Ok::<_, McpError>((obs, cands))
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        let max = params.max.unwrap_or(20);
        let candidates: Vec<serde_json::Value> = cands
            .iter()
            .take(max)
            .map(|c| {
                serde_json::json!({
                    "action": c.action,
                    "rationale": c.rationale,
                    "prior": c.prior,
                })
            })
            .collect();
        Ok(self.respond(
            "dexter_candidates",
            serde_json::json!({
                "observation": obs.id.0,
                "candidates": candidates,
                "note": "priors are heuristic hints — the agent decides; \
                         dexter_act still runs policy+verify on whatever it picks",
            }),
        ))
    }

    /// Execute one action through policy -> act -> verify. Returns the
    /// StepStatus JSON: done | needs_approval(fingerprint) | denied |
    /// failed | error.
    #[tool(
        name = "dexter_act",
        description = "Run one action through policy+verify; needs_approval returns a fingerprint for dexter_grant"
    )]
    async fn dexter_act(
        &self,
        Parameters(params): Parameters<ActParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        // Payload bound — an action blob has no business being huge.
        // Count serialized bytes without materializing the JSON.
        if json_len(&params.action)? > 65_536 {
            return Err(err("action JSON exceeds 64KB"));
        }
        let action: Action = serde_json::from_value(params.action)
            .map_err(|e| err(format!("invalid action JSON: {e}")))?;
        let expect: Option<ExpectedState> =
            params
                .expect
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| err(format!("invalid expect JSON: {e}")))?;
        let step = Step {
            note: None,
            action,
            expect,
            max_attempts: Some(3),
            app: params.app.as_deref().map(AppSelector::parse),
        };
        let cfg = run_cfg(params.app.clone(), self.runtime.config);
        let presence = self.runtime.config.presence;
        let runtime = self.runtime.clone();
        let status = tokio::task::spawn_blocking(move || {
            let mut engine = runtime.engine.lock().map_err(err)?;
            let events = presence.then(dexter_engine::presence::overlay_journal_path);
            if let Some(p) = &events {
                // Presence is best-effort — never fail the act.
                let _ = engine.set_journal_sink(p);
                let _ = dexter_engine::presence::spawn_overlay(p);
            }
            let status = engine.run_step(&step, &cfg);
            if events.is_some() {
                use dexter_engine::StepStatus;
                let (kind, data) = match &status {
                    StepStatus::Done { .. } => (
                        dexter_core::EventKind::TaskCompleted,
                        serde_json::json!({"steps": 1}),
                    ),
                    StepStatus::Denied { reason } => (
                        dexter_core::EventKind::TaskFailed,
                        serde_json::json!({"outcome": "denied", "reason": reason}),
                    ),
                    StepStatus::NeedsApproval { reason, .. } => (
                        dexter_core::EventKind::TaskFailed,
                        serde_json::json!({"outcome": "escalated", "reason": reason}),
                    ),
                    StepStatus::Failed { reason, .. } => (
                        dexter_core::EventKind::TaskFailed,
                        serde_json::json!({"outcome": "failed", "reason": reason}),
                    ),
                    StepStatus::Errored { error } => (
                        dexter_core::EventKind::TaskFailed,
                        serde_json::json!({"outcome": "failed", "reason": error.to_string()}),
                    ),
                };
                engine.emit(kind, data);
            }
            // The overlay file is per-act — detach it so unrelated
            // engine events don't keep streaming into it.
            if events.is_some() {
                engine.clear_journal_sink();
            }
            Ok::<_, McpError>(status)
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        Ok(self.respond("dexter_act", status_json(status)))
    }

    /// Grant an approval fingerprint for this session (single use,
    /// TTL-bound). This is the human-in-the-loop hook — note the same
    /// channel that surfaces the fingerprint can grant it, so an
    /// autonomous agent could self-serve. Operators who need approval
    /// authority outside the agent's reach start the server with
    /// `no_grants`.
    #[tool(
        name = "dexter_grant",
        description = "Grant an approval fingerprint returned by a needs_approval step (single-use, session-scoped)"
    )]
    async fn dexter_grant(
        &self,
        Parameters(params): Parameters<GrantParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        if self.runtime.config.no_grants {
            return Err(err(
                "dexter_grant is disabled by the operator — approvals must come \
                 from outside this channel",
            ));
        }
        let mut engine = self.runtime.engine.lock().map_err(err)?;
        engine.grant_approval(&params.fingerprint);
        Ok(self.respond(
            "dexter_grant",
            serde_json::json!({
                "granted": true,
                "fingerprint": params.fingerprint,
            }),
        ))
    }

    /// Check an ExpectedState against a fresh observation.
    #[tool(
        name = "dexter_verify",
        description = "Check an ExpectedState against a fresh observation (VERIFIED/FAILED/UNCERTAIN)"
    )]
    async fn dexter_verify(
        &self,
        Parameters(params): Parameters<VerifyParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        let scope = ObservationScope {
            app: params.app.as_deref().map(AppSelector::parse),
            ..Default::default()
        };
        let expected: ExpectedState = serde_json::from_value(params.expected)
            .map_err(|e| err(format!("invalid expected JSON: {e}")))?;
        let runtime = self.runtime.clone();
        let v = tokio::task::spawn_blocking(move || {
            let engine = runtime.engine.lock().map_err(err)?;
            let obs = engine.driver().observe(&scope).map_err(err)?;
            Ok::<_, McpError>(dexter_verify::verify(&obs, &expected))
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        Ok(self.respond(
            "dexter_verify",
            serde_json::json!({
                "status": format!("{:?}", v.status),
                "checks": v.checks,
            }),
        ))
    }

    /// Closed-loop task: observe -> candidates -> decide -> act ->
    /// recheck until `done` verifies or bounds hit.
    #[tool(
        name = "dexter_task",
        description = "Run a goal in the closed loop (decider chosen at server start) until done_when verifies"
    )]
    async fn dexter_task(
        &self,
        Parameters(params): Parameters<TaskParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        let done: ExpectedState = serde_json::from_value(params.done.clone())
            .map_err(|e| err(format!("invalid done JSON: {e}")))?;
        // Payload bounds — a misbehaving agent can't request an
        // unbounded loop or an oversized goal/done blob.
        if params.goal.len() > 4_096 {
            return Err(err("goal exceeds 4KB"));
        }
        if json_len(&params.done)? > 65_536 {
            return Err(err("done spec exceeds 64KB"));
        }
        let max_steps = params.max_steps.unwrap_or(10).min(200);
        let max_secs = params.max_secs.map(|s| s.min(3_600));
        let runtime = self.runtime.clone();
        let goal = params.goal.clone();
        // Fresh cooperative-cancel token for this task — dexter_cancel
        // flips it; cleared when the task returns. One world = one task:
        // a second concurrent task would overwrite the slot and orphan
        // the running task's token — reject it honestly instead.
        let token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut slot = runtime.task_cancel.lock().map_err(err)?;
            if slot.is_some() {
                return Err(err("a dexter_task is already running — cancel it or wait"));
            }
            *slot = Some(token.clone());
        }
        let presence = runtime.config.presence;
        let outcome = tokio::task::spawn_blocking(move || {
            let mut engine = runtime.engine.lock().map_err(err)?;
            if presence {
                // run_plan emits its own terminal events — the overlay
                // only needs the sink and a tail.
                let p = dexter_engine::presence::overlay_journal_path();
                let _ = engine.set_journal_sink(&p);
                let _ = dexter_engine::presence::spawn_overlay(&p);
            }
            // Sequential goals: "write X and save" runs as ordered
            // subgoals — the done expectation belongs to the last.
            let parts = dexter_decision::split_goal(&goal);
            let last = parts.len() - 1;
            let subgoals: Vec<dexter_engine::Subgoal> = parts
                .iter()
                .enumerate()
                .map(|(i, g)| dexter_engine::Subgoal {
                    goal: g.clone(),
                    done_when: (i == last).then(|| done.clone()),
                })
                .collect();
            let outcome = engine.run_plan(
                &subgoals,
                &runtime.generator,
                runtime.decider.as_ref(),
                &TaskConfig {
                    run: RunConfig {
                        window_scope: params.window,
                        ..run_cfg(params.app.clone(), runtime.config)
                    },
                    max_steps,
                    max_duration: max_secs.map(Duration::from_secs),
                    cancel: Some(token),
                    done_when: done,
                },
            );
            if presence {
                engine.clear_journal_sink();
            }
            *runtime.task_cancel.lock().map_err(err)? = None;
            Ok::<_, McpError>(outcome)
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        let status = match &outcome {
            dexter_engine::PlanOutcome::Completed { subgoals, steps } => {
                serde_json::json!({"status": "completed", "steps": steps, "subgoals": subgoals})
            }
            dexter_engine::PlanOutcome::Failed {
                index,
                goal,
                inner,
                completed,
            } => {
                let inner_status = match inner.as_ref() {
                    TaskOutcome::Abstained { reason } => {
                        serde_json::json!({"status": "abstained", "reason": reason})
                    }
                    TaskOutcome::Escalated { route, reason } => serde_json::json!({
                        "status": "escalated",
                        "route": format!("{route:?}"),
                        "reason": reason,
                    }),
                    TaskOutcome::NeedsApproval {
                        fingerprint,
                        reason,
                        action,
                    } => serde_json::json!({
                        "status": "needs_approval",
                        "fingerprint": fingerprint,
                        "reason": reason,
                        "action": action,
                    }),
                    TaskOutcome::Denied { reason } => {
                        serde_json::json!({"status": "denied", "reason": reason})
                    }
                    TaskOutcome::Failed { reason } => {
                        serde_json::json!({"status": "failed", "reason": reason})
                    }
                    TaskOutcome::MaxSteps => serde_json::json!({"status": "max_steps"}),
                    TaskOutcome::Cancelled => serde_json::json!({"status": "cancelled"}),
                    TaskOutcome::TimedOut { elapsed } => serde_json::json!({
                        "status": "timed_out",
                        "elapsed_ms": elapsed.as_millis() as u64,
                    }),
                    TaskOutcome::Completed { .. } => {
                        serde_json::json!({"status": "failed", "reason": "unexpected"})
                    }
                };
                let mut v = inner_status;
                v["subgoal_index"] = (*index).into();
                v["subgoal"] = goal.clone().into();
                v["subgoals_completed"] = (*completed).into();
                v
            }
        };
        // Steps a completed plan absorbed — each is a loop the agent
        // didn't pay a call for. Failed plans' internal work stays
        // journal-visible but isn't claimed as avoided calls.
        if let dexter_engine::PlanOutcome::Completed { steps, .. } = &outcome {
            if let Ok(mut m) = self.runtime.metrics.lock() {
                m.task_internal_steps += *steps as u64;
            }
        }
        Ok(self.respond("dexter_task", status))
    }

    /// Ask the running `dexter_task` to stop between steps (cooperative —
    /// the current action finishes first). No-op when nothing is running.
    #[tool(
        name = "dexter_cancel",
        description = "Cancel the running dexter_task cooperatively (checked between steps)"
    )]
    async fn dexter_cancel(&self) -> Result<Json<serde_json::Value>, McpError> {
        let slot = self.runtime.task_cancel.lock().map_err(err)?;
        let cancelled = match slot.as_ref() {
            Some(t) => {
                t.store(true, std::sync::atomic::Ordering::Relaxed);
                true
            }
            None => false,
        };
        Ok(self.respond("dexter_cancel", serde_json::json!({"cancelled": cancelled})))
    }

    /// Audit journal for this session — every observation, policy check,
    /// decision, action and verification. Readable while a task runs.
    #[tool(
        name = "dexter_journal",
        description = "Session audit journal (JSONL-shaped event list)"
    )]
    async fn dexter_journal(&self) -> Result<Json<serde_json::Value>, McpError> {
        let journal = self.runtime.journal.lock().map_err(err)?;
        Ok(self.respond(
            "dexter_journal",
            serde_json::json!({
                "events": journal.events,
                "dropped": journal.dropped,
            }),
        ))
    }

    /// Runtime status probe: driver capabilities, decision-engine
    /// health (read-only liveness — never respawns), journal stats and
    /// whether a task is in flight. Never takes the engine lock, so it
    /// answers while a task runs.
    #[tool(
        name = "dexter_status",
        description = "Runtime status: driver, decision-engine health, journal stats, task running"
    )]
    async fn dexter_status(&self) -> Result<Json<serde_json::Value>, McpError> {
        let runtime = self.runtime.clone();
        let (health, jlen, jdropped, task_running) = tokio::task::spawn_blocking(move || {
            let health = runtime.decider.health();
            let (jlen, jdropped) = {
                let j = runtime.journal.lock().map_err(err)?;
                (j.events.len(), j.dropped)
            };
            let task_running = runtime.task_cancel.lock().map_err(err)?.is_some();
            Ok::<_, McpError>((health, jlen, jdropped, task_running))
        })
        .await
        .map_err(|e| err(format!("join: {e}")))??;
        let caps = &self.runtime.caps;
        // Snapshot of agent-cost accounting. `est_response_tokens` is a
        // bytes/4 heuristic — it measures the response payloads this
        // session returned, not provider billing tokens.
        let session = self
            .runtime
            .metrics
            .lock()
            .map(|m| {
                serde_json::json!({
                    "tool_calls": m.calls,
                    "tool_calls_total": m.calls.values().sum::<u64>(),
                    "response_bytes": m.response_bytes,
                    "est_response_tokens": m.est_response_tokens(),
                    "task_internal_steps": m.task_internal_steps,
                })
            })
            .unwrap_or(serde_json::json!({}));
        Ok(self.respond(
            "dexter_status",
            serde_json::json!({
                "driver": {
                    "name": caps.name,
                    "element_tree": caps.element_tree,
                    "background_input": caps.background_input,
                },
                "engine": {
                    "name": self.runtime.decider.name(),
                    "health": health,
                },
                "trust": {
                    "approve_all": self.runtime.config.approve_all,
                    "coords": self.runtime.config.allow_coords,
                },
                "journal": { "events": jlen, "dropped": jdropped },
                "task_running": task_running,
                "session": session,
            }),
        ))
    }
}

fn status_json(status: StepStatus) -> serde_json::Value {
    match status {
        StepStatus::Done {
            result,
            verification,
            attempts,
        } => serde_json::json!({
            "status": "done",
            "attempts": attempts,
            "result": result,
            "verification": verification,
        }),
        StepStatus::Denied { reason } => {
            serde_json::json!({"status": "denied", "reason": reason})
        }
        StepStatus::NeedsApproval {
            fingerprint,
            reason,
            action,
        } => serde_json::json!({
            "status": "needs_approval",
            "reason": reason,
            "fingerprint": fingerprint,
            "action": action,
        }),
        StepStatus::Failed { reason, attempts } => serde_json::json!({
            "status": "failed",
            "reason": reason,
            "attempts": attempts,
        }),
        StepStatus::Errored { error } => {
            serde_json::json!({"status": "error", "error": error.to_string()})
        }
    }
}

#[tool_handler]
impl ServerHandler for DexterMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::V_2025_06_18,
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "dexter".into(),
                title: Some("Dexter — agent computer runtime".into()),
                version: env!("CARGO_PKG_VERSION").into(),
                icons: None,
                website_url: Some("https://github.com/Shugar03/dexter".into()),
            },
            instructions: Some(
                "Workflow: dexter_map (what is this app, what can it do) -> \
                 dexter_observe (digest + structured elements) -> \
                 dexter_candidates (ranked action menu for your goal) -> \
                 dexter_act on the action you choose (policy gates every \
                 call; needs_approval returns a fingerprint a human grants \
                 via dexter_grant) -> dexter_verify to check post-state. \
                 dexter_task runs the whole loop itself; dexter_journal is \
                 the audit trail. Prefer semantic/element targets — \
                 coordinates are physical-tier, denied unless the operator \
                 launched the server with --coords."
                    .into(),
            ),
        }
    }
}

/// Serve over stdio until the client disconnects. `decider` overrides
/// the engine `dexter_task` uses (`None` = rule-based); `config` is the
/// operator trust level for the whole session.
pub async fn serve_stdio(
    policy: Policy,
    driver: Box<dyn ComputerDriver>,
    decider: Option<Box<dyn DecisionEngine>>,
    config: ServerConfig,
) -> anyhow::Result<()> {
    use rmcp::service::ServiceExt;
    let server = DexterMcp::with_decider(policy, driver, decider, config);
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
