//! Blocking-modal scoping: a live modal (positive `modal: Some(true)`
//! on a window-ish role) confines candidates to its subtree — controls
//! behind it can't be acted on, so offering them would simulate reach
//! that isn't there. Unknown modality (`None`/`Some(false)` or a flag
//! on a non-modal role) never restricts.

use dexter_core::{Action, Element, ElementId, Observation, Target};
use dexter_decision::{CandidateGenerator, GenHistory, HeuristicGenerator};

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

fn child_of(parent: u64, e: Element) -> Element {
    Element {
        parent: Some(ElementId(parent)),
        ..e
    }
}

fn modal_dialog(id: u64) -> Element {
    Element {
        modal: Some(true),
        ..el(id, "dialog", "Confirm", &[])
    }
}

fn obs(elements: Vec<Element>) -> Observation {
    Observation {
        elements,
        ..Default::default()
    }
}

fn target_name(c: &dexter_decision::CandidateAction) -> Option<String> {
    match &c.action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        }
        | Action::Focus {
            target: Target::Semantic(st),
        }
        | Action::SetValue {
            target: Target::Semantic(st),
            ..
        } => st.name.clone(),
        Action::TypeText {
            target: Some(Target::Semantic(st)),
            ..
        } => st.name.clone(),
        _ => None,
    }
}

#[test]
fn modal_scopes_candidates_to_its_subtree() {
    // A confirmation dialog is up; "Guardar" sits behind it. Even though
    // it matches the goal, the press cannot land — it must not be offered.
    let o = obs(vec![
        el(1, "button", "Guardar", &["press"]),
        modal_dialog(2),
        child_of(2, el(3, "button", "Confirmar", &["press"])),
        child_of(2, el(4, "button", "Cancelar", &["press"])),
    ]);
    let cands =
        HeuristicGenerator::default().generate(&o, "guardar el documento", &GenHistory::default());
    assert!(
        !cands
            .iter()
            .any(|c| target_name(c).as_deref() == Some("Guardar")),
        "modal must veto the background control: {cands:?}"
    );
}

#[test]
fn descendants_of_the_modal_still_offer() {
    let o = obs(vec![
        el(1, "button", "Confirmar pedido", &["press"]),
        modal_dialog(2),
        child_of(2, el(3, "button", "Confirmar", &["press"])),
        child_of(2, el(4, "static_text", "¿Seguro?", &[])),
    ]);
    let cands = HeuristicGenerator::default().generate(&o, "confirmar", &GenHistory::default());
    assert!(
        cands
            .iter()
            .any(|c| target_name(c).as_deref() == Some("Confirmar")),
        "the in-modal control must be offerable: {cands:?}"
    );
    // The background duplicate is silently absent — not even low-prior.
    assert!(
        !cands
            .iter()
            .any(|c| target_name(c).as_deref() == Some("Confirmar pedido")),
        "background duplicate must not be offered: {cands:?}"
    );
    // The modal mark is journal-visible in the rationale.
    assert!(
        cands
            .iter()
            .all(|c| c.rationale.contains("inside blocking modal")),
        "modal scope must be journaled: {cands:?}"
    );
}

#[test]
fn deep_descendants_are_inside_the_scope() {
    // A button two levels under the dialog (dialog > group > button).
    let o = obs(vec![
        el(1, "button", "Confirmar", &["press"]),
        modal_dialog(2),
        child_of(2, el(5, "group", "Choices", &[])),
        child_of(5, el(3, "button", "Confirmar", &["press"])),
    ]);
    let cands = HeuristicGenerator::default().generate(&o, "confirmar", &GenHistory::default());
    assert_eq!(cands.len(), 1);
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => {
            assert_eq!(st.name.as_deref(), Some("Confirmar"));
            // index disambiguation still sees the FULL tree — the
            // in-modal "Confirmar" is match #2 of two.
            assert_eq!(st.index, Some(1));
        }
        other => panic!("expected click, got {other:?}"),
    }
}

#[test]
fn modal_with_only_background_matches_offers_nothing() {
    // The goal names a background control and the modal carries no
    // matching controls — honest empty set, an abstain downstream.
    let o = obs(vec![
        el(1, "button", "Guardar", &["press"]),
        modal_dialog(2),
        child_of(2, el(3, "button", "OK", &["press"])),
    ]);
    let cands = HeuristicGenerator::default().generate(&o, "guardar", &GenHistory::default());
    assert!(
        !cands
            .iter()
            .any(|c| target_name(c).as_deref() == Some("Guardar")),
        "background match must not leak: {cands:?}"
    );
}

#[test]
fn unknown_modality_never_restricts() {
    // Same tree, but the dialog's modality is unknown (None) — web
    // popovers abuse role=dialog constantly, so only positive evidence
    // scopes.
    for flag in [None, Some(false)] {
        let mut dialog = el(2, "dialog", "Panel", &[]);
        dialog.modal = flag;
        let o = obs(vec![
            el(1, "button", "Guardar", &["press"]),
            dialog,
            child_of(2, el(3, "button", "OK", &["press"])),
        ]);
        let cands = HeuristicGenerator::default().generate(&o, "guardar", &GenHistory::default());
        assert!(
            cands
                .iter()
                .any(|c| target_name(c).as_deref() == Some("Guardar")),
            "modal {flag:?} must not restrict: {cands:?}"
        );
    }
}

#[test]
fn modal_flag_on_non_dialog_role_is_ignored() {
    // A stray flag on a plain control is not a container — ignoring it
    // keeps a stray `aria-modal` from blacking out the page.
    let mut rogue = el(1, "button", "Guardar", &["press"]);
    rogue.modal = Some(true);
    let o = obs(vec![rogue, el(2, "button", "Cancelar", &["press"])]);
    let cands = HeuristicGenerator::default().generate(&o, "cancelar", &GenHistory::default());
    assert!(
        cands
            .iter()
            .any(|c| target_name(c).as_deref() == Some("Cancelar")),
        "rogue modal flag must be ignored: {cands:?}"
    );
}
