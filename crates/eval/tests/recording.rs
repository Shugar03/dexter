use dexter_core::{
    Action, ActionResult, AppSelector, Element, ElementId, Event, EventKind, ExpectedState,
    KeyChord, MouseButton, ObservationScope, SemanticTarget, Target, VerificationStatus, Window,
};
use dexter_decision::{HeuristicGenerator, RuleBased};
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
use dexter_engine::RunConfig;
use dexter_eval::{
    recording::{
        events_from_jsonl, outcome_from_events, record, record_run, replay, replay_with,
        RecordError, RecordedStep, Recording, ReplayOptions, StepResult,
    },
    scenario::{build_driver, run_scenario, ScenarioRun, ScenarioSpec},
};
use dexter_policy::{ActionContext, Policy};
use dexter_sim::SimDriver;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

fn scenario_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/scenarios")
}

fn recording_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../datasets/recordings")
}

fn sim_scenarios() -> Vec<(PathBuf, ScenarioSpec)> {
    let mut paths: Vec<_> = std::fs::read_dir(scenario_dir())
        .expect("scenario dataset exists")
        .map(|entry| entry.expect("scenario entry").path())
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("toml")
                && path.file_name().and_then(|name| name.to_str()) != Some("baseline.toml")
        })
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(&path).expect("read scenario");
            let spec: ScenarioSpec =
                toml::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()));
            (spec.driver() == "sim").then_some((path, spec))
        })
        .collect()
}

fn read_spec(id: &str) -> ScenarioSpec {
    let path = scenario_dir().join(format!("{id}.toml"));
    toml::from_str(&std::fs::read_to_string(&path).expect("read scenario"))
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn run(spec: &ScenarioSpec) -> ScenarioRun {
    run_scenario(spec, &HeuristicGenerator::default(), &RuleBased::default())
}

fn proposal(action: &Action) -> Event {
    Event::new(
        EventKind::ActionProposed,
        json!({"action": action, "app": null}),
    )
}

fn proposal_with_identity(action: &Action, driver: &str, fingerprint: Option<&str>) -> Event {
    let mut data = json!({"action": action, "app": null, "driver": driver});
    if let Some(fingerprint) = fingerprint {
        data["fingerprint"] = json!(fingerprint);
    }
    Event::new(EventKind::ActionProposed, data)
}

struct CountingDriver {
    inner: SimDriver,
    name: &'static str,
    act_calls: Arc<AtomicUsize>,
    observe_calls: Arc<AtomicUsize>,
    fail_observe_at: Option<usize>,
}

impl CountingDriver {
    fn new(
        name: &'static str,
        elements: Vec<Element>,
        fail_observe_at: Option<usize>,
    ) -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let act_calls = Arc::new(AtomicUsize::new(0));
        let observe_calls = Arc::new(AtomicUsize::new(0));
        (
            Self {
                inner: SimDriver::new(elements),
                name,
                act_calls: act_calls.clone(),
                observe_calls: observe_calls.clone(),
                fail_observe_at,
            },
            act_calls,
            observe_calls,
        )
    }
}

impl ComputerDriver for CountingDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: self.name,
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        self.inner.windows()
    }

    fn observe(&self, scope: &ObservationScope) -> Result<dexter_core::Observation, DriverError> {
        let call = self.observe_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_observe_at == Some(call) {
            return Err(DriverError::Platform(format!("observe call {call} failed")));
        }
        self.inner.observe(scope)
    }

    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.act_calls.fetch_add(1, Ordering::SeqCst);
        self.inner.act(action, ctx)
    }
}

fn one_step_recording(
    driver: Option<&str>,
    action: Action,
    result: StepResult,
    fingerprint: Option<String>,
    granted: bool,
) -> Recording {
    Recording {
        scenario: "replay-test".into(),
        goal: "exercise replay".into(),
        outcome: "failed".into(),
        done_when: never_present(),
        driver: driver.map(str::to_string),
        steps: vec![RecordedStep {
            action,
            app: None,
            observes_before: 0,
            result,
            fingerprint,
            granted,
        }],
        trailing_observes: 0,
    }
}

