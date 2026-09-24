//! `LayaEngine` — a `DecisionEngine` backed by a sidecar worker.
//!
//! The worker is a child process speaking NDJSON over stdio:
//!
//! ```text
//! → {"id":1,"method":"predict","params":{"state":"…","questions":[Question…]}}
//! ← {"id":1,"ok":true,"provider":"laya","answers":[Answer…]}
//! ← {"id":1,"ok":false,"error":"laya SDK not installed"}
//! ```
//!
//! `decide()` renders the `DecisionContext` into ONE `choice` question
//! whose options are the generated candidates plus trailing route
//! options (`wait`, `reobserve`, `escalate`). The model picks an index;
//! we interpret it. The worker binary comes from `DEXTER_LAYA_WORKER`
//! or `--engine-path`; there is no bundled fake — a missing worker is a
//! `DecisionError`, never a silent fallback.

use dexter_core::Action;
use dexter_decision::{
    Answer, CandidateAction, Decision, DecisionContext, DecisionEngine, DecisionError,
    EngineHealth, Question, Route,
};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Mutex;
use std::time::Duration;

/// Extra options appended after the real candidates, in order.
const ROUTE_OPTIONS: [(&str, Route); 4] = [
    ("wait and re-observe", Route::Wait { millis: 500 }),
    ("re-observe without acting", Route::Reobserve),
    ("abstain — no candidate fits the goal", Route::Abstain),
    ("escalate to a larger model", Route::EscalateLlm),
];

#[derive(Serialize)]
struct PredictRequest<'a> {
    id: u64,
    method: &'a str,
    params: PredictParams<'a>,
}

#[derive(Serialize)]
struct PredictParams<'a> {
    state: &'a str,
    questions: &'a [Question],
}

#[derive(Deserialize)]
struct PredictResponse {
    ok: bool,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    answers: Option<Vec<Answer>>,
    #[serde(default)]
    error: Option<String>,
}

/// One worker process: child + its pipes. Swapped wholesale on respawn.
struct WorkerProc {
    #[allow(dead_code)]
    child: Child, // kept alive for the worker's lifetime
    stdin: ChildStdin,
    responses: Receiver<String>,
}

impl WorkerProc {
    fn spawn(worker_cmd: &str) -> std::io::Result<Self> {
        let mut parts = worker_cmd.split_whitespace();
        let program = parts.next().unwrap_or(worker_cmd);
        let mut child = Command::new(program)
            .args(parts)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        // Reader thread: worker stdout lines → response channel. Responses
        // are consumed with recv_timeout so a hung worker fails as
        // DecisionError::Timeout instead of blocking forever.
        let (tx, rx) = channel::<String>();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) if !l.trim().is_empty() => {
                        if tx.send(l).is_err() {
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            responses: rx,
        })
    }
}

/// Bounded respawns per engine — a crash-looping worker fails hard
/// instead of masking a broken install.
const MAX_RESPAWNS: u32 = 2;

/// Transport-level failures justify a respawn+retry; protocol-level
/// ones (bad JSON, ok:false) do not — the worker is alive, the answer
/// was the problem.
fn is_transport_error(e: &DecisionError) -> bool {
    match e {
        DecisionError::Timeout { .. } => true,
        DecisionError::Engine { message, .. } => message.starts_with("worker io:"),
    }
}

/// Decision engine that asks a Laya sidecar to rank candidates.
pub struct LayaEngine {
    worker: Mutex<WorkerProc>,
    worker_cmd: String,
    next_id: Mutex<u64>,
    timeout: Duration,
    /// Below this calibrated confidence the engine abstains instead of
    /// acting. `0.0` = never gate (report only).
    min_confidence: f32,
    /// Last provider string reported by the worker (audit).
    provider: Mutex<String>,
    /// Worker crashes tolerated — a dead child is respawned once per
    /// failure and the call retried once.
    respawns_left: std::sync::atomic::AtomicU32,
}

impl LayaEngine {
    /// Spawn a worker process speaking the NDJSON predict protocol.
    pub fn spawn(worker_cmd: &str, timeout: Duration) -> std::io::Result<Self> {
        Ok(Self {
            worker: Mutex::new(WorkerProc::spawn(worker_cmd)?),
            worker_cmd: worker_cmd.to_string(),
            next_id: Mutex::new(0),
            timeout,
            min_confidence: 0.0,
            provider: Mutex::new("unknown".into()),
            respawns_left: std::sync::atomic::AtomicU32::new(MAX_RESPAWNS),
        })
    }

    /// Gate acting on the model's calibrated confidence.
    pub fn with_min_confidence(mut self, min: f32) -> Self {
        self.min_confidence = min;
        self
    }

