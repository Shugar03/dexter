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
}

impl DexterRuntime {
    pub fn new(policy: Policy, driver: Box<dyn ComputerDriver>) -> Self {
        Self {
            engine: Mutex::new(Engine::new(driver, policy, Duration::from_secs(300))),
            generator: HeuristicGenerator::default(),
            decider: Box::new(dexter_decision::RuleBased::default()),
        }
    }

    /// Swap the decider `dexter_task` uses (spawned once, kept warm).
    pub fn with_decider(mut self, d: Box<dyn DecisionEngine>) -> Self {
        self.decider = d;
        self
    }
}

fn run_cfg(app: Option<String>, coords: bool, approve_all: bool) -> RunConfig {
    RunConfig {
        app: app.as_deref().map(AppSelector::parse),
        max_attempts: 3,
        verify_delay: Duration::from_millis(250),
        allow_coordinates: coords,
        approve_all,
        observe_max_elements: 4_000,
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ObserveParams {
    /// Scope to an app: name, `com.bundle.id` or pid.
    pub app: Option<String>,
    /// Cap on flattened elements.
    pub max_elements: Option<usize>,
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
    /// Permit coordinate-level input.
    pub coords: Option<bool>,
    /// Approve this exact action (human-approved flag).
    pub approve: Option<bool>,
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
    /// Max decide/act iterations (default 10).
    pub max_steps: Option<u32>,
    /// Approve all required actions (the human approved the goal).
    pub approve_all: Option<bool>,
    /// Permit coordinate-level input.
    pub coords: Option<bool>,
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

#[tool_router]
impl DexterMcp {
    pub fn new(policy: Policy, driver: Box<dyn ComputerDriver>) -> Self {
        Self::with_decider(policy, driver, None)
    }

    /// Same as `new`, plus the decider `dexter_task` should use
    /// (`None` = rule-based).
    pub fn with_decider(
        policy: Policy,
        driver: Box<dyn ComputerDriver>,
        decider: Option<Box<dyn DecisionEngine>>,
    ) -> Self {
        let mut runtime = DexterRuntime::new(policy, driver);
        if let Some(d) = decider {
            runtime = runtime.with_decider(d);
        }
        Self {
            runtime: Arc::new(runtime),
            tool_router: Self::tool_router(),
        }
    }

    /// Observe the current world: window list + element count + text
    /// digest. This is what a decision layer should read first.
    #[tool(
        name = "dexter_observe",
        description = "Observe the world: windows, element count, text digest"
    )]
    async fn dexter_observe(
        &self,
        Parameters(params): Parameters<ObserveParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        let scope = ObservationScope {
            app: params.app.as_deref().map(AppSelector::parse),
            max_elements: params.max_elements.unwrap_or(4_000),
            ..Default::default()
        };
        let engine = self.runtime.engine.lock().map_err(err)?;
        let obs = engine.driver().observe(&scope).map_err(err)?;
        // Structured element list alongside the digest — agents target
        // `element` ids programmatically instead of parsing text. Capped
        // to keep the payload sane; the digest always stays complete-ish.
        let elements: Vec<serde_json::Value> = obs
            .elements
            .iter()
            .filter(|e| dexter_world_model::digest_worthy(e))
            .take(params.max_elements.unwrap_or(4_000).min(500))
            .map(|e| {
                serde_json::json!({
                    "id": e.id.to_string(),
                    "role": e.role,
                    "name": e.name,
                    "value": e.value,
                    "enabled": e.enabled,
                    "focused": e.focused,
                    "actions": e.actions,
                    "bounds": e.bounds,
                })
            })
            .collect();
        Ok(Json(serde_json::json!({
            "observation": obs.id.0,
            "windows": obs.windows.len(),
            "element_count": obs.elements.len(),
            "elements": elements,
            "elements_truncated": obs.elements_truncated,
            "ax_limited": obs.ax_limited,
            "digest": obs.digest,
        })))
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
        let engine = self.runtime.engine.lock().map_err(err)?;
        let obs = engine.driver().observe(&scope).map_err(err)?;
        let cands = self.runtime.generator.generate(
            &obs,
            &params.goal,
            &dexter_decision::GenHistory::default(),
        );
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
        Ok(Json(serde_json::json!({
            "observation": obs.id.0,
            "candidates": candidates,
            "note": "priors are heuristic hints — the agent decides; \
                     dexter_act still runs policy+verify on whatever it picks",
        })))
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
        let cfg = run_cfg(
            params.app.clone(),
            params.coords.unwrap_or(false),
            params.approve.unwrap_or(false),
        );
        let mut engine = self.runtime.engine.lock().map_err(err)?;
        let status = engine.run_step(&step, &cfg);
        status_json(status)
    }

    /// Grant an approval fingerprint for this session (single use,
    /// TTL-bound). This is the human-in-the-loop hook.
    #[tool(
        name = "dexter_grant",
        description = "Grant an approval fingerprint returned by a needs_approval step (single-use, session-scoped)"
    )]
    async fn dexter_grant(
        &self,
        Parameters(params): Parameters<GrantParams>,
    ) -> Result<Json<serde_json::Value>, McpError> {
        let mut engine = self.runtime.engine.lock().map_err(err)?;
        engine.grant_approval(&params.fingerprint);
        Ok(Json(serde_json::json!({
            "granted": true,
            "fingerprint": params.fingerprint,
        })))
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
        let engine = self.runtime.engine.lock().map_err(err)?;
        let obs = engine.driver().observe(&scope).map_err(err)?;
        let v = dexter_verify::verify(&obs, &expected);
        Ok(Json(serde_json::json!({
            "status": format!("{:?}", v.status),
            "checks": v.checks,
        })))
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
        let outcome = {
            let mut engine = self.runtime.engine.lock().map_err(err)?;
            engine.run_task(
                &params.goal,
                &self.runtime.generator,
                self.runtime.decider.as_ref(),
                &TaskConfig {
                    run: run_cfg(
                        params.app.clone(),
                        params.coords.unwrap_or(false),
                        params.approve_all.unwrap_or(false),
                    ),
                    max_steps: params.max_steps.unwrap_or(10),
                    done_when: serde_json::from_value::<ExpectedState>(params.done.clone())
                        .map_err(|e| err(format!("invalid done JSON: {e}")))?,
                },
            )
        };
        let status = match &outcome {
            TaskOutcome::Completed { steps } => {
                serde_json::json!({"status": "completed", "steps": steps})
            }
            TaskOutcome::Abstained { reason } => {
                serde_json::json!({"status": "abstained", "reason": reason})
            }
            TaskOutcome::Escalated { route, reason } => serde_json::json!({
                "status": "escalated",
                "route": format!("{route:?}"),
                "reason": reason,
            }),
            TaskOutcome::Failed { reason } => {
                serde_json::json!({"status": "failed", "reason": reason})
            }
            TaskOutcome::MaxSteps => serde_json::json!({"status": "max_steps"}),
        };
        Ok(Json(status))
    }

    /// Audit journal for this session — every observation, policy check,
    /// decision, action and verification.
    #[tool(
        name = "dexter_journal",
        description = "Session audit journal (JSONL-shaped event list)"
    )]
    async fn dexter_journal(&self) -> Result<Json<serde_json::Value>, McpError> {
        let engine = self.runtime.engine.lock().map_err(err)?;
        Ok(Json(serde_json::json!({
            "events": engine.events(),
        })))
    }
}