fn is_terminal(event: &Event) -> bool {
    match event.kind {
        EventKind::PolicyChecked => event.data["decision"] == "deny",
        EventKind::HumanApprovalRequired => event.data.get("granted").is_none(),
        EventKind::ActionFailed | EventKind::ActionExecuted => true,
        _ => false,
    }
}

fn never_present() -> ExpectedState {
    ExpectedState::ElementExists {
        target: SemanticTarget {
            name: Some("recording-test-absent".into()),
            ..Default::default()
        },
    }
}

#[test]
fn every_sim_scenario_records_and_replays_faithfully() {
    let mut had_multi_step = false;
    let mut download_had_waited_observes = false;
    let scenarios = sim_scenarios();
    assert!(!scenarios.is_empty(), "sim scenario dataset is present");

    for (_, spec) in &scenarios {
        let run = run(spec);
        let recording =
            record_run(spec, &run).unwrap_or_else(|err| panic!("{}: {err}", spec.scenario.id));
        let encoded = serde_json::to_string(&recording).expect("recording serializes");
        let decoded: Recording = serde_json::from_str(&encoded).expect("recording parses");
        assert_eq!(recording, decoded, "{} JSON round-trip", spec.scenario.id);

        let report = replay(&recording, build_driver(spec), &spec.task.grants);
        assert!(
            report.faithful(&recording),
            "{} replay diverged: {report:?}",
            spec.scenario.id
        );
        had_multi_step |= recording.steps.len() > 1;
        if spec.scenario.id == "download-wait" {
            download_had_waited_observes =
                recording.steps.is_empty() && recording.trailing_observes > 1;
        }
    }

    assert!(
        had_multi_step,
        "at least one recording must have multiple steps"
    );
    assert!(
        download_had_waited_observes,
        "download-wait's route-wait path must preserve multiple trailing loop observes"
    );
}

#[test]
fn committed_fixtures_replay_faithfully_and_cover_every_sim_scenario() {
    let scenarios = sim_scenarios();
    let mut fixture_ids = std::collections::BTreeSet::new();
    let mut fixture_count = 0;

    for entry in std::fs::read_dir(recording_dir()).expect("datasets/recordings exists") {
        let path = entry.expect("recording fixture entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let recording: Recording = serde_json::from_slice(
            &std::fs::read(&path).unwrap_or_else(|err| panic!("{}: {err}", path.display())),
        )
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
        let spec = read_spec(&recording.scenario);
        assert_eq!(
            spec.driver(),
            "sim",
            "{} fixture is not sim",
            path.display()
        );
        let report = replay(&recording, build_driver(&spec), &spec.task.grants);
        assert!(
            report.faithful(&recording),
            "{} fixture diverged: {report:?}",
            recording.scenario
        );
        fixture_ids.insert(recording.scenario.clone());
        fixture_count += 1;
    }

    let expected_ids: std::collections::BTreeSet<_> = scenarios
        .iter()
        .map(|(_, spec)| spec.scenario.id.clone())
        .collect();
    assert_eq!(
        fixture_ids, expected_ids,
        "fixture coverage must match sim scenarios"
    );
    assert_eq!(fixture_count, expected_ids.len());
}

#[test]
fn replay_detects_final_verification_divergence_in_a_changed_world() {
    let mut spec = read_spec("form-fill");
    let run = run(&spec);
    let recording = record_run(&spec, &run).expect("form-fill recording");
    assert_eq!(
        recording.steps.len(),
        2,
        "form-fill has edit and submit steps"
    );

    spec.world.rule.clear();
    let report = replay(&recording, build_driver(&spec), &spec.task.grants);
    assert_eq!(
        report.diverged_at, None,
        "actions still have matching results"
    );
    assert_eq!(report.replayed.len(), recording.steps.len());
    assert_eq!(
        report.done_when,
        VerificationStatus::Failed,
        "removed Enviar press rule leaves final completion text absent"
    );
    assert!(!report.faithful(&recording));
}

