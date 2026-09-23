//! Contract tests: LayaEngine <-> worker NDJSON protocol, and decision
//! interpretation. Uses the repo's `workers/laya/worker.py --provider dev`
//! — the real protocol path, deterministic answers, honestly labeled.

use dexter_core::{Action, MouseButton, SemanticTarget, Target};
use dexter_decision::{CandidateAction, Decision, DecisionContext, DecisionEngine, Route};
use dexter_laya::LayaEngine;
use std::path::PathBuf;
use std::time::Duration;

fn worker_cmd() -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workers/laya/worker.py");
    let root = root.canonicalize().expect("worker.py exists");
    format!("python3 {}", root.display())
}

fn candidate(name: &str) -> CandidateAction {
    CandidateAction {
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some(name.into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        rationale: format!("button \"{name}\" matches goal"),
        prior: 0.8,
    }
}

#[test]
fn laya_picks_candidate_matching_goal() {
    let engine = LayaEngine::spawn(&worker_cmd(), Duration::from_secs(10)).expect("worker spawns");
    let ctx = DecisionContext {
        goal: "click the save button".into(),
        state_digest: "button Save [press]\nbutton Delete [press]".into(),
        candidates: vec![candidate("Delete"), candidate("Save")],
        last_error: None,
        step: 1,
    };
    let d = engine.decide(&ctx).expect("decision");
    match d {
        Decision::Act {
            candidate_index, ..
        } => {
            // dev provider scores keyword overlap: "save" matches
            // candidate 1 ("Save"), not candidate 0 ("Delete").
            assert_eq!(candidate_index, Some(1));
        }
        other => panic!("expected Act, got {other:?}"),
    }
    assert_eq!(engine.provider(), "dev");
}

#[test]
fn laya_route_when_no_candidates() {
    let engine = LayaEngine::spawn(&worker_cmd(), Duration::from_secs(10)).expect("worker spawns");
    let ctx = DecisionContext {
        goal: "nothing matches anything".into(),
        state_digest: "empty".into(),
        candidates: vec![],
        last_error: None,
        step: 1,
    };
    let d = engine.decide(&ctx).expect("decision");
    match d {
        // Only route options exist — dev picks index 0 = wait.
        Decision::Route { route, .. } => {
            assert!(matches!(
                route,
                Route::Wait { .. } | Route::Reobserve | Route::EscalateLlm
            ));
        }
        other => panic!("expected Route, got {other:?}"),
    }
}

#[test]
fn missing_worker_is_an_honest_error() {
    let res = LayaEngine::spawn("/nonexistent/dexter-laya-worker", Duration::from_secs(1));
    assert!(res.is_err(), "missing worker must fail at spawn");
}

#[test]
fn malformed_worker_reply_is_a_decision_error() {
    // A "worker" that just echoes garbage — the engine must surface a
    // DecisionError, not panic or accept it.
    let engine = LayaEngine::spawn("/bin/cat", Duration::from_secs(10)).expect("cat spawns");
    let ctx = DecisionContext {
        goal: "g".into(),
        state_digest: "s".into(),
        candidates: vec![candidate("X")],
        last_error: None,
        step: 1,
    };
    // cat echoes the request back — not a valid response object.
    match engine.decide(&ctx) {
        Err(_) => {}
        Ok(d) => panic!("expected DecisionError for garbage reply, got {d:?}"),
    }
}
