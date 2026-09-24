//! Task scenarios — end-to-end utility measurement.
//!
//! `eval run` replays frozen decision points; `eval scenario` replays
//! *tasks*: a declared world, a goal, and a `done_when`, run through the
//! real `Engine::run_task` loop. Metrics come from the journal the
//! engine already writes — success, steps, phase latencies, recoveries —
//! so this layer adds aggregation, not instrumentation.

use dexter_core::{AppSelector, Element, ElementId, Event, ExpectedState, SemanticTarget};
use dexter_decision::{CandidateGenerator, Decision, DecisionContext, DecisionEngine};
use dexter_driver::ComputerDriver;
use dexter_engine::{Engine, PlanOutcome, RunConfig, TaskConfig, TaskOutcome};
use dexter_sim::{Effect, SimDriver};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// `*.toml` task-scenario file.
#[derive(Debug, Deserialize)]
pub struct ScenarioSpec {
    pub scenario: ScenarioMeta,
    pub task: TaskSpec,
    /// Sim world — absent for live-driver scenarios.
    #[serde(default)]
    pub world: WorldSpec,
    /// Live-browser target — only read when `scenario.driver = "browser"`.
    #[serde(default)]
    pub browser: Option<BrowserSpec>,
    /// Real-app target — only read when `scenario.driver = "macos"`.
    #[serde(default)]
    pub live: Option<LiveSpec>,
}

impl ScenarioSpec {
    /// Which driver the scenario runs on. Absent = sim (hermetic).
    pub fn driver(&self) -> &str {
        self.scenario.driver.as_deref().unwrap_or("sim")
    }
}

#[derive(Debug, Deserialize)]
pub struct ScenarioMeta {
    pub id: String,
    /// The goal handed to the task loop, verbatim.
    pub goal: String,
    /// Steps an ideal agent needs — the denominator of
    /// steps-over-optimal. `None` = not scored for efficiency.
    pub optimal_steps: Option<u32>,
    /// Provenance label (like `meta.app` on eval items).
    pub app: Option<String>,
    /// Driver backend: absent/`"sim"` for the programmable world,
    /// `"browser"` for a live WebDriver session.
    #[serde(default)]
    pub driver: Option<String>,
}

/// Live-browser target for `driver = "browser"` scenarios.
#[derive(Debug, Deserialize)]
pub struct BrowserSpec {
    /// HTML file next to the spec (resolved to a `file://` URL).
    pub page: Option<String>,
    /// Any URL — used verbatim when `page` is absent.
    pub url: Option<String>,
    /// Post-navigation settle before the task loop starts.
    #[serde(default = "default_settle_ms")]
    pub settle_ms: u64,
}

fn default_settle_ms() -> u64 {
    500
}

