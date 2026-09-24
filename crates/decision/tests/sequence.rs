//! Sequential-goal support: `split_goal` turns "do A, then B" into
//! ordered intents, and the expression path turns "calcular 134 más 89"
//! into the next keypad press — both deterministic, no model.

use dexter_core::{Action, Element, ElementId, Observation, SemanticTarget, Target};
use dexter_decision::{split_goal, CandidateGenerator, GenHistory, HeuristicGenerator};

fn el(id: u64, role: &str, name: &str, actions: &[&str]) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: actions.iter().map(|s| s.to_string()).collect(),
        enabled: Some(true),
        ..Default::default()
    }
}

fn obs(elements: Vec<Element>) -> Observation {
    Observation {
        elements,
        ..Default::default()
    }
}

fn press(name: &str) -> Action {
    Action::Click {
        target: Target::Semantic(SemanticTarget {
            role: Some("button".into()),
            name: Some(name.into()),
            ..Default::default()
        }),
        button: dexter_core::MouseButton::Left,
    }
}

// ---------- split_goal ----------

#[test]
fn conjunction_splits_into_ordered_subgoals() {
    assert_eq!(
        split_goal("escribir 'hola' y guardar"),
        vec!["escribir 'hola'", "guardar"]
    );
    assert_eq!(
        split_goal("aceptar los términos y continuar"),
        vec!["aceptar los términos", "continuar"]
    );
    // Spanish "e" before i-sounds is the same conjunction.
    assert_eq!(
        split_goal("ir a cronómetro e iniciar"),
        vec!["ir a cronómetro", "iniciar"]
    );
    assert_eq!(
        split_goal("click save then close"),
        vec!["click save", "close"]
    );
}

#[test]
fn conjunction_inside_quotes_never_splits() {
    assert_eq!(
        split_goal("escribir 'pan y vino' y guardar"),
        vec!["escribir 'pan y vino'", "guardar"]
    );
}

#[test]
fn bare_conjunction_only_splits_before_a_verb() {
    // "white" is not a verb — this is one intent, not two.
    assert_eq!(
        split_goal("open black and white"),
        vec!["open black and white"]
    );
    // "entrar" is not in the verb lexicon — stays one goal.
    assert_eq!(
        split_goal("escribir \"demo\" en usuario y entrar"),
        vec!["escribir \"demo\" en usuario y entrar"]
    );
}

#[test]
fn no_conjunction_returns_single_goal() {
    assert_eq!(split_goal("abrir wi-fi"), vec!["abrir wi-fi"]);
    assert_eq!(
        split_goal("calcular 134 más 89"),
        vec!["calcular 134 más 89"]
    );
}

// ---------- expression sequencing ----------

fn keypad() -> Vec<Element> {
    let mut els = vec![
        el(1, "window", "Calculadora", &[]),
        el(2, "static_text", "0", &[]),
    ];
    for (i, d) in "0123456789".chars().enumerate() {
        els.push(el(10 + i as u64, "button", &d.to_string(), &["press"]));
    }
    for (i, op) in ["Sumar", "Restar", "Multiplicar", "Dividir", "Es igual a"]
        .iter()
        .enumerate()
    {
        els.push(el(30 + i as u64, "button", op, &["press"]));
    }
    els
}

fn history_with(pressed: &[&str]) -> GenHistory {
    GenHistory {
        attempts: pressed.iter().map(|n| press(n)).collect(),
        attempt_names: pressed.iter().map(|n| Some(n.to_string())).collect(),
        ..Default::default()
    }
}

fn top_label(o: &Observation, cands: &[dexter_decision::CandidateAction]) -> String {
    match &cands[0].action {
        Action::Click {
            target: Target::Element { element, .. },
            ..
        } => o
            .element(*element)
            .and_then(|e| e.name.clone())
            .unwrap_or_default(),
        other => panic!("expected element-bound click, got {other:?}"),
    }
}

#[test]
fn expression_goal_presses_next_keypad_key() {
    let g = HeuristicGenerator::default();
    let goal = "calcular 134 más 89";
    let world = obs(keypad());

    // Fresh world → first operand's first digit.
    let cands = g.generate(&world, goal, &GenHistory::default());
    assert_eq!(top_label(&world, &cands), "1");

    // 1,3,4 pressed → operator next (localized label).
    let cands = g.generate(&world, goal, &history_with(&["1", "3", "4"]));
    assert_eq!(top_label(&world, &cands), "Sumar");

    // Operator pressed → second operand's digits.
    let cands = g.generate(&world, goal, &history_with(&["1", "3", "4", "Sumar", "8"]));
    assert_eq!(top_label(&world, &cands), "9");

    // Both operands + op → equals.
    let cands = g.generate(
        &world,
        goal,
        &history_with(&["1", "3", "4", "Sumar", "8", "9"]),
    );
    assert_eq!(top_label(&world, &cands), "Es igual a");
}

#[test]
fn expression_path_ignores_non_expression_goals() {
    let g = HeuristicGenerator::default();
    // Same keypad, but the goal is not arithmetic — no forced presses.
    let cands = g.generate(&obs(keypad()), "abrir ayuda", &GenHistory::default());
    assert!(cands
        .iter()
        .all(|c| !c.rationale.contains("expression sequence")));
}

#[test]
fn expression_path_requires_a_keypad() {
    let g = HeuristicGenerator::default();
    // Goal parses as arithmetic but the world has no digit buttons —
    // expr must stay silent instead of fabricating candidates.
    let world = obs(vec![el(1, "button", "Guardar", &["press"])]);
    let cands = g.generate(&world, "calcular 134 más 89", &GenHistory::default());
    assert!(cands
        .iter()
        .all(|c| !c.rationale.contains("expression sequence")));
}

#[test]
fn already_satisfied_detects_selected_destination() {
    use dexter_decision::goal_already_satisfied;
    let mut tab = el(1, "radio_button", "Cronómetro", &["press"]);
    tab.value = Some("1".into());
    // The intent held before any act — don't press a selected tab.
    assert!(goal_already_satisfied(
        &obs(vec![tab.clone()]),
        "ir a cronómetro"
    ));
    // Unselected → not satisfied, the press must happen.
    tab.value = Some("0".into());
    assert!(!goal_already_satisfied(&obs(vec![tab]), "ir a cronómetro"));
    // A plain button can't be "already done" — only selectable roles.
    let btn = el(2, "button", "Cronómetro", &["press"]);
    assert!(!goal_already_satisfied(&obs(vec![btn]), "ir a cronómetro"));
}