#[test]
fn incomplete_dropped_and_malformed_journals_fail_closed() {
    let spec = read_spec("form-fill");
    let mut scenario_run = run(&spec);
    let last_proposal = scenario_run
        .events
        .iter()
        .rposition(|event| event.kind == EventKind::ActionProposed)
        .expect("form-fill proposes an action");
    let terminal = scenario_run.events[last_proposal + 1..]
        .iter()
        .position(is_terminal)
        .map(|offset| last_proposal + 1 + offset)
        .expect("last proposal has a terminal");
    scenario_run.events.remove(terminal);
    assert!(matches!(
        record_run(&spec, &scenario_run),
        Err(RecordError::Incomplete(_))
    ));

    let mut dropped_run = run(&spec);
    dropped_run.journal_dropped = 1;
    assert!(matches!(
        record_run(&spec, &dropped_run),
        Err(RecordError::Incomplete(_))
    ));

    let done_when = never_present();
    let garbage_action = Event::new(
        EventKind::ActionProposed,
        json!({"action": "garbage", "app": null}),
    );
    assert!(matches!(
        record("bad", "goal", "failed", &done_when, &[garbage_action]),
        Err(RecordError::Malformed(_))
    ));

    let action = Action::Wait { millis: 1 };
    let two_open_proposals = [proposal(&action), proposal(&action)];
    assert!(matches!(
        record("bad", "goal", "failed", &done_when, &two_open_proposals),
        Err(RecordError::Malformed(_))
    ));
}

#[test]
fn jsonl_parser_is_strict_but_accepts_blank_lines() {
    assert!(matches!(
        events_from_jsonl("this is not JSON\n"),
        Err(RecordError::Malformed(_))
    ));
    assert!(events_from_jsonl("\n \t\n").unwrap().is_empty());
}

#[test]
fn jsonl_round_trip_matches_the_in_memory_recording() {
    let spec = read_spec("form-fill");
    let run = run(&spec);
    let expected = record_run(&spec, &run).expect("in-memory recording");
    let mut jsonl = String::new();
    for event in &run.events {
        jsonl.push_str(&serde_json::to_string(event).expect("event serializes"));
        jsonl.push('\n');
    }
    let parsed = events_from_jsonl(&jsonl).expect("journal JSONL parses");
    let actual = record(
        &spec.scenario.id,
        &spec.scenario.goal,
        &run.outcome,
        &spec.task.done_when,
        &parsed,
    )
    .expect("JSONL events record");
    assert_eq!(actual, expected);
}

#[test]
fn record_extracts_driver_fingerprint_and_recorded_grant() {
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let fingerprint = dexter_policy::fingerprint(&action, &ActionContext::default());
    let events = [
        proposal_with_identity(&action, "linux", Some(&fingerprint)),
        Event::new(
            EventKind::PolicyChecked,
            json!({"decision": "approved", "fingerprint": fingerprint}),
        ),
        Event::new(EventKind::ActionExecuted, json!({"status": "Success"})),
    ];

    let recording = record("live", "click OK", "completed", &never_present(), &events)
        .expect("live journal records");
    assert_eq!(recording.driver.as_deref(), Some("linux"));
    assert_eq!(
        recording.steps[0].fingerprint.as_deref(),
        Some(fingerprint.as_str())
    );
    assert!(recording.steps[0].granted);
}

#[test]
fn recorded_grant_requires_the_proposed_fingerprint() {
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let fingerprint = dexter_policy::fingerprint(&action, &ActionContext::default());
    let events = [
        proposal_with_identity(&action, "linux", Some(&fingerprint)),
        Event::new(
            EventKind::PolicyChecked,
            json!({"decision": "approved", "fingerprint": "different"}),
        ),
        Event::new(EventKind::ActionExecuted, json!({"status": "Success"})),
    ];

    let recording = record("live", "click OK", "completed", &never_present(), &events)
        .expect("live journal records");
    assert!(!recording.steps[0].granted);
}

#[test]
fn approve_all_is_not_a_recorded_fingerprint_grant() {
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let fingerprint = dexter_policy::fingerprint(&action, &ActionContext::default());
    let events = [
        proposal_with_identity(&action, "browser", Some(&fingerprint)),
        Event::new(
            EventKind::HumanApprovalRequired,
            json!({"fingerprint": fingerprint, "granted": "approve_all"}),
        ),
        Event::new(
            EventKind::PolicyChecked,
            json!({"decision": "approved", "fingerprint": fingerprint}),
        ),
        Event::new(EventKind::ActionExecuted, json!({"status": "Success"})),
    ];

    let recording = record("live", "click OK", "completed", &never_present(), &events)
        .expect("approve_all journal records");
    assert!(!recording.steps[0].granted);
}

