use dexter_core::{
    Action, AppSelector, Event, EventKind, ExpectedState, KeyChord, SemanticTarget,
    VerificationStatus,
};
use dexter_decision::{HeuristicGenerator, RuleBased};
use dexter_eval::{
    recording::{
        events_from_jsonl, record, record_run, replay, RecordError, RecordedStep, Recording,
        StepResult,
    },
    scenario::{build_driver, run_scenario, ScenarioRun, ScenarioSpec},
};
use dexter_sim::SimDriver;
use serde_json::{json, Value};
use std::path::PathBuf;

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
fn replay_preserves_embedded_policy_denials() {
    let recording = Recording {
        scenario: "policy-denied-key".into(),
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
