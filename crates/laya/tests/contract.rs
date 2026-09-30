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
fn low_confidence_pick_abstains() {
    let stub = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub_worker.py");
    let engine = LayaEngine::spawn(
        &format!("python3 {} 0.05", stub.display()),
        Duration::from_secs(10),
    )
    .expect("stub spawns")
    .with_min_confidence(0.3);
    let ctx = DecisionContext {
        goal: "click save".into(),
        state_digest: "button Save".into(),
        candidates: vec![candidate("Save")],
        last_error: None,
        step: 1,
    };
    match engine.decide(&ctx).expect("decision") {
        Decision::Route { route, rationale } => {
            assert_eq!(route, Route::Abstain);
            assert!(rationale.contains("0.05"), "rationale: {rationale}");
        }
        other => panic!("confidence 0.05 < 0.3 must abstain, got {other:?}"),
    }
}

#[test]
fn confident_pick_acts() {
    let stub = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub_worker.py");
    let engine = LayaEngine::spawn(
        &format!("python3 {} 0.9", stub.display()),
        Duration::from_secs(10),
    )
    .expect("stub spawns")
    .with_min_confidence(0.3);
    let ctx = DecisionContext {
        goal: "click save".into(),
        state_digest: "button Save".into(),
        candidates: vec![candidate("Save")],
        last_error: None,
        step: 1,
    };
    match engine.decide(&ctx).expect("decision") {
        Decision::Act {
            candidate_index, ..
        } => assert_eq!(candidate_index, Some(0)),
        other => panic!("confidence 0.9 >= 0.3 must act, got {other:?}"),
    }
}

#[test]
fn missing_confidence_is_not_gated() {
    // The dev worker emits no confidence field — engines must still
    // act on its picks (absence ≠ low confidence).
    let engine = LayaEngine::spawn(&worker_cmd(), Duration::from_secs(10))
        .expect("worker spawns")
        .with_min_confidence(0.9);
    let ctx = DecisionContext {
        goal: "click the save button".into(),
        state_digest: "button Save".into(),
        candidates: vec![candidate("Delete"), candidate("Save")],
        last_error: None,
        step: 1,
    };
    match engine.decide(&ctx).expect("decision") {
        Decision::Act {
            candidate_index, ..
        } => assert_eq!(candidate_index, Some(1)),
        other => panic!("missing confidence must not trigger the gate, got {other:?}"),
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

#[test]
fn crashed_worker_is_respawned_and_call_retried() {
    // die_once.py answers one request then exits. decide #1 succeeds,
    // #2 hits a dead worker → respawn + retry must still answer.
    let stub = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/die_once.py");
    let engine = LayaEngine::spawn(
        &format!("python3 {} 0", stub.display()),
        Duration::from_secs(10),
    )
    .expect("stub spawns");
    let ctx = DecisionContext {
        goal: "click save".into(),
        state_digest: "button Save".into(),
        candidates: vec![candidate("Save")],
        last_error: None,
        step: 1,
    };
    for i in 0..3 {
        match engine.decide(&ctx) {
            Ok(Decision::Act {
                candidate_index, ..
            }) => {
                assert_eq!(candidate_index, Some(0), "call {i}")
            }
            other => panic!("call {i}: expected Act after respawn, got {other:?}"),
        }
    }
}

#[test]
fn stale_response_is_dropped_not_consumed_positionally() {
    // delayed_once.py answers its first request ~300ms late. A health
    // probe with a 100ms timeout abandons that response — but the line
    // still lands on the channel. The next request must drop it by id
    // and wait for its own, not pair it positionally: a stale empty
    // answer would surface as "ok but no answers" on a healthy worker.
    let stub = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/delayed_once.py");
    let engine = LayaEngine::spawn(
        &format!("python3 {} 300", stub.display()),
        Duration::from_millis(100),
    )
    .expect("stub spawns");

    // Probe times out; the stale response arrives afterwards.
    let _ = engine.health();
    std::thread::sleep(Duration::from_millis(500));

    // With the stale line queued, decide must still get ITS response.
    let ctx = DecisionContext {
        goal: "click save".into(),
        state_digest: "button Save".into(),
        candidates: vec![candidate("Save")],
        last_error: None,
        step: 1,
    };
    match engine.decide(&ctx) {
        Ok(Decision::Act {
            candidate_index, ..
        }) => assert_eq!(candidate_index, Some(0)),
        other => panic!("stale line desynced the stream, got {other:?}"),
    }
}

#[test]
fn worker_cmd_with_quoted_path_spawns() {
    // Paths with spaces must reach Command::new as one arg — whitespace
    // splitting breaks "my dir/worker.py" into bogus pieces.
    let stub_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub_worker.py");
    let dir = std::env::temp_dir().join("dexter laya space dir");
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("stub.py");
    std::fs::copy(&stub_src, &stub).unwrap();
    let engine = LayaEngine::spawn(
        &format!("python3 \"{}\"", stub.display()),
        Duration::from_secs(10),
    )
    .expect("quoted worker path spawns");
    let ctx = DecisionContext {
        goal: "g".into(),
        state_digest: "s".into(),
        candidates: vec![candidate("X")],
        last_error: None,
        step: 1,
    };
    assert!(matches!(engine.decide(&ctx), Ok(Decision::Act { .. })));
}

#[test]
fn health_reports_ready_for_live_worker_and_down_for_dead() {
    use dexter_decision::EngineHealth;
    let stub = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stub_worker.py");
    let engine = LayaEngine::spawn(
        &format!("python3 {}", stub.display()),
        Duration::from_secs(10),
    )
    .expect("stub spawns");
    assert_eq!(engine.health(), EngineHealth::Ready);

    // die_once answers exactly once then exits — a second probe finds
    // a dead transport → Down, and supervision budget is untouched
    // (health is read-only, it must not respawn).
    let die = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/die_once.py");
    let dead = LayaEngine::spawn(
        &format!("python3 {}", die.display()),
        Duration::from_secs(10),
    )
    .expect("stub spawns");
    let _ = dead.decide(&DecisionContext {
        goal: "g".into(),
        state_digest: "s".into(),
        candidates: vec![candidate("x")],
        last_error: None,
        step: 1,
    });
    // consume the one answer; worker exits after
    std::thread::sleep(Duration::from_millis(150));
    match dead.health() {
        EngineHealth::Down(_) => {}
        other => panic!("dead worker should report Down, got {other:?}"),
    }
}
