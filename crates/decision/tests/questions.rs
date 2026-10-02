//! Typed Laya questions: blocking-modal detection and ambiguous-target
//! resolution (docs/sdd/laya-questions.md).

use dexter_core::{Action, Element, ElementId, MouseButton, Observation, SemanticTarget, Target};
use dexter_decision::questions::{
    ambiguous_group, typed_questions, BLOCKING_MODAL_ID, DISAMBIGUATE_ID,
};
use dexter_decision::{
    CandidateAction, CandidateGenerator, Decision, DecisionContext, DecisionEngine, GenHistory,
    HeuristicGenerator, Question, Route, RuleBased,
};

fn click(name: &str, index: Option<usize>) -> Action {
    Action::Click {
        target: Target::Semantic(SemanticTarget {
            role: Some("button".into()),
            name: Some(name.into()),
            index,
            ..Default::default()
        }),
        button: MouseButton::Left,
    }
}

fn cand(name: &str, index: Option<usize>, prior: f32) -> CandidateAction {
    CandidateAction {
        action: click(name, index),
        rationale: format!("button \"{name}\""),
        prior,
        behind_modal: None,
    }
}

fn ctx(candidates: Vec<CandidateAction>) -> DecisionContext {
    DecisionContext {
        goal: "g".into(),
        state_digest: String::new(),
        candidates,
        last_error: None,
        step: 1,
    }
}

fn el(id: u64, parent: Option<u64>, role: &str, name: &str) -> Element {
    Element {
        id: ElementId(id),
        parent: parent.map(ElementId),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: if role == "button" {
            vec!["press".into()]
        } else {
            vec![]
        },
        enabled: Some(true),
        ..Default::default()
    }
}

#[test]
fn twins_tied_at_the_top_form_an_ambiguous_group() {
    let c = vec![
        cand("Eliminar", Some(0), 0.75),
        cand("Eliminar", Some(1), 0.75),
        cand("Cancelar", None, 0.3),
    ];
    assert_eq!(ambiguous_group(&c), vec![0, 1]);
}

#[test]
fn no_group_when_priors_or_labels_differ() {
    assert!(ambiguous_group(&[cand("A", Some(0), 0.9), cand("A", Some(1), 0.5)]).is_empty());
    assert!(ambiguous_group(&[cand("A", None, 0.9), cand("B", None, 0.9)]).is_empty());
    assert!(ambiguous_group(&[cand("A", None, 0.9)]).is_empty());
    assert!(ambiguous_group(&[]).is_empty());
}

#[test]
fn plain_context_asks_no_extra_questions() {
    assert!(typed_questions(&ctx(vec![cand("Guardar", None, 0.9)])).is_empty());
}

#[test]
fn twins_yield_a_disambiguation_choice_with_a_none_option() {
    let qs = typed_questions(&ctx(vec![
        cand("Eliminar", Some(0), 0.75),
        cand("Eliminar", Some(1), 0.75),
    ]));
    assert_eq!(qs.len(), 1);
    match &qs[0] {
        Question::Choice { id, options, .. } => {
            assert_eq!(id, DISAMBIGUATE_ID);
            assert_eq!(options.len(), 3, "two members + none: {options:?}");
            assert!(options[2].starts_with("none"));
        }
        other => panic!("expected choice, got {other:?}"),
    }
}

#[test]
fn modal_yields_a_blocking_bool_naming_it() {
    let mut c = cand("Guardar", None, 0.9);
    c.behind_modal = Some("Actualización disponible".into());
    let qs = typed_questions(&ctx(vec![c]));
    assert_eq!(qs.len(), 1);
    match &qs[0] {
        Question::Bool { id, prompt } => {
            assert_eq!(id, BLOCKING_MODAL_ID);
            assert!(prompt.contains("Actualización disponible"), "{prompt}");
        }
        other => panic!("expected bool, got {other:?}"),
    }
}

#[test]
fn rule_based_abstains_on_ambiguous_twins() {
    let d = RuleBased::default()
        .decide(&ctx(vec![
            cand("Eliminar", Some(0), 0.9),
            cand("Eliminar", Some(1), 0.9),
        ]))
        .unwrap();
    assert!(
        matches!(
            d,
            Decision::Route {
                route: Route::Abstain,
                ..
            }
        ),
        "{d:?}"
    );
}

#[test]
fn rule_based_abstains_when_top_is_behind_a_modal() {
    let mut c = cand("Guardar", None, 0.95);
    c.behind_modal = Some("Aviso".into());
    let d = RuleBased::default().decide(&ctx(vec![c])).unwrap();
    assert!(
        matches!(
            d,
            Decision::Route {
                route: Route::Abstain,
                ..
            }
        ),
        "{d:?}"
    );
}

#[test]
fn generator_marks_candidates_outside_the_dialog() {
    let o = Observation {
        elements: vec![
            el(1, None, "window", "Editor"),
            el(2, Some(1), "button", "Guardar documento"),
            el(3, Some(1), "dialog", "Actualización disponible"),
            el(4, Some(3), "button", "Guardar ajustes"),
        ],
        ..Default::default()
    };
    let cands =
        HeuristicGenerator::default().generate(&o, "guardar el documento", &GenHistory::default());
    let outside = cands
        .iter()
        .find(|c| c.action == click("Guardar documento", None))
        .expect("outside candidate");
    assert_eq!(
        outside.behind_modal.as_deref(),
        Some("Actualización disponible")
    );
    assert!(outside.rationale.contains("behind modal"));
    let inside = cands
        .iter()
        .find(|c| c.action == click("Guardar ajustes", None))
        .expect("inside candidate");
    assert_eq!(inside.behind_modal, None);
    assert!(inside.prior > 0.0);
}

#[test]
fn generator_leaves_modal_free_worlds_unmarked() {
    let o = Observation {
        elements: vec![el(1, None, "button", "Guardar")],
        ..Default::default()
    };
    let cands = HeuristicGenerator::default().generate(&o, "guardar", &GenHistory::default());
    assert_eq!(cands[0].behind_modal, None);
}

#[test]
fn generator_gives_twins_their_row_context() {
    let o = Observation {
        elements: vec![
            el(1, None, "row", "Factura marzo"),
            el(2, Some(1), "button", "Eliminar"),
            el(3, None, "row", "Factura abril"),
            el(4, Some(3), "button", "Eliminar"),
        ],
        ..Default::default()
    };
    let cands = HeuristicGenerator::default().generate(
        &o,
        "eliminar la factura de abril",
        &GenHistory::default(),
    );
    assert_eq!(ambiguous_group(&cands), vec![0, 1]);
    assert!(
        cands[0]
            .rationale
            .contains("occurrence 1 of 2 in \"Factura marzo\""),
        "{}",
        cands[0].rationale
    );
    assert!(
        cands[1]
            .rationale
            .contains("occurrence 2 of 2 in \"Factura abril\""),
        "{}",
        cands[1].rationale
    );
}
