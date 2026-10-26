use crate::scenario::{self, ScenarioRun, ScenarioSpec};
use dexter_core::{
    Action, AppSelector, Event, EventKind, ExpectedState, Observation, ObservationScope,
    VerificationStatus,
};
use dexter_driver::ComputerDriver;
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_policy::{fingerprint, ActionContext, Policy};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Recording {
    pub scenario: String,
    pub goal: String,
    pub outcome: String,
    pub done_when: ExpectedState,
    pub steps: Vec<RecordedStep>,
    pub trailing_observes: u32,
    #[serde(default)]
    pub driver: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedStep {
    pub action: Action,
    pub app: Option<AppSelector>,
    pub observes_before: u32,
    pub result: StepResult,
    #[serde(default)]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub granted: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ReplayOptions {
    pub grants: Vec<String>,
    pub honor_recorded_grants: bool,
    pub allow_coordinates: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Divergence {
    pub index: usize,
    pub recorded: Option<StepResult>,
    pub replayed: Option<StepResult>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepResult {
    Done,
    Failed,
    Denied,
    NeedsApproval,
    Errored,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("journal incomplete: {0}")]
    Incomplete(String),
    #[error("journal malformed: {0}")]
    Malformed(String),
}

#[derive(Debug)]
pub struct ReplayReport {
    pub replayed: Vec<StepResult>,
    pub diverged_at: Option<usize>,
    pub divergence: Option<Divergence>,
    pub done_when: VerificationStatus,
}

impl ReplayReport {
    pub fn faithful(&self, rec: &Recording) -> bool {
        self.diverged_at.is_none()
            && self.divergence.is_none()
            && self.replayed.len() == rec.steps.len()
            && if rec.outcome == "completed" {
                self.done_when == VerificationStatus::Verified
            } else {
                self.done_when != VerificationStatus::Verified
            }
    }
}

struct OpenStep {
    action: Action,
    app: Option<AppSelector>,
    observes_before: u32,
    fingerprint: Option<String>,
    approved: bool,
    approve_all: bool,
}

pub fn record(
    scenario: &str,
    goal: &str,
    outcome: &str,
    done_when: &ExpectedState,
    events: &[Event],
) -> Result<Recording, RecordError> {
    let mut steps = Vec::new();
    let mut open: Option<OpenStep> = None;
    let mut observes_since_terminal = 0u32;
    let mut trailing_observes = 0u32;
    let mut driver: Option<String> = None;

    for event in events {
        if event.kind == EventKind::ObservationCreated && event.data.get("step").is_some() {
            observes_since_terminal = observes_since_terminal.checked_add(1).ok_or_else(|| {
                RecordError::Malformed("loop observation count exceeds u32".into())
            })?;
            trailing_observes = observes_since_terminal;
        }

        if event.kind == EventKind::ActionProposed {
            if open.is_some() {
                return Err(RecordError::Malformed(
                    "ActionProposed occurred before the previous step terminated".into(),
                ));
            }
            let action = event
                .data
                .get("action")
                .cloned()
                .ok_or_else(|| RecordError::Malformed("ActionProposed has no action".into()))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|err| {
                        RecordError::Malformed(format!("invalid proposed action: {err}"))
                    })
                })?;
            let app = event
                .data
                .get("app")
                .cloned()
                .ok_or_else(|| RecordError::Malformed("ActionProposed has no app".into()))
                .and_then(|value| {
                    serde_json::from_value(value).map_err(|err| {
                        RecordError::Malformed(format!("invalid proposed app: {err}"))
                    })
                })?;
            let event_driver = match event.data.get("driver") {
                None | Some(serde_json::Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| {
                            RecordError::Malformed("ActionProposed driver is not a string".into())
                        })?
                        .to_string(),
                ),
            };
            if let Some(event_driver) = event_driver {
                if let Some(recorded_driver) = &driver {
                    if recorded_driver != &event_driver {
                        return Err(RecordError::Malformed(format!(
                            "journal mixes drivers {recorded_driver} and {event_driver}"
                        )));
                    }
                } else {
                    driver = Some(event_driver);
                }
            }
            let fingerprint = match event.data.get("fingerprint") {
                None | Some(serde_json::Value::Null) => None,
                Some(value) => Some(
                    value
                        .as_str()
                        .ok_or_else(|| {
                            RecordError::Malformed(
                                "ActionProposed fingerprint is not a string".into(),
                            )
                        })?
                        .to_string(),
                ),
            };
            open = Some(OpenStep {
                action,
                app,
                observes_before: std::mem::take(&mut observes_since_terminal),
                fingerprint,
                approved: false,
                approve_all: false,
            });
            trailing_observes = 0;
            continue;
        }

        if let Some(step) = open.as_mut() {
            if event.kind == EventKind::PolicyChecked
                && event.data.get("decision").and_then(|v| v.as_str()) == Some("approved")
                && event.data.get("fingerprint").and_then(|v| v.as_str())
                    == step.fingerprint.as_deref()
            {
                step.approved = true;
            }
            if event.kind == EventKind::HumanApprovalRequired
                && event.data.get("granted").and_then(|v| v.as_str()) == Some("approve_all")
            {
                step.approve_all = true;
            }
        }

        let Some(result) = terminal_result(event)? else {
            continue;
        };
        let Some(step) = open.take() else {
            return Err(RecordError::Malformed(format!(
                "terminal {:?} event has no open action",
                event.kind
            )));
        };
        steps.push(RecordedStep {
            action: step.action,
            app: step.app,
            observes_before: step.observes_before,
            result,
            granted: step.fingerprint.is_some() && step.approved && !step.approve_all,
            fingerprint: step.fingerprint,
        });
        observes_since_terminal = 0;
        trailing_observes = 0;
    }

    if open.is_some() {
        return Err(RecordError::Incomplete(format!(
            "step {} has no terminal event",
            steps.len() + 1
        )));
    }

    Ok(Recording {
        scenario: scenario.to_string(),
        goal: goal.to_string(),
        outcome: outcome.to_string(),
        done_when: done_when.clone(),
        steps,
        trailing_observes,
        driver,
    })
}

