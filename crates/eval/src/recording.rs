use crate::scenario::{self, ScenarioRun, ScenarioSpec};
use dexter_core::{
    Action, AppSelector, Event, EventKind, ExpectedState, Observation, ObservationScope,
    VerificationStatus,
};
use dexter_driver::ComputerDriver;
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_policy::Policy;
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordedStep {
    pub action: Action,
    pub app: Option<AppSelector>,
    pub observes_before: u32,
    pub result: StepResult,
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
    pub done_when: VerificationStatus,
}

impl ReplayReport {
    pub fn faithful(&self, rec: &Recording) -> bool {
        self.diverged_at.is_none()
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
            open = Some(OpenStep {
                action,
                app,
                observes_before: std::mem::take(&mut observes_since_terminal),
            });
            trailing_observes = 0;
            continue;
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
    let mut engine = Engine::new(driver, Policy::embedded(), Duration::from_secs(60));
    for grant in grants {
        engine.pre_grant_approval(grant);
    }
    let cfg: RunConfig = scenario::sim_run_config();
    let scope = ObservationScope {
        app: None,
        max_elements: 4_000,
        ..Default::default()
    };
    let mut replayed = Vec::new();
    let mut diverged_at = None;

    'steps: for (index, recorded) in rec.steps.iter().enumerate() {
        for _ in 0..recorded.observes_before {
            if engine.driver().observe(&scope).is_err() {
                diverged_at = Some(index);
                break 'steps;
            }
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
        let result = step_result(status);
        replayed.push(result);
        if result != recorded.result {
            diverged_at = Some(index);
            break;
        }
    }

    let mut last_observation: Option<Observation> = None;
    let mut final_observe_failed = false;
    for _ in 0..rec.trailing_observes.max(1) {
        match engine.driver().observe(&scope) {
            Ok(observation) => last_observation = Some(observation),
            Err(_) => {
                final_observe_failed = true;
                if diverged_at.is_none() {
                    diverged_at = Some(rec.steps.len());
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

    ReplayReport {
        replayed,
        diverged_at,
        done_when,
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