    /// Which provider answered last (`laya`, `dev`, …) — audit metadata.
    pub fn provider(&self) -> String {
        self.provider.lock().unwrap().clone()
    }

    /// Supervised rpc: on a transport failure (dead/hung worker) respawn
    /// the sidecar once and retry once. Protocol errors (bad JSON,
    /// `ok:false`) are not retried — the worker is healthy, the answer
    /// was the problem.
    fn rpc(&self, questions: &[Question], state: &str) -> Result<PredictResponse, DecisionError> {
        match self.rpc_once(questions, state) {
            Err(e) if is_transport_error(&e) => {
                self.respawn()?;
                let out = self.rpc_once(questions, state);
                if out.is_ok() {
                    // Healthy again — refund the respawn budget.
                    self.respawns_left
                        .store(MAX_RESPAWNS, std::sync::atomic::Ordering::Relaxed);
                }
                out
            }
            other => other,
        }
    }

    /// Kill the current child (if still running) and spawn a fresh one.
    /// Bounded by `respawns_left`.
    fn respawn(&self) -> Result<(), DecisionError> {
        let left = self
            .respawns_left
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |n| n.checked_sub(1),
            )
            .map_err(|_| DecisionError::Engine {
                engine: self.name().into(),
                message: "worker respawn budget exhausted — not retrying".into(),
            })?;
        let _ = left; // consumed
        let mut w = self.worker.lock().unwrap();
        let _ = w.child.kill(); // already-dead is fine
        let _ = w.child.wait(); // reap
        *w = WorkerProc::spawn(&self.worker_cmd).map_err(|e| DecisionError::Engine {
            engine: self.name().into(),
            message: format!("worker respawn failed: {e}"),
        })?;
        Ok(())
    }

    fn rpc_once(
        &self,
        questions: &[Question],
        state: &str,
    ) -> Result<PredictResponse, DecisionError> {
        let id = {
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            *n
        };
        let req = PredictRequest {
            id,
            method: "predict",
            params: PredictParams { state, questions },
        };
        let line = serde_json::to_string(&req).map_err(|e| DecisionError::Engine {
            engine: self.name().into(),
            message: format!("serialize request: {e}"),
        })?;
        // Hold the worker lock across write+read: the sidecar is a
        // serial protocol, one outstanding request at a time.
        let w = self.worker.lock().unwrap();
        let mut stdin = &w.stdin;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| DecisionError::Engine {
                engine: self.name().into(),
                message: format!("worker io: {e}"),
            })?;
        let line = w
            .responses
            .recv_timeout(self.timeout)
            .map_err(|_| DecisionError::Timeout {
                engine: self.name().into(),
                millis: self.timeout.as_millis() as u64,
            })?;
        serde_json::from_str(&line).map_err(|e| DecisionError::Engine {
            engine: self.name().into(),
            message: format!("bad worker response '{line}': {e}"),
        })
    }
}

fn describe_candidate(i: usize, c: &CandidateAction) -> String {
    let verb = match &c.action {
        Action::Click { .. } => "click",
        Action::TypeText { .. } => "type text",
        Action::Key { .. } => "press key",
        Action::Scroll { .. } => "scroll",
        Action::Focus { .. } => "focus",
        Action::SetValue { .. } => "set value",
        Action::Navigate { .. } => "navigate",
        Action::Invoke { .. } => "invoke",
        Action::LaunchApp { .. } => "launch app",
        Action::QuitApp { .. } => "quit app",
        Action::Window { .. } => "window",
        Action::ReadClipboardText => "read clipboard",
        Action::WriteClipboardText { .. } => "write clipboard",
        Action::Drag { .. } => "drag",
        Action::Observe => "observe",
        Action::Wait { .. } => "wait",
    };
    // Keep the generator's rationale (it carries role + label + priors)
    // but lead with the verb — the model reads options as actions.
    format!("candidate {i}: {verb} — {}", c.rationale)
}