#[test]
fn record_rejects_journals_that_mix_driver_identities() {
    let events = [
        proposal_with_identity(&Action::Wait { millis: 1 }, "linux", None),
        Event::new(EventKind::ActionExecuted, json!({"status": "Success"})),
        proposal_with_identity(&Action::Wait { millis: 2 }, "browser", None),
        Event::new(EventKind::ActionExecuted, json!({"status": "Success"})),
    ];

    let error = record("mixed", "wait", "completed", &never_present(), &events)
        .expect_err("mixed drivers are malformed");
    assert!(matches!(error, RecordError::Malformed(message) if message.contains("mixes drivers")));
}

#[test]
fn legacy_recording_json_defaults_driver_and_recorded_grants() {
    let legacy = json!({
        "scenario": "legacy",
        "goal": "wait",
        "outcome": "failed",
        "done_when": never_present(),
        "steps": [{
            "action": Action::Wait { millis: 1 },
            "app": null,
            "observes_before": 0,
            "result": "done"
        }],
        "trailing_observes": 0
    });
    let recording: Recording =
        serde_json::from_value(legacy).expect("legacy recording still loads");
    assert_eq!(recording.driver, None);
    assert_eq!(recording.steps[0].fingerprint, None);
    assert!(!recording.steps[0].granted);
}

#[test]
fn replay_refuses_driver_mismatches_before_observation_or_action() {
    let recording = one_step_recording(
        Some("sim"),
        Action::Wait { millis: 1 },
        StepResult::Done,
        None,
        false,
    );
    let (driver, act_calls, observe_calls) = CountingDriver::new("browser", vec![], None);
    let report = replay_with(
        &recording,
        driver,
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions::default(),
    );

    assert!(report.replayed.is_empty());
    assert_eq!(act_calls.load(Ordering::SeqCst), 0);
    assert_eq!(observe_calls.load(Ordering::SeqCst), 0);
    let divergence = report.divergence.expect("driver mismatch diverges");
    assert_eq!(divergence.index, 0);
    assert_eq!(divergence.recorded, Some(StepResult::Done));
    assert_eq!(divergence.replayed, None);
    assert_eq!(
        divergence.reason,
        "driver mismatch: recorded by sim, replaying on browser"
    );
}

#[test]
fn live_replay_refuses_legacy_recordings_without_driver_identity() {
    let recording = one_step_recording(
        None,
        Action::Wait { millis: 1 },
        StepResult::Done,
        None,
        false,
    );
    let (driver, act_calls, observe_calls) = CountingDriver::new("linux", vec![], None);
    let report = replay_with(
        &recording,
        driver,
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions::default(),
    );

    assert!(report.replayed.is_empty());
    assert_eq!(act_calls.load(Ordering::SeqCst), 0);
    assert_eq!(observe_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        report.divergence.expect("legacy live replay diverges").reason,
        "recording has no driver identity; live replay needs a recording made from a driver journal"
    );
}

#[test]
fn replay_reports_pre_step_and_final_observe_failures() {
    let mut before_step = one_step_recording(
        Some("sim"),
        Action::Wait { millis: 1 },
        StepResult::Done,
        None,
        false,
    );
    before_step.steps[0].observes_before = 1;
    let (driver, _, _) = CountingDriver::new("sim", vec![], Some(0));
    let report = replay_with(
        &before_step,
        driver,
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions::default(),
    );
    let divergence = report
        .divergence
        .expect("pre-step observe failure diverges");
    assert_eq!(divergence.index, 0);
    assert_eq!(
        divergence.reason,
        "observe failed before step: platform error: observe call 0 failed"
    );

    let after_steps = one_step_recording(
        Some("sim"),
        Action::Wait { millis: 1 },
        StepResult::Done,
        None,
        false,
    );
    let (driver, _, _) = CountingDriver::new("sim", vec![], Some(0));
    let report = replay_with(
        &after_steps,
        driver,
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions::default(),
    );
    let divergence = report.divergence.expect("final observe failure diverges");
    assert_eq!(divergence.index, 1);
    assert_eq!(
        divergence.reason,
        "final observe failed: platform error: observe call 0 failed"
    );
}