/// Real-app target for `driver = "macos"` scenarios.
#[derive(Debug, Deserialize)]
pub struct LiveSpec {
    /// Observation scope: app name, `com.bundle.id` or pid — becomes
    /// `RunConfig.app`, so the run only sees this app's windows.
    pub app: String,
    /// Shell command run before each rep (launch/reset the fixture).
    pub prep: Option<String>,
    /// Shell command run after each rep — always, pass or fail.
    pub teardown: Option<String>,
    /// Post-prep settle before the task loop starts.
    #[serde(default = "default_settle_ms")]
    pub settle_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct TaskSpec {
    /// Structural completion check — verified per observation.
    pub done_when: ExpectedState,
    /// Which `TaskOutcome` counts as success. Default `completed`; use
    /// `abstained` for worlds where the correct answer is not to act.
    #[serde(default = "default_expected")]
    pub expected: String,
    /// Evidence assertion: minimum `VerificationFailed` events the run
    /// must show. `None` = no evidence requirement. Lets a scenario say
    /// "the loop must have *seen* the no-op", not just ended right.
    #[serde(default)]
    pub expect_verify_fails: Option<u32>,
    #[serde(default = "default_max_steps")]
    pub max_steps: u32,
    pub max_secs: Option<u64>,
    /// Approval fingerprints to pre-grant for the run.
    #[serde(default)]
    pub grants: Vec<String>,
}

fn default_expected() -> String {
    "completed".into()
}
fn default_max_steps() -> u32 {
    10
}

#[derive(Debug, Default, Deserialize)]
pub struct WorldSpec {
    #[serde(default)]
    pub element: Vec<SpecElement>,
    /// `on_press` rules: when the pressed element matches, apply effect.
    #[serde(default)]
    pub rule: Vec<WorldRule>,
    /// `on_tick` effects applied on every observe — self-evolving worlds.
    #[serde(default)]
    pub tick: Vec<SpecEffect>,
}

#[derive(Debug, Deserialize)]
pub struct WorldRule {
    pub when: SemanticTarget,
    pub effect: SpecEffect,
}

/// Light element spec — fills `Element::default()` for the rest so
/// scenario files only carry what matters.
#[derive(Debug, Deserialize)]
pub struct SpecElement {
    pub id: u64,
    #[serde(default)]
    pub parent: Option<u64>,
    #[serde(default)]
    pub depth: u32,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub focused: bool,
    #[serde(default)]
    pub actions: Vec<String>,
}

impl SpecElement {
    fn to_element(&self) -> Element {
        Element {
            id: ElementId(self.id),
            parent: self.parent.map(ElementId),
            depth: self.depth,
            role: self.role.clone(),
            name: self.name.clone(),
            value: self.value.clone(),
            enabled: self.enabled,
            focused: self.focused,
            actions: self.actions.clone(),
            ..Default::default()
        }
    }
}

/// Spec-side mirror of `dexter_sim::Effect` (which stays un-serde'd on
/// purpose — sim internals are code, spec files are data).
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SpecEffect {
    Spawn {
        element: SpecElement,
    },
    SetValue {
        value: String,
    },
    RemoveSelf,
    SetValueOf {
        target: SemanticTarget,
        value: String,
    },
    SetEnabledOf {
        target: SemanticTarget,
        enabled: bool,
    },
    Remove {
        target: SemanticTarget,
    },
    CycleValueOf {
        target: SemanticTarget,
        values: Vec<String>,
    },
}

impl SpecEffect {
    fn to_effect(&self) -> Effect {
        match self {
            SpecEffect::Spawn { element } => Effect::Spawn(element.to_element()),
            SpecEffect::SetValue { value } => Effect::SetValue(value.clone()),
            SpecEffect::RemoveSelf => Effect::RemoveSelf,
            SpecEffect::SetValueOf { target, value } => {
                Effect::SetValueOf(target.clone(), value.clone())
            }
            SpecEffect::SetEnabledOf { target, enabled } => {
                Effect::SetEnabledOf(target.clone(), *enabled)
            }
            SpecEffect::Remove { target } => Effect::Remove(target.clone()),
            SpecEffect::CycleValueOf { target, values } => {
                Effect::CycleValueOf(target.clone(), values.clone())
            }
        }
    }
}

/// Build the world the scenario runs against.
pub fn build_driver(spec: &ScenarioSpec) -> SimDriver {
    let driver = SimDriver::new(spec.world.element.iter().map(|e| e.to_element()).collect());
    for rule in &spec.world.rule {
        driver.on_press(rule.when.clone(), rule.effect.to_effect());
    }
    for tick in &spec.world.tick {
        driver.on_tick(tick.to_effect());
    }
    driver
}

/// What one scenario run produced — outcome plus journal-derived metrics.
#[derive(Debug)]
pub struct ScenarioRun {
    /// `TaskOutcome` variant, snake_case ("completed", "abstained", …).
    pub outcome: String,
    /// Whether the outcome matches `task.expected`.
    pub success: bool,
    /// Decide/act iterations used.
    pub steps: u32,
    /// Wall-clock of the whole task (engine-reported).
    pub elapsed_ms: u64,
    /// Per-step engine latency: CandidatesGenerated → DecisionMade.
    /// The number that answers "is this engine fast enough".
    pub decide_ms: Vec<u64>,
    /// Per-step generation time: ObservationCreated → CandidatesGenerated
    /// (verify + candidate generation).
    pub gen_ms: Vec<u64>,
    /// Per-step act time: DecisionMade → ActionExecuted.
    pub act_ms: Vec<u64>,
    /// `RecoveryStarted` events (verify-failed retries inside a step).
    pub recoveries: usize,
    /// `VerificationFailed` events.
    pub verify_fails: usize,
    /// `ActionFailed` events (driver errors).
    pub action_failures: usize,
    /// `HumanApprovalRequired` events.
    pub approvals: usize,
    /// Actions executed via `Mechanism::Coordinates` — on sim that means
    /// the engine reached for physical input where semantics should do.
    pub physical_acts: usize,
    /// The run's journal — the raw material for `--export` rows and
    /// `--journal-out` dumps.
    pub events: Vec<Event>,
}

/// Run one scenario once through the real closed loop, on the
/// programmable sim world it declares.
pub fn run_scenario(
    spec: &ScenarioSpec,
    generator: &dyn CandidateGenerator,
    decider: &dyn DecisionEngine,
) -> ScenarioRun {
    run_scenario_with(spec, build_driver(spec), generator, decider, None)
}

/// Same run on any driver — the browser surface injects a live
/// `BrowserDriver` here after navigating to the scenario's page.
/// `journal_path` streams the run's events to a live sink — the
/// presence overlay tails it mid-run.
pub fn run_scenario_with<D: ComputerDriver>(
    spec: &ScenarioSpec,
    driver: D,
    generator: &dyn CandidateGenerator,
    decider: &dyn DecisionEngine,
    journal_path: Option<&std::path::Path>,
) -> ScenarioRun {
    let mut engine = Engine::new(
        driver,
        dexter_policy::Policy::embedded(),
        Duration::from_secs(60),
    );
    // Scenario runs feed `rows_from_events` — they need the full
    // decision context, which the audit trail deliberately redacts.
    engine.set_trace_mode(dexter_engine::TraceMode::Training);
    for fp in &spec.task.grants {
        engine.grant_approval(fp);
    }
    if let Some(p) = journal_path {
        // Presence is best-effort — a journal that can't open doesn't
        // fail the rep.
        let _ = engine.set_journal_sink(p);
    }
    // Sequential goals: "ir a cronómetro e iniciar" runs as two subgoals,
    // each through the same closed loop. The task-level done_when belongs
    // to the LAST subgoal; earlier ones auto-complete on verified change.
    let parts = dexter_decision::split_goal(&spec.scenario.goal);
    let last = parts.len() - 1;
    let subgoals: Vec<dexter_engine::Subgoal> = parts
        .iter()
        .enumerate()
        .map(|(i, g)| dexter_engine::Subgoal {
            goal: g.clone(),
            done_when: (i == last).then(|| spec.task.done_when.clone()),
        })
        .collect();
    let outcome = engine.run_plan(
        &subgoals,
        generator,
        decider,
        &TaskConfig {
            run: RunConfig {
                // Live-app scenarios scope the observation to the
                // declared app; sim/browser see everything.
                app: spec.live.as_ref().map(|l| AppSelector::parse(&l.app)),
                max_attempts: 1,
                verify_delay: Duration::from_millis(5),
                // Live apps propagate AX state asynchronously — reuse the
                // [live] settle after each act so done_when doesn't judge
                // a stale world.
                post_act_settle: spec
                    .live
                    .as_ref()
                    .map(|l| Duration::from_millis(l.settle_ms))
                    .unwrap_or(Duration::ZERO),
                allow_coordinates: false,
                // The suite author is the operator approving the run —
                // same contract as `dexter run --approve-all`. Approval
                // events still journal, so `approvals` stays honest.
                approve_all: true,
                observe_max_elements: 4_000,
            },
            max_steps: spec.task.max_steps,
            max_duration: spec.task.max_secs.map(Duration::from_secs),
            cancel: None,
            done_when: spec.task.done_when.clone(),
        },
    );

    let (outcome, steps) = match &outcome {
        PlanOutcome::Completed { steps, .. } => ("completed".to_string(), *steps),
        PlanOutcome::Failed { inner, .. } => match inner.as_ref() {
            TaskOutcome::Completed { steps } => ("completed".to_string(), *steps),
            TaskOutcome::Abstained { .. } => ("abstained".to_string(), 0),
            TaskOutcome::Escalated { .. } => ("escalated".to_string(), 0),
            TaskOutcome::NeedsApproval { .. } => ("needs_approval".to_string(), 0),
            TaskOutcome::Denied { .. } => ("denied".to_string(), 0),
            TaskOutcome::Failed { .. } => ("failed".to_string(), 0),
            TaskOutcome::MaxSteps => ("max_steps".to_string(), spec.task.max_steps),
            TaskOutcome::Cancelled => ("cancelled".to_string(), 0),
            TaskOutcome::TimedOut { .. } => ("timed_out".to_string(), 0),
        },
    };
    let mut run = ScenarioRun {
        success: false, // set after journal metrics land
        outcome: outcome.clone(),
        steps,
        elapsed_ms: 0,
        decide_ms: Vec::new(),
        gen_ms: Vec::new(),
        act_ms: Vec::new(),
        recoveries: 0,
        verify_fails: 0,
        action_failures: 0,
        approvals: 0,
        physical_acts: 0,
        events: Vec::new(),
    };
    let events = engine.events();
    measure(&events, &mut run);
    run.success = outcome == spec.task.expected
        && spec
            .task
            .expect_verify_fails
            .is_none_or(|min| run.verify_fails >= min as usize);
    run.events = events;
    run
}

/// Derive metrics from the event stream. The loop is single-threaded so
/// events are strictly ordered — phase latency is the delta between
/// consecutive journal timestamps within a step.
fn measure(events: &[dexter_core::Event], run: &mut ScenarioRun) {
    use dexter_core::EventKind::*;
    let mut last_obs: Option<std::time::SystemTime> = None;
    let mut last_candidates: Option<std::time::SystemTime> = None;
    let mut last_decision: Option<std::time::SystemTime> = None;
    for ev in events {
        let delta = |a: Option<std::time::SystemTime>| {
            a.and_then(|t| ev.ts.duration_since(t).ok())
                .map(|d| d.as_millis() as u64)
        };
        match ev.kind {
            ObservationCreated => last_obs = ev.ts.into(),
            CandidatesGenerated => {
                run.gen_ms.push(delta(last_obs).unwrap_or(0));
                last_candidates = ev.ts.into();
            }
            DecisionMade => {
                run.decide_ms.push(delta(last_candidates).unwrap_or(0));
                last_decision = ev.ts.into();
            }
            ActionExecuted => {
                run.act_ms.push(delta(last_decision).unwrap_or(0));
                if ev.data.get("mechanism").and_then(|m| m.as_str()) == Some("Coordinates") {
                    run.physical_acts += 1;
                }
            }
            RecoveryStarted => run.recoveries += 1,
            VerificationFailed => run.verify_fails += 1,
            ActionFailed => run.action_failures += 1,
            HumanApprovalRequired => run.approvals += 1,
            TaskCompleted | TaskFailed | TaskCancelled | TaskTimedOut => {
                if let Some(ms) = ev.data.get("elapsed_ms").and_then(|v| v.as_u64()) {
                    run.elapsed_ms = ms;
                }
            }
            _ => {}
        }
    }
}

// ----- training-row export ----------------------------------------------

/// One Laya training row — the same shape `eval export` emits, with the
/// scenario provenance fields appended.
#[derive(Debug, Serialize)]
pub struct ScenarioRow {
    /// `"<scenario>#step<N>"`.
    pub id: String,
    /// Provenance group (leave-one-app-out filters key on this).
    pub app: String,
    /// Always `"scenario"` — lets a merged dataset keep its origin.
    pub source: &'static str,
    /// 1-based step within the task — sequential context the frozen
    /// datasets never carry.
    pub step: u32,
    /// `[GOAL]/[WORLD_STATE]/[LAST_ERROR]` — exactly what Laya sees.
    pub state: String,
    /// Candidate + route option texts, inference-identical.
    pub options: Vec<String>,
    pub n_candidates: usize,
    /// Absolute option index (candidates first, then route options).
    pub gold_index: Option<usize>,
    pub gold_route: Option<&'static str>,
}

/// Rebuild training rows from a run's journal: pair each
/// `CandidatesGenerated` context with the following `DecisionMade`, and
/// label the row with the decision the engine took. Callers decide which
/// runs qualify — exporting a failed trajectory mislabels it.
///
/// Returns (rows, skipped): a row is skipped when the engine invented an
/// action not among the offered candidates (`candidate_index: None`) —
/// no honest label exists for it.
pub fn rows_from_events(
    events: &[Event],
    scenario_id: &str,
    app: &str,
) -> (Vec<ScenarioRow>, usize) {
    let mut rows = Vec::new();
    let mut skipped = 0;
    let mut last_ctx: Option<DecisionContext> = None;
    for ev in events {
        match ev.kind {
            dexter_core::EventKind::CandidatesGenerated => {
                last_ctx = ev
                    .data
                    .get("context")
                    .and_then(|v| serde_json::from_value(v.clone()).ok());
            }
            dexter_core::EventKind::DecisionMade => {
                let (Some(ctx), Some(decision)) = (
                    last_ctx.as_ref(),
                    ev.data
                        .get("decision")
                        .and_then(|v| serde_json::from_value::<Decision>(v.clone()).ok()),
                ) else {
                    continue;
                };
                let n_cands = ctx.candidates.len();
                let (gold_index, gold_route) = match &decision {
                    Decision::Act {
                        candidate_index, ..
                    } => match candidate_index {
                        Some(i) if *i < n_cands => (Some(*i), None),
                        _ => {
                            skipped += 1;
                            continue;
                        }
                    },
                    Decision::Route { route, .. } => {
                        let variant = crate::route_variant(route);
                        let slot = dexter_laya::ROUTE_VARIANT_ORDER
                            .iter()
                            .position(|v| *v == variant);
                        match slot {
                            Some(s) => (Some(n_cands + s), Some(variant)),
                            None => {
                                skipped += 1;
                                continue;
                            }
                        }
                    }
                };
                let (state, q) = dexter_laya::build_question(ctx);
                let options = match q {
                    dexter_decision::Question::Choice { options, .. } => options,
                    _ => continue,
                };
                rows.push(ScenarioRow {
                    id: format!("{scenario_id}#step{}", ctx.step),
                    app: app.to_string(),
                    source: "scenario",
                    step: ctx.step,
                    state,
                    options,
                    n_candidates: n_cands,
                    gold_index,
                    gold_route,
                });
            }
            _ => {}
        }
    }
    (rows, skipped)
}

// ----- aggregation -----------------------------------------------------

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn stats(values: &[u64]) -> (f64, u64, u64) {
    if values.is_empty() {
        return (0.0, 0, 0);
    }
    let mut s = values.to_vec();
    s.sort_unstable();
    let mean = s.iter().sum::<u64>() as f64 / s.len() as f64;
    (mean, percentile(&s, 0.50), percentile(&s, 0.95))
}

/// One scenario's metrics aggregated over `reps` runs.
#[derive(Debug, Serialize)]
pub struct ScenarioMetrics {
    pub id: String,
    pub reps: usize,
    /// Runs whose outcome matched `task.expected`.
    pub succeeded: usize,
    pub outcomes: std::collections::BTreeMap<String, usize>,
    pub mean_steps: f64,
    /// Mean of (steps − optimal_steps) over successful reps. `None` when
    /// the scenario declares no `optimal_steps` or none succeeded.
    pub mean_over_optimal: Option<f64>,
    pub mean_elapsed_ms: f64,
    /// Engine latency percentiles across all steps of all reps.
    pub decide_p50_ms: u64,
    pub decide_p95_ms: u64,
    pub recoveries: usize,
    pub verify_fails: usize,
    pub action_failures: usize,
    pub approvals: usize,
    pub physical_acts: usize,
}

/// Aggregate `reps` of one scenario.
pub fn aggregate(id: &str, optimal_steps: Option<u32>, runs: Vec<ScenarioRun>) -> ScenarioMetrics {
    let reps = runs.len();
    let succeeded = runs.iter().filter(|r| r.success).count();
    let mut outcomes = std::collections::BTreeMap::new();
    let mut steps = Vec::new();
    let mut over = Vec::new();
    let mut elapsed = Vec::new();
    let mut decide = Vec::new();
    let mut m = ScenarioMetrics {
        id: id.to_string(),
        reps,
        succeeded,
        outcomes: std::collections::BTreeMap::new(),
        mean_steps: 0.0,
        mean_over_optimal: None,
        mean_elapsed_ms: 0.0,
        decide_p50_ms: 0,
        decide_p95_ms: 0,
        recoveries: 0,
        verify_fails: 0,
        action_failures: 0,
        approvals: 0,
        physical_acts: 0,
    };
    for r in &runs {
        *outcomes.entry(r.outcome.clone()).or_insert(0) += 1;
        steps.push(r.steps as u64);
        elapsed.push(r.elapsed_ms);
        decide.extend_from_slice(&r.decide_ms);
        m.recoveries += r.recoveries;
        m.verify_fails += r.verify_fails;
        m.action_failures += r.action_failures;
        m.approvals += r.approvals;
        m.physical_acts += r.physical_acts;
        if r.success {
            if let Some(opt) = optimal_steps {
                over.push(r.steps.saturating_sub(opt) as u64);
            }
        }
    }
    m.outcomes = outcomes;
    m.mean_steps = stats(&steps).0;
    m.mean_elapsed_ms = stats(&elapsed).0;
    if !over.is_empty() {
        m.mean_over_optimal = Some(stats(&over).0);
    }
    let (_, p50, p95) = stats(&decide);
    m.decide_p50_ms = p50;
    m.decide_p95_ms = p95;
    m
}

/// Suite rollup across scenarios.
#[derive(Debug, Serialize)]
pub struct SuiteMetrics {
    pub scenarios: usize,
    pub reps: usize,
    pub succeeded: usize,
    pub success_rate: f64,
    /// Worst decide latency percentile seen across the suite.
    pub decide_p95_ms: u64,
    pub physical_acts: usize,
}

pub fn suite_rollup(metrics: &[ScenarioMetrics]) -> SuiteMetrics {
    let succeeded: usize = metrics.iter().map(|m| m.succeeded).sum();
    let reps: usize = metrics.iter().map(|m| m.reps).sum();
    SuiteMetrics {
        scenarios: metrics.len(),
        reps,
        succeeded,
        success_rate: if reps == 0 {
            0.0
        } else {
            succeeded as f64 / reps as f64
        },
        decide_p95_ms: metrics.iter().map(|m| m.decide_p95_ms).max().unwrap_or(0),
        physical_acts: metrics.iter().map(|m| m.physical_acts).sum(),
    }
}

// ----- baseline gate ----------------------------------------------------

/// `baseline.toml` — the committed expectation the suite must not regress.
#[derive(Debug, Deserialize)]
pub struct Baseline {
    /// Per-scenario bounds, keyed by scenario id.
    #[serde(default)]
    pub scenario: std::collections::BTreeMap<String, ScenarioBounds>,
    /// Suite-level bounds.
    #[serde(default)]
    pub suite: Option<SuiteBounds>,
}

#[derive(Debug, Deserialize)]
pub struct ScenarioBounds {
    /// Minimum fraction of reps that must succeed.
    pub min_success: Option<f64>,
    /// Mean steps must stay at or under this.
    pub max_mean_steps: Option<f64>,
    /// Engine p95 latency bound, ms.
    pub max_decide_p95_ms: Option<u64>,
    /// Physical-tier acts must stay at or under this (usually 0).
    pub max_physical_acts: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct SuiteBounds {
    pub min_success_rate: Option<f64>,
    pub max_decide_p95_ms: Option<u64>,
    pub max_physical_acts: Option<usize>,
}

/// One way a run regressed vs the baseline.
#[derive(Debug)]
pub struct Violation {
    pub scenario: String,
    pub message: String,
}

/// Compare measured metrics against the baseline; empty = pass.
/// A scenario named in the baseline but absent from the results is a
/// violation — a skipped or renamed-away spec must not pass silently.
pub fn check_baseline(metrics: &[ScenarioMetrics], base: &Baseline) -> Vec<Violation> {
    let mut out = Vec::new();
    for id in base.scenario.keys() {
        if !metrics.iter().any(|m| m.id == *id) {
            out.push(Violation {
                scenario: id.clone(),
                message: "missing from results (skipped or deleted)".into(),
            });
        }
    }
    for m in metrics {
        let Some(b) = base.scenario.get(&m.id) else {
            continue;
        };
        let rate = if m.reps == 0 {
            0.0
        } else {
            m.succeeded as f64 / m.reps as f64
        };
        if let Some(min) = b.min_success {
            if rate < min {
                out.push(Violation {
                    scenario: m.id.clone(),
                    message: format!("success {rate:.2} < baseline {min:.2}"),
                });
            }
        }
        if let Some(max) = b.max_mean_steps {
            if m.mean_steps > max {
                out.push(Violation {
                    scenario: m.id.clone(),
                    message: format!("mean steps {:.1} > baseline {max:.1}", m.mean_steps),
                });
            }
        }
        if let Some(max) = b.max_decide_p95_ms {
            if m.decide_p95_ms > max {
                out.push(Violation {
                    scenario: m.id.clone(),
                    message: format!("decide p95 {}ms > baseline {max}ms", m.decide_p95_ms),
                });
            }
        }
        if let Some(max) = b.max_physical_acts {
            if m.physical_acts > max {
                out.push(Violation {
                    scenario: m.id.clone(),
                    message: format!("physical acts {} > baseline {max}", m.physical_acts),
                });
            }
        }
    }
    if let Some(suite) = &base.suite {
        let roll = suite_rollup(metrics);
        if let Some(min) = suite.min_success_rate {
            if roll.success_rate < min {
                out.push(Violation {
                    scenario: "suite".into(),
                    message: format!("success rate {:.2} < baseline {min:.2}", roll.success_rate),
                });
            }
        }
        if let Some(max) = suite.max_decide_p95_ms {
            if roll.decide_p95_ms > max {
                out.push(Violation {
                    scenario: "suite".into(),
                    message: format!("decide p95 {}ms > baseline {max}ms", roll.decide_p95_ms),
                });
            }
        }
        if let Some(max) = suite.max_physical_acts {
            if roll.physical_acts > max {
                out.push(Violation {
                    scenario: "suite".into(),
                    message: format!("physical acts {} > baseline {max}", roll.physical_acts),
                });
            }
        }
    }
    out
}