/// Build the exact (state, question) pair `decide` sends to the worker.
/// Public so eval/training export renders the identical distribution the
/// model sees at inference time.
pub fn build_question(ctx: &DecisionContext) -> (String, Question) {
    let mut options: Vec<String> = ctx
        .candidates
        .iter()
        .enumerate()
        .map(|(i, c)| describe_candidate(i, c))
        .collect();
    for (label, _) in &ROUTE_OPTIONS {
        options.push((*label).to_string());
    }
    let state = format!(
        "[GOAL]\n{}\n\n[WORLD_STATE]\n{}\n\n[LAST_ERROR]\n{}",
        ctx.goal,
        ctx.state_digest,
        ctx.last_error.as_deref().unwrap_or("none"),
    );
    let q = Question::Choice {
        id: "pick".into(),
        prompt: "You are choosing the next action for a computer-use agent. \
                 Pick the UI element whose action best advances the goal in [GOAL]. \
                 If no element fits, pick a route: 'wait' for busy/loading states, \
                 're-observe' when the view may be stale, 'abstain' when nothing \
                 applies, or 'escalate' for genuinely hard steps. Priors in the \
                 option text are heuristic hints, not truth."
            .into(),
        options,
    };
    (state, q)
}

/// Route variant names in ROUTE_OPTIONS order — the mapping eval
/// export uses to turn a gold route into an option index.
/// Order must match ROUTE_OPTIONS above.
pub const ROUTE_VARIANT_ORDER: [&str; 4] = ["wait", "reobserve", "abstain", "escalate_llm"];

/// Route options appended after candidates — index of the matching
/// route option for `route`, or None if it isn't offered.
pub fn route_option_index(route: Route) -> Option<usize> {
    ROUTE_OPTIONS.iter().position(|(_, r)| *r == route)
}

/// Number of route options appended after the candidates.
pub fn route_option_count() -> usize {
    ROUTE_OPTIONS.len()
}

impl DecisionEngine for LayaEngine {
    fn name(&self) -> &str {
        "laya"
    }

    /// Liveness probe: one `predict` with no questions. Read-only —
    /// it never respawns; supervision state belongs to real calls.
    fn health(&self) -> EngineHealth {
        match self.rpc_once(&[], "health probe") {
            Ok(r) if r.ok => EngineHealth::Ready,
            Ok(r) => {
                EngineHealth::Degraded(r.error.unwrap_or_else(|| "probe returned ok:false".into()))
            }
            Err(e) if is_transport_error(&e) => EngineHealth::Down(format!(
                "{e} (respawns left: {})",
                self.respawns_left
                    .load(std::sync::atomic::Ordering::Relaxed)
            )),
            Err(e) => EngineHealth::Degraded(format!("probe error: {e}")),
        }
    }

    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        let (state, question) = build_question(ctx);
        let questions = [question];

        let resp = self.rpc(&questions, &state)?;
        if let Some(p) = &resp.provider {
            *self.provider.lock().unwrap() = p.clone();
        }
        if !resp.ok {
            return Err(DecisionError::Engine {
                engine: self.name().into(),
                message: resp.error.unwrap_or_else(|| "unknown".into()),
            });
        }
        let answer = resp
            .answers
            .and_then(|a| a.into_iter().next())
            .ok_or_else(|| DecisionError::Engine {
                engine: self.name().into(),
                message: "worker returned ok but no answers".into(),
            })?;
        let (idx, confidence) = match answer {
            Answer::Choice {
                index, confidence, ..
            } => (index, confidence),
            other => {
                return Err(DecisionError::Engine {
                    engine: self.name().into(),
                    message: format!("expected choice answer, got {other:?}"),
                })
            }
        };

        // Calibrated abstention: when the model says its pick is unlikely
        // to be right, prefer the honest route over a shot in the dark.
        if let Some(c) = confidence {
            if c < self.min_confidence {
                return Ok(Decision::Route {
                    route: Route::Abstain,
                    rationale: format!(
                        "laya confidence {c:.2} < min {} — abstaining",
                        self.min_confidence
                    ),
                });
            }
        }

        if idx < ctx.candidates.len() {
            let c = &ctx.candidates[idx];
            let conf = confidence
                .map(|c| format!(" (confidence {c:.2})"))
                .unwrap_or_default();
            return Ok(Decision::Act {
                action: Box::new(c.action.clone()),
                candidate_index: Some(idx),
                rationale: format!("laya picked candidate {idx}{conf}: {}", c.rationale),
            });
        }
        let route_idx = idx - ctx.candidates.len();
        if let Some((label, route)) = ROUTE_OPTIONS.get(route_idx) {
            return Ok(Decision::Route {
                route: *route,
                rationale: format!("laya picked route '{label}'"),
            });
        }
        Err(DecisionError::Engine {
            engine: self.name().into(),
            message: format!("answer index {idx} out of range"),
        })
    }
}

/// The Action a Laya response selected — re-exported for callers that
/// want to inspect what the model picked vs what the generator offered.
#[allow(dead_code)]
pub fn picked_action(d: &Decision) -> Option<&Action> {
    match d {
        Decision::Act { action, .. } => Some(action),
        _ => None,
    }
}