#[test]
fn recorded_grants_are_opt_in_and_fingerprint_exact() {
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            role: Some("button".into()),
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let fingerprint = dexter_policy::fingerprint(&action, &ActionContext::default());
    let recording = one_step_recording(
        Some("sim"),
        action,
        StepResult::Done,
        Some(fingerprint.clone()),
        true,
    );
    let driver = || {
        SimDriver::new(vec![Element {
            id: ElementId(1),
            role: Some("button".into()),
            name: Some("OK".into()),
            actions: vec!["press".into()],
            enabled: Some(true),
            ..Default::default()
        }])
    };

    let report = replay_with(
        &recording,
        driver(),
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions::default(),
    );
    assert_eq!(report.replayed, vec![StepResult::NeedsApproval]);
    assert!(report
        .divergence
        .as_ref()
        .expect("missing recorded grant diverges")
        .reason
        .contains(&fingerprint));

    let report = replay_with(
        &recording,
        driver(),
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions {
            honor_recorded_grants: true,
            ..Default::default()
        },
    );
    assert_eq!(report.replayed, vec![StepResult::Done]);
    assert!(report.faithful(&recording));
    assert_eq!(report.divergence, None);
}

#[test]
fn recorded_grant_fingerprint_mismatch_does_not_run_the_step() {
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let recording = one_step_recording(
        Some("sim"),
        action,
        StepResult::Done,
        Some("recorded-fingerprint".into()),
        true,
    );
    let (driver, act_calls, _) = CountingDriver::new(
        "sim",
        vec![Element {
            id: ElementId(1),
            role: Some("button".into()),
            name: Some("OK".into()),
            actions: vec!["press".into()],
            enabled: Some(true),
            ..Default::default()
        }],
        None,
    );
    let report = replay_with(
        &recording,
        driver,
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions {
            honor_recorded_grants: true,
            ..Default::default()
        },
    );

    assert!(report.replayed.is_empty());
    assert_eq!(act_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        report
            .divergence
            .expect("fingerprint mismatch diverges")
            .reason,
        "recorded grant fingerprint does not match replayed step"
    );
}

#[test]
fn recorded_grant_does_not_leak_to_an_identical_next_step() {
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            role: Some("button".into()),
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let fingerprint = dexter_policy::fingerprint(&action, &ActionContext::default());
    let mut recording = one_step_recording(
        Some("sim"),
        action.clone(),
        StepResult::Done,
        Some(fingerprint.clone()),
        true,
    );
    recording.steps.push(RecordedStep {
        action,
        app: None,
        observes_before: 0,
        result: StepResult::NeedsApproval,
        fingerprint: Some(fingerprint),
        granted: false,
    });
    let driver = SimDriver::new(vec![Element {
        id: ElementId(1),
        role: Some("button".into()),
        name: Some("OK".into()),
        actions: vec!["press".into()],
        enabled: Some(true),
        ..Default::default()
    }]);
    let report = replay_with(
        &recording,
        driver,
        Policy::embedded(),
        RunConfig::default(),
        &ReplayOptions {
            honor_recorded_grants: true,
            ..Default::default()
        },
    );

    assert_eq!(
        report.replayed,
        vec![StepResult::Done, StepResult::NeedsApproval]
    );
    assert_eq!(report.divergence, None);
}

#[test]
fn divergence_reports_step_status_reasons() {
    let click = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let recording = one_step_recording(Some("sim"), click.clone(), StepResult::Done, None, false);
    let deny_policy = Policy::from_toml(
        r#"[[rule]]
action = "click"
decision = "deny"
reason = "replay cannot press""#,
    )
    .expect("policy parses");
    let report = replay_with(
        &recording,
        SimDriver::new(vec![]),
        deny_policy,
        RunConfig::default(),
        &ReplayOptions::default(),
    );
    assert_eq!(
        report.divergence.expect("denied action diverges").reason,
        "replay cannot press"
    );

    let point_click = Action::Click {
        target: Target::Point { x: 12.0, y: 34.0 },
        button: MouseButton::Left,
    };
    let point_recording =
        one_step_recording(Some("sim"), point_click, StepResult::Done, None, false);
    let allow_physical =
        Policy::from_toml("[defaults]\nphysical = \"allow\"\nmutating = \"allow\"")
            .expect("policy parses");
    let report = replay_with(
        &point_recording,
        SimDriver::new(vec![]),
        allow_physical,
        RunConfig::default(),
        &ReplayOptions::default(),
    );
    assert_eq!(
        report.divergence.expect("failed action diverges").reason,
        "Unsupported: coordinate input disabled"
    );
}

