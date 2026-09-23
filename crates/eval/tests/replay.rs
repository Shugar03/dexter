//! Replay tests — synthetic observations with known golds.

use dexter_core::{Element, ElementId, ElementSource, Observation, SemanticTarget};
use dexter_decision::{HeuristicGenerator, RuleBased};
use dexter_eval::{run_eval, EvalItem, Gold};

fn el(id: u64, role: &str, name: &str, actions: &[&str]) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: actions.iter().map(|s| s.to_string()).collect(),
        enabled: Some(true),
        source: ElementSource::Dom,
        ..Default::default()
    }
}

fn obs(elements: Vec<Element>) -> Observation {
    let mut o = Observation {
        elements,
        ..Default::default()
    };
    o.digest = dexter_world_model::digest(&o, 250);
    o
}

fn item(id: &str, goal: &str, observation: Observation, gold: Gold) -> EvalItem {
    EvalItem {
        id: id.into(),
        goal: goal.into(),
        observation,
        gold,
        source: "test".into(),
        meta: Default::default(),
    }
}

#[test]
fn engine_correct_when_gold_is_top_candidate() {
    let o = obs(vec![
        el(1, "button", "Cancel", &["press"]),
        el(2, "button", "Confirm order", &["press"]),
        el(3, "text_field", "Email", &["set_value", "focus"]),
    ]);
    let items = vec![item(
        "confirm-1",
        "confirm the order",
        o,
        Gold::Act {
            target: SemanticTarget {
                role: Some("button".into()),
                name: Some("Confirm order".into()),
                ..Default::default()
            },
            element: ElementId(2),
        },
    )];
    let report = run_eval(
        &items,
        &HeuristicGenerator::default(),
        &RuleBased::default(),
    );
    assert_eq!(report.covered, 1);
    assert_eq!(report.correct, 1);
    assert!((report.accuracy() - 1.0).abs() < f64::EPSILON);
}

#[test]
fn coverage_fail_when_generator_misses_gold() {
    // Goal mentions nothing that matches the gold element's label —
    // generator can't offer it → coverage=0, engine not at fault.
    let o = obs(vec![el(1, "button", "Submit", &["press"])]);
    let items = vec![item(
        "opaque-1",
        "finish the wizard",
        o,
        Gold::Act {
            target: SemanticTarget {
                role: Some("button".into()),
                name: Some("Submit".into()),
                ..Default::default()
            },
            element: ElementId(1),
        },
    )];
    let report = run_eval(
        &items,
        &HeuristicGenerator::default(),
        &RuleBased::default(),
    );
    assert_eq!(report.covered, 0);
    assert_eq!(report.correct, 0);
}

#[test]
fn route_gold_counts_separately() {
    // The page shows a tempting "Buy now" but the goal's gold is to wait
    // (page still settling). The generator offers the button — a dumb
    // engine takes it. That's a false act: the dangerous direction.
    let o = obs(vec![el(1, "button", "Buy now", &["press"])]);
    let items = vec![item(
        "wait-1",
        "buy the item",
        o,
        Gold::Route {
            route: dexter_decision::Route::Wait { millis: 500 },
        },
    )];
    let report = run_eval(
        &items,
        &HeuristicGenerator::default(),
        &RuleBased::default(),
    );
    assert_eq!(report.route_items, 1);
    assert_eq!(report.false_acts, 1);
}

#[test]
fn split_by_app_groups_by_provenance() {
    let o = obs(vec![el(1, "button", "Save", &["press"])]);
    let mut a = item(
        "a1",
        "g",
        o.clone(),
        Gold::Route {
            route: dexter_decision::Route::Abstain,
        },
    );
    a.meta = serde_json::json!({"app": "com.apple.TextEdit"});
    let mut b = item(
        "b1",
        "g",
        o.clone(),
        Gold::Route {
            route: dexter_decision::Route::Abstain,
        },
    );
    b.meta = serde_json::json!({"app": "com.apple.finder"});
    // No meta → falls back to the observation's app selector.
    let mut c = item(
        "c1",
        "g",
        o,
        Gold::Route {
            route: dexter_decision::Route::Abstain,
        },
    );
    c.observation.app = Some(dexter_core::AppSelector::BundleId(
        "com.apple.finder".into(),
    ));
    let mut d = item(
        "d1",
        "g",
        c.observation.clone(),
        Gold::Route {
            route: dexter_decision::Route::Abstain,
        },
    );
    d.meta = serde_json::json!({"url": "https://fake.test/checkout"});

    let groups = dexter_eval::split_by_app(&[a, b, c, d]);
    let keys: Vec<&str> = groups.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        vec![
            "com.apple.TextEdit",
            "com.apple.finder",
            "https://fake.test/checkout"
        ]
    );
    // TextEdit 1, finder 2 (meta.app + obs.app fallback merge), url 1.
    assert_eq!(groups[0].1.len(), 1);
    assert_eq!(groups[1].1.len(), 2);
    assert_eq!(groups[2].1.len(), 1);
}

#[test]
fn jsonl_roundtrip() {
    let o = obs(vec![el(7, "button", "Save", &["press"])]);
    let it = item(
        "rt-1",
        "save",
        o,
        Gold::Act {
            target: SemanticTarget {
                name: Some("Save".into()),
                ..Default::default()
            },
            element: ElementId(7),
        },
    );
    let line = serde_json::to_string(&it).unwrap();
    let loaded = dexter_eval::load_jsonl(&line).unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].id, "rt-1");
}