fn status_json(status: StepStatus) -> Result<Json<serde_json::Value>, McpError> {
    let v = match status {
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
        } => serde_json::json!({
            "status": "needs_approval",
            "reason": reason,
            "fingerprint": fingerprint,
        }),
        StepStatus::Failed { reason, attempts } => serde_json::json!({
            "status": "failed",
            "reason": reason,
            "attempts": attempts,
        }),
        StepStatus::Errored { error } => {
            serde_json::json!({"status": "error", "error": error.to_string()})
        }
    };
    Ok(Json(v))
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
                "Workflow: dexter_observe (digest + structured elements) -> \
                 dexter_candidates (ranked action menu for your goal) -> \
                 dexter_act on the action you choose (policy gates every \
                 call; needs_approval returns a fingerprint a human grants \
                 via dexter_grant) -> dexter_verify to check post-state. \
                 dexter_task runs the whole loop itself; dexter_journal is \
                 the audit trail. Prefer semantic/element targets — \
                 coordinates are physical-tier and need coords=true."
                    .into(),
            ),
        }
    }
}

/// Serve over stdio until the client disconnects. `decider` overrides
/// the engine `dexter_task` uses (`None` = rule-based).
pub async fn serve_stdio(
    policy: Policy,
    driver: Box<dyn ComputerDriver>,
    decider: Option<Box<dyn DecisionEngine>>,
) -> anyhow::Result<()> {
    use rmcp::service::ServiceExt;
    let server = DexterMcp::with_decider(policy, driver, decider);
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