#[test]
fn coordinates_are_disabled_unless_replay_options_allow_them() {
    let action = Action::Click {
        target: Target::Point { x: 12.0, y: 34.0 },
        button: MouseButton::Left,
    };
    let recording = one_step_recording(Some("sim"), action, StepResult::Done, None, false);
    let policy =
        || Policy::from_toml("[defaults]\nphysical = \"allow\"\nmutating = \"allow\"").unwrap();
    let cfg = RunConfig {
        allow_coordinates: true,
        ..RunConfig::default()
    };
    let report = replay_with(
        &recording,
        SimDriver::new(vec![]),
        policy(),
        cfg,
        &ReplayOptions::default(),
    );
    assert_eq!(report.replayed, vec![StepResult::Failed]);

    let report = replay_with(
        &recording,
        SimDriver::new(vec![]),
        policy(),
        RunConfig::default(),
        &ReplayOptions {
            allow_coordinates: true,
            ..Default::default()
        },
    );
    assert_eq!(report.replayed, vec![StepResult::Done]);
    assert!(report.divergence.is_none());
}

#[test]
fn replay_preserves_embedded_policy_denials() {
    let recording = Recording {
        scenario: "policy-denied-key".into(),
        driver: None,
        goal: "press a physical key".into(),
        outcome: "abstained".into(),
        done_when: never_present(),
        steps: vec![RecordedStep {
            action: Action::Key {
                chord: KeyChord::parse("a").expect("valid key chord"),
            },
            app: Some(AppSelector::Name("sim".into())),
            observes_before: 1,
            result: StepResult::Denied,
            fingerprint: None,
            granted: false,
        }],
        trailing_observes: 1,
    };
    let report = replay(&recording, SimDriver::new(Vec::new()), &[]);
    assert_eq!(report.replayed, vec![StepResult::Denied]);
    assert_eq!(report.diverged_at, None);
    assert_ne!(report.done_when, VerificationStatus::Verified);
    assert!(report.faithful(&recording));
}

#[test]
fn proposal_parser_rejects_invalid_app_values() {
    let done_when = never_present();
    let action = Action::Wait { millis: 1 };
    let mut event = proposal(&action);
    event.data["app"] = Value::String("not an AppSelector".into());
    assert!(matches!(
        record("bad", "goal", "failed", &done_when, &[event]),
        Err(RecordError::Malformed(_))
    ));
}