pub fn record_run(spec: &ScenarioSpec, run: &ScenarioRun) -> Result<Recording, RecordError> {
    if run.journal_dropped > 0 {
        return Err(RecordError::Incomplete(format!(
            "{} journal events were dropped",
            run.journal_dropped
        )));
    }
    record(
        &spec.scenario.id,
        &spec.scenario.goal,
        &run.outcome,
        &spec.task.done_when,
        &run.events,
    )
}

pub fn events_from_jsonl(text: &str) -> Result<Vec<Event>, RecordError> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|err| {
                RecordError::Malformed(format!("invalid JSONL event on line {}: {err}", index + 1))
            })
        })
        .collect()
}

/// The task-level outcome a journal ended with — the last terminal task
/// event wins. `None` when the journal has no terminal: a truncated or
/// in-flight trace has no outcome and must not be recorded as one.
///
/// `run_plan` journals `SubgoalFailed` with the inner `TaskOutcome`'s
/// Debug name when a subgoal ends the plan; `run_goal`/`run_scenario`
/// journal `TaskCompleted`/`TaskFailed`/`TaskCancelled`/`TaskTimedOut`
/// directly. Both surfaces map onto the same outcome vocabulary
/// `ScenarioRun` uses ("completed", "abstained", "escalated", "failed",
/// "max_steps", "cancelled", "timed_out").
pub fn outcome_from_events(events: &[Event]) -> Option<String> {
    let mut outcome = None;
    for event in events {
        let mapped = match event.kind {
            EventKind::TaskCompleted | EventKind::SubgoalCompleted => Some("completed"),
            EventKind::TaskCancelled => Some("cancelled"),
            EventKind::TaskTimedOut => Some("timed_out"),
            EventKind::TaskFailed => match event.data.get("outcome").and_then(|v| v.as_str()) {
                Some("abstain") => Some("abstained"),
                Some("escalated") => Some("escalated"),
                _ => Some("failed"),
            },
            EventKind::SubgoalFailed => {
                match event.data.get("outcome").and_then(|v| v.as_str()) {
                    Some(debug) if debug.starts_with("Completed") => Some("completed"),
                    Some(debug) if debug.starts_with("Abstained") => Some("abstained"),
                    Some(debug) if debug.starts_with("Escalated") => Some("escalated"),
                    Some(debug) if debug.starts_with("Failed") => Some("failed"),
                    Some(debug) if debug.starts_with("MaxSteps") => Some("max_steps"),
                    Some(debug) if debug.starts_with("Cancelled") => Some("cancelled"),
                    Some(debug) if debug.starts_with("TimedOut") => Some("timed_out"),
                    // An unrecognized Debug payload never becomes a guess.
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(mapped) = mapped {
            outcome = Some(mapped.to_string());
        }
    }
    outcome
}

fn terminal_result(event: &Event) -> Result<Option<StepResult>, RecordError> {
    match event.kind {
        EventKind::PolicyChecked
            if event.data.get("decision").and_then(|v| v.as_str()) == Some("deny") =>
        {
            Ok(Some(StepResult::Denied))
        }
        EventKind::HumanApprovalRequired if event.data.get("granted").is_none() => {
            Ok(Some(StepResult::NeedsApproval))
        }
        EventKind::ActionFailed => Ok(Some(StepResult::Errored)),
        EventKind::ActionExecuted => {
            let status = event
                .data
                .get("status")
                .and_then(|value| value.as_str())
                .ok_or_else(|| {
                    RecordError::Malformed("ActionExecuted has no string status".into())
                })?;
            match status {
                "Success" => Ok(Some(StepResult::Done)),
                "ForegroundRequired" | "Unsupported" | "PermissionDenied" | "Timeout"
                | "Failed" => Ok(Some(StepResult::Failed)),
                other => Err(RecordError::Malformed(format!(
                    "ActionExecuted has unknown status {other:?}"
                ))),
            }
        }
        _ => Ok(None),
    }
}

pub fn replay<D: ComputerDriver>(rec: &Recording, driver: D, grants: &[String]) -> ReplayReport {
    let options = ReplayOptions {
        grants: grants.to_vec(),
        ..Default::default()
    };
    replay_with(
        rec,
        driver,
        Policy::embedded(),
        scenario::sim_run_config(),
        &options,
    )
}

/// `replay` with the policy and run config explicit — the sim surface
/// delegates with the embedded policy and the sim `RunConfig` (which
/// `approve_all`s like the recording runs did). Live callers set their
/// own: a driver replay on the real desktop grants nothing implicitly.
pub fn replay_with<D: ComputerDriver>(
    rec: &Recording,
    driver: D,
    policy: Policy,
    cfg: RunConfig,
    options: &ReplayOptions,
) -> ReplayReport {
    let driver_name = driver.capabilities().name;
    let mismatch = match rec.driver.as_deref() {
        Some(recorded) if recorded != driver_name => Some(format!(
            "driver mismatch: recorded by {recorded}, replaying on {driver_name}"
        )),
        None if driver_name != "sim" => Some(
            "recording has no driver identity; live replay needs a recording made from a driver journal"
                .to_string(),
        ),
        _ => None,
    };
    if let Some(reason) = mismatch {
        let divergence = Divergence {
            index: 0,
            recorded: rec.steps.first().map(|step| step.result),
            replayed: None,
            reason,
        };
        return ReplayReport {
            replayed: Vec::new(),
            diverged_at: Some(0),
            divergence: Some(divergence),
            done_when: VerificationStatus::Uncertain,
        };
    }

    let mut cfg = cfg;
    cfg.allow_coordinates = options.allow_coordinates;
    let mut engine = Engine::new(driver, policy, Duration::from_secs(60));
    for grant in &options.grants {
        engine.pre_grant_approval(grant);
    }
    let scope = ObservationScope {
        app: None,
        max_elements: 4_000,
        ..Default::default()
    };
    let mut replayed = Vec::new();
    let mut divergence = None;

    'steps: for (index, recorded) in rec.steps.iter().enumerate() {
        let recorded_grant = if options.honor_recorded_grants && recorded.granted {
            let fingerprint = fingerprint(
                &recorded.action,
                &ActionContext {
                    app: recorded.app.clone().or(cfg.app.clone()),
                    target_hint: None,
                },
            );
            if recorded.fingerprint.as_deref() != Some(fingerprint.as_str()) {
                divergence = Some(Divergence {
                    index,
                    recorded: Some(recorded.result),
                    replayed: None,
                    reason: "recorded grant fingerprint does not match replayed step".into(),
                });
                break;
            }
            Some(fingerprint)
        } else {
            None
        };

        for _ in 0..recorded.observes_before {
            if let Err(err) = engine.driver().observe(&scope) {
                divergence = Some(Divergence {
                    index,
                    recorded: Some(recorded.result),
                    replayed: None,
                    reason: format!("observe failed before step: {err}"),
                });
                break 'steps;
            }
        }
        if let Some(fingerprint) = recorded_grant.as_deref() {
            engine.pre_grant_approval(fingerprint);
        }
        let status = engine.run_step(
            &Step {
                note: None,
                action: recorded.action.clone(),
                expect: None,
                max_attempts: Some(1),
                app: recorded.app.clone(),
            },
            &cfg,
        );
        if let Some(fingerprint) = recorded_grant.as_deref() {
            engine.revoke_pre_grant(fingerprint);
        }
        let reason = step_status_reason(&status);
        let result = step_result(status);
        replayed.push(result);
        if result != recorded.result {
            divergence = Some(Divergence {
                index,
                recorded: Some(recorded.result),
                replayed: Some(result),
                reason,
            });
            break;
        }
    }

    let final_scope = ObservationScope {
        app: rec
            .steps
            .last()
            .and_then(|step| step.app.clone())
            .or(cfg.app.clone()),
        max_elements: 4_000,
        ..Default::default()
    };
    let mut last_observation: Option<Observation> = None;
    let mut final_observe_failed = false;
    for _ in 0..rec.trailing_observes.max(1) {
        match engine.driver().observe(&final_scope) {
            Ok(observation) => last_observation = Some(observation),
            Err(err) => {
                final_observe_failed = true;
                if divergence.is_none() {
                    divergence = Some(Divergence {
                        index: rec.steps.len(),
                        recorded: None,
                        replayed: None,
                        reason: format!("final observe failed: {err}"),
                    });
                }
                break;
            }
        }
    }
    let done_when = if final_observe_failed {
        VerificationStatus::Uncertain
    } else {
        last_observation
            .as_ref()
            .map(|observation| dexter_verify::verify(observation, &rec.done_when).status)
            .unwrap_or(VerificationStatus::Uncertain)
    };

    let diverged_at = divergence.as_ref().map(|divergence| divergence.index);
    ReplayReport {
        replayed,
        diverged_at,
        divergence,
        done_when,
    }
}

fn step_status_reason(status: &StepStatus) -> String {
    match status {
        StepStatus::Done { .. } => "step completed".into(),
        StepStatus::Failed { reason, .. } | StepStatus::Denied { reason } => reason.clone(),
        StepStatus::NeedsApproval {
            fingerprint,
            reason,
        } => format!("{fingerprint}: {reason}"),
        StepStatus::Errored { error } => error.to_string(),
    }
}

fn step_result(status: StepStatus) -> StepResult {
    match status {
        StepStatus::Done { .. } => StepResult::Done,
        StepStatus::Failed { .. } => StepResult::Failed,
        StepStatus::Denied { .. } => StepResult::Denied,
        StepStatus::NeedsApproval { .. } => StepResult::NeedsApproval,
        StepStatus::Errored { .. } => StepResult::Errored,
    }
}
