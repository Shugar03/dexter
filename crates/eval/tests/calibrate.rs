//! Threshold calibration — sweeping a decision knob over frozen items
//! and picking the operating point fail-closed.

use dexter_core::{Element, ElementId, ElementSource, Observation, SemanticTarget};
use dexter_decision::{CandidateGenerator, GenHistory, HeuristicGenerator, RuleBased};
use dexter_eval::calibrate::{pick, sweep, sweep_act_threshold, SweepPoint};
use dexter_eval::{EvalItem, EvalReport, Gold};

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

fn top_prior(i: &EvalItem) -> f32 {
    HeuristicGenerator::default()
        .generate(&i.observation, &i.goal, &GenHistory::default())
        .first()
        .map(|c| c.prior)
        .expect("a candidate")
}

/// A strong act-gold and a weak-evidence route-gold: the act threshold
/// trades a false act (too low) against a false route (too high).
fn items() -> Vec<EvalItem> {
    let strong = item(
        "strong",
        "confirm the order",
        obs(vec![el(1, "button", "Confirm order", &["press"])]),
        Gold::Act {
            target: SemanticTarget {
                role: Some("button".into()),
                name: Some("Confirm order".into()),
                ..Default::default()
            },
            element: ElementId(1),
        },
    );
    let weak = item(
        "weak",
        "confirm the payment transfer now",
        obs(vec![el(1, "button", "Confirm order", &["press"])]),
        Gold::Route {
            route: dexter_decision::Route::Abstain,
        },
    );
    vec![strong, weak]
}

#[test]
fn sweep_reports_one_point_per_value_and_pick_is_the_safe_middle() {
    let items = items();
    let (p_strong, p_weak) = (top_prior(&items[0]), top_prior(&items[1]));
    assert!(p_weak < p_strong, "precondition: {p_weak} < {p_strong}");

    let low = p_weak - 0.01;
    let mid = (p_weak + p_strong) / 2.0;
    let high = p_strong + 0.01;
    let points = sweep_act_threshold(&items, &HeuristicGenerator::default(), &[low, mid, high]);
    assert_eq!(points.len(), 3);

    // Too low: acts on the weak item — the dangerous direction.
    assert_eq!(points[0].report.false_acts, 1);
    assert_eq!(points[0].report.correct, 1);
    // Middle: both right.
    assert_eq!(points[1].report.false_acts, 0);
    assert_eq!(points[1].report.false_routes, 0);
    assert_eq!(points[1].score(), 2);
    // Too high: over-caution on the strong item.
    assert_eq!(points[2].report.false_routes, 1);
    assert_eq!(points[2].report.routes_correct, 1);

    assert_eq!(pick(&points), Some(mid));
}

#[test]
fn pick_prefers_no_false_act_at_equal_score() {
    let items = items();
    let p_weak = top_prior(&items[1]);
    // Below the weak prior: 1 correct act + 1 false act. Above both
    // priors: 0 acts, 1 correct route. Equal score, but only the
    // second is free of false acts.
    let points = sweep_act_threshold(
        &items,
        &HeuristicGenerator::default(),
        &[p_weak - 0.01, 1.01],
    );
    assert_eq!(points[0].score(), points[1].score());
    assert_eq!(pick(&points), Some(1.01));
}

fn point(value: f32, correct: usize, routes_correct: usize, false_acts: usize) -> SweepPoint {
    SweepPoint {
        value,
        report: EvalReport {
            correct,
            routes_correct,
            false_acts,
            ..Default::default()
        },
    }
}

#[test]
fn a_false_act_costs_more_than_a_correct_answer_earns() {
    // One more correct answer bought with one false act loses.
    assert_eq!(pick(&[point(0.4, 3, 0, 1), point(0.7, 2, 0, 0)]), Some(0.7));
    // But abstaining on everything is not calibration: a point that
    // keeps one false act and many correct answers beats zero acts.
    assert_eq!(
        pick(&[point(0.65, 40, 9, 2), point(1.0, 4, 11, 0)]),
        Some(0.65)
    );
}

#[test]
fn pick_breaks_ties_toward_the_more_conservative_value() {
    let items = items();
    let (p_strong, p_weak) = (top_prior(&items[0]), top_prior(&items[1]));
    let a = p_weak + (p_strong - p_weak) / 3.0;
    let b = p_weak + 2.0 * (p_strong - p_weak) / 3.0;
    let points = sweep_act_threshold(&items, &HeuristicGenerator::default(), &[b, a]);
    assert_eq!(points[0].score(), points[1].score());
    assert_eq!(pick(&points), Some(b));
    assert_eq!(pick(&[]), None);
}

#[test]
fn sweep_propagates_engine_build_errors() {
    let items = items();
    let res: Result<_, String> = sweep(&items, &HeuristicGenerator::default(), &[0.5, 0.9], |v| {
        if v > 0.6 {
            Err(format!("cannot build at {v}"))
        } else {
            Ok(Box::new(RuleBased { act_threshold: v }) as _)
        }
    });
    assert_eq!(res.err().as_deref(), Some("cannot build at 0.9"));
}