#[test]
fn outcome_derives_from_the_last_terminal_task_event() {
    let done_proposal = proposal(&Action::Wait { millis: 1 });
    // A journal without a task-level terminal is not a recording source —
    // a truncated file ends mid-run and must not mint an outcome.
    assert_eq!(outcome_from_events(&[]), None);
    assert_eq!(outcome_from_events(&[done_proposal]), None);

    for (event, expected) in [
        (
            Event::new(EventKind::TaskCompleted, json!({"steps": 2})),
            "completed",
        ),
        (
            Event::new(EventKind::SubgoalCompleted, json!({"index": 0})),
            "completed",
        ),
        (
            Event::new(EventKind::TaskFailed, json!({"outcome": "abstain"})),
            "abstained",
        ),
        (
            Event::new(EventKind::TaskFailed, json!({"outcome": "escalated"})),
            "escalated",
        ),
        (
            Event::new(EventKind::TaskFailed, json!({"error": "observe: boom"})),
            "failed",
        ),
        (Event::new(EventKind::TaskCancelled, json!({})), "cancelled"),
        (Event::new(EventKind::TaskTimedOut, json!({})), "timed_out"),
        (
            Event::new(
                EventKind::SubgoalFailed,
                json!({"outcome": "Abstained { reason: \"no candidate\" }"}),
            ),
            "abstained",
        ),
        (
            Event::new(EventKind::SubgoalFailed, json!({"outcome": "MaxSteps"})),
            "max_steps",
        ),
        (
            Event::new(
                EventKind::SubgoalFailed,
                json!({"outcome": "Failed { reason: \"boom\" }"}),
            ),
            "failed",
        ),
    ] {
        assert_eq!(
            outcome_from_events(std::slice::from_ref(&event)).as_deref(),
            Some(expected),
            "{event:?}"
        );
    }

    // Last terminal wins: a successful subgoal followed by an abstain.
    let journal = vec![
        Event::new(EventKind::SubgoalStarted, json!({"index": 0, "goal": "g"})),
        Event::new(EventKind::TaskCompleted, json!({"steps": 1})),
        Event::new(EventKind::SubgoalCompleted, json!({"index": 0})),
        Event::new(EventKind::SubgoalStarted, json!({"index": 1, "goal": "g2"})),
        Event::new(EventKind::TaskFailed, json!({"outcome": "abstain"})),
        Event::new(
            EventKind::SubgoalFailed,
            json!({"outcome": "Abstained { reason: \"no candidate\" }"}),
        ),
    ];
    assert_eq!(outcome_from_events(&journal).as_deref(), Some("abstained"));

    // Unknown debug payload — never guess an outcome.
    assert_eq!(
        outcome_from_events(&[Event::new(
            EventKind::SubgoalFailed,
            json!({"outcome": "SomethingNew { .. }"})
        )]),
        None
    );
}

#[test]
fn replay_with_honors_its_policy_and_run_config() {
    // An action the embedded policy escalates (mutating default) must not
    // be silently approved: with approve_all off and no grants the replay
    // lands `needs_approval`, which diverges from the recorded `done`.
    let click = Action::Click {
        target: Target::Semantic(SemanticTarget {
            role: Some("button".into()),
            name: Some("OK".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let recording = Recording {
        scenario: "needs-consent".into(),
        driver: None,
        goal: "press OK".into(),
        outcome: "failed".into(),
        done_when: never_present(),
        steps: vec![RecordedStep {
            action: click.clone(),
            app: None,
            observes_before: 0,
            result: StepResult::Done,
            fingerprint: None,
            granted: false,
        }],
        trailing_observes: 0,
    };
    let driver = || {
        SimDriver::new(vec![Element {
            id: ElementId(1),
            role: Some("button".into()),
            name: Some("OK".into()),
            actions: vec!["press".into()],
            enabled: Some(true),
            ..Default::default()
        }])
    };
    let cfg = |approve_all| RunConfig {
        app: None,
        max_attempts: 1,
        verify_delay: std::time::Duration::from_millis(5),
        post_act_settle: std::time::Duration::ZERO,
        allow_coordinates: false,
        approve_all,
        observe_max_elements: 4_000,
    };

    // No approve_all, no grant — the escalation is the honest result.
    let report = replay_with(
        &recording,
        driver(),
        Policy::embedded(),
        cfg(false),
        &ReplayOptions::default(),
    );
    assert_eq!(report.replayed, vec![StepResult::NeedsApproval]);
    assert_eq!(report.diverged_at, Some(0));
    assert!(!report.faithful(&recording));

    // The same replay with the operator's fingerprint grant executes.
    let fp = dexter_policy::fingerprint(&click, &ActionContext::default());
    let report = replay_with(
        &recording,
        driver(),
        Policy::embedded(),
        cfg(false),
        &ReplayOptions {
            grants: vec![fp],
            ..Default::default()
        },
    );
    assert_eq!(report.replayed, vec![StepResult::Done]);
    assert!(report.faithful(&recording));

    // A policy that flatly denies the action re-evaluates to Denied.
    let policy = Policy::from_toml(
        r#"[[rule]]
action = "click"
decision = "deny"
reason = "replay cannot press""#,
    )
    .expect("policy parses");
    let report = replay_with(
        &recording,
        driver(),
        policy,
        cfg(true),
        &ReplayOptions::default(),
    );
    assert_eq!(report.replayed, vec![StepResult::Denied]);
    assert!(!report.faithful(&recording));
}
