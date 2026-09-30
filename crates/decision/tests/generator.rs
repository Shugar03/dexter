//! Candidate-generator behavior tests: verb/object parsing, action
//! variety, coverage-based ranking, focused/delta/repeat signals.

use dexter_core::{Action, Element, ElementId, MouseButton, Observation, Target};
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

fn obs(elements: Vec<Element>) -> Observation {
    Observation {
        elements,
        ..Default::default()
    }
}

fn gen() -> HeuristicGenerator {
    HeuristicGenerator::default()
}

fn empty() -> GenHistory {
    GenHistory::default()
}

#[test]
fn coverage_breaks_ties_between_similar_labels() {
    // "open invoice 1042": all three buttons match open+invoice; only the
    // gold matches "1042". The old saturating score tied them and the
    // stable sort picked the first — this test is the regression lock.
    let o = obs(vec![
        el(1, "button", "Open invoice 1041", &["press"]),
        el(2, "button", "Open invoice 1042", &["press"]),
        el(3, "button", "Open invoice 1043", &["press"]),
    ]);
    let cands = gen().generate(&o, "open invoice 1042", &empty());
    assert_eq!(cands.len(), 3);
    let first = &cands[0];
    match &first.action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Open invoice 1042")),
        other => panic!("expected semantic click, got {other:?}"),
    }
    // The gold strictly outranks the distractors (they tie with each
    // other, which is correct — both match 2/3 terms).
    assert!(cands[0].prior > cands[1].prior);
    assert!((cands[1].prior - cands[2].prior).abs() < f32::EPSILON);
}

#[test]
fn edit_verb_emits_setvalue_and_typetext_with_quoted_literal() {
    let o = obs(vec![
        el(1, "text_field", "Card number", &["set_value", "focus"]),
        el(2, "button", "Pay now", &["press"]),
    ]);
    let cands = gen().generate(&o, "enter '4242' in the card field", &empty());
    assert!(
        cands
            .iter()
            .any(|c| matches!(&c.action, Action::SetValue { value, .. } if value == "4242")),
        "expected SetValue with quoted literal, got {cands:?}"
    );
    assert!(
        cands
            .iter()
            .any(|c| matches!(&c.action, Action::TypeText { text, .. } if text == "4242")),
        "expected TypeText with quoted literal, got {cands:?}"
    );
}

#[test]
fn focused_unnamed_field_with_literal_emits_typetext() {
    // Real apps (TextEdit, compose panes) expose unnamed focused text
    // areas: no label can match, so without this the only offer was a
    // Focus no-op and TypeText never appeared — a focus loop, not a
    // write. The caret plus the literal is enough evidence to type.
    let o = obs(vec![Element {
        focused: true,
        ..el(1, "text_area", "", &["set_value", "focus"])
    }]);
    let cands = gen().generate(&o, "escribir 'hola dexter'", &empty());
    assert!(
        cands
            .iter()
            .any(|c| matches!(&c.action, Action::TypeText { text, .. } if text == "hola dexter")),
        "expected TypeText to the focused field, got {cands:?}"
    );
}

#[test]
fn edit_verb_without_literal_offers_focus() {
    let o = obs(vec![el(
        1,
        "text_field",
        "Departure city",
        &["set_value", "focus"],
    )]);
    let cands = gen().generate(&o, "enter the departure city", &empty());
    assert_eq!(cands.len(), 1);
    assert!(matches!(cands[0].action, Action::Focus { .. }));
}

#[test]
fn press_goal_prefers_button_over_pressable_field() {
    // Both match "search"; the button's label is fully covered by goal
    // terms and pressing a field is focus, not action.
    let o = obs(vec![
        el(
            1,
            "text_field",
            "Search destinations",
            &["press", "set_value", "focus"],
        ),
        el(2, "button", "Search", &["press"]),
    ]);
    let cands = gen().generate(&o, "submit the flight search", &empty());
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            button: MouseButton::Left,
        } => assert_eq!(st.name.as_deref(), Some("Search")),
        other => panic!("expected click on Search button, got {other:?}"),
    }
}

#[test]
fn focused_editable_gets_bonus() {
    let focused_field = Element {
        focused: true,
        ..el(1, "text_field", "Email", &["set_value", "focus"])
    };
    let blurred_field = el(2, "text_field", "Email", &["set_value", "focus"]);
    let o = obs(vec![focused_field, blurred_field]);
    let cands = gen().generate(&o, "type the email", &empty());
    // Both are "Email" fields; the focused one must rank first.
    match &cands[0].action {
        Action::Focus {
            target: Target::Semantic(st),
        } => assert_eq!(st.name.as_deref(), Some("Email")),
        other => panic!("{other:?}"),
    }
    assert!(cands[0].prior > cands[1].prior);
}

#[test]
fn repeated_attempt_is_penalized() {
    let o = obs(vec![
        el(1, "button", "Retry upload", &["press"]),
        el(2, "button", "Cancel upload", &["press"]),
    ]);
    let mut hist = GenHistory::default();
    hist.attempts.push(Action::Click {
        target: Target::Semantic(dexter_core::SemanticTarget {
            role: Some("button".into()),
            name: Some("Retry upload".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    });
    let cands = gen().generate(&o, "retry or cancel the upload", &hist);
    assert_eq!(cands.len(), 2);
    // The already-tried element drops below the untried alternative.
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Cancel upload")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn delta_bonus_lifts_newly_appeared_element() {
    let prev = obs(vec![el(1, "button", "Keep editing", &["press"])]);
    let now = obs(vec![
        el(1, "button", "Keep editing", &["press"]),
        el(2, "button", "Discard draft", &["press"]), // appeared
    ]);
    let hist = GenHistory {
        prev: Some(prev),
        ..Default::default()
    };
    let cands = gen().generate(&now, "keep or discard the draft", &hist);
    // Both match one object term; the newly appeared one gets the bonus.
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Discard draft")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn disabled_elements_never_generate() {
    let mut o = obs(vec![el(1, "button", "Sign up", &["press"])]);
    o.elements[0].enabled = Some(false);
    let cands = gen().generate(&o, "sign up for the service", &empty());
    assert!(cands.is_empty());
}

#[test]
fn verb_only_goal_matches_by_verb() {
    let o = obs(vec![
        el(1, "button", "Pay now", &["press"]),
        el(2, "button", "Cancel", &["press"]),
    ]);
    let cands = gen().generate(&o, "pay", &empty());
    assert_eq!(cands.len(), 1);
}

#[test]
fn phrase_verb_log_in() {
    let o = obs(vec![
        el(1, "button", "Log in", &["press"]),
        el(2, "button", "Create account", &["press"]),
    ]);
    let cands = gen().generate(&o, "log in to my account", &empty());
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Log in")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn bounded_candidate_count() {
    let elements: Vec<Element> = (0..50)
        .map(|i| el(i, "button", &format!("Save item {i}"), &["press"]))
        .collect();
    let o = obs(elements);
    let cands = gen().generate(&o, "save the item", &empty());
    assert!(cands.len() <= 12);
}

#[test]
fn edit_penalty_lifts_once_the_field_was_filled() {
    // Fill→submit is the canonical web form: after the SetValue lands,
    // the goal's remaining intent is the press — a submit control that
    // matches a goal term must become actable instead of staying
    // penalized under the edit verb forever.
    let o = obs(vec![
        el(1, "text_field", "Usuario", &["set_value", "focus"]),
        el(2, "button", "Entrar", &["press"]),
    ]);
    let goal = "escribir \"demo\" en usuario y entrar";

    let pre = gen().generate(&o, goal, &empty());
    let entrar = |cands: &[dexter_decision::CandidateAction]| {
        cands
            .iter()
            .find(|c| matches!(&c.action, Action::Click { .. }))
            .expect("a click candidate on Entrar")
            .prior
    };
    assert!(
        entrar(&pre) < 0.65,
        "before the edit, the press is weak evidence"
    );

    let mut hist = GenHistory::default();
    hist.attempts.push(Action::SetValue {
        target: Target::Semantic(dexter_core::SemanticTarget {
            role: Some("text_field".into()),
            name: Some("Usuario".into()),
            ..Default::default()
        }),
        value: "demo".into(),
    });
    let post = gen().generate(&o, goal, &hist);
    assert!(
        entrar(&post) >= 0.65,
        "after the edit, pressing Entrar must cross the act threshold"
    );
}

/// Regressions found by the macOS AX dataset eval — each case was a
/// real MISS against a live TextEdit/Finder tree.

#[test]
fn quantifier_distinguishes_close_from_close_all() {
    // "cerrar todas las ventanas": "Cerrar todo" must beat bare "Cerrar".
    let o = obs(vec![
        el(1, "menu_item", "Cerrar", &["press"]),
        el(2, "menu_item", "Cerrar todo", &["press"]),
    ]);
    let cands = gen().generate(&o, "cerrar todas las ventanas", &empty());
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Cerrar todo")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn label_stopwords_and_empty_tokens_never_match() {
    // "comprar un vuelo" on a text editor: "Guardar como…" must NOT
    // stem-match "comprar" (label-side stopword "como"), and no
    // candidate should be generated at all.
    let o = obs(vec![
        el(1, "menu_item", "Guardar como…", &["press"]),
        el(2, "menu_item", "Nuevo", &["press"]),
    ]);
    let cands = gen().generate(&o, "comprar un vuelo", &empty());
    assert!(cands.is_empty());
}

#[test]
fn digits_are_identifiers_not_words() {
    // "open invoice 1042": "1041"/"1043" share the "104" prefix but are
    // different identifiers — no stemming on pure digits.
    let o = obs(vec![
        el(1, "button", "Open invoice 1041", &["press"]),
        el(2, "button", "Open invoice 1042", &["press"]),
    ]);
    let cands = gen().generate(&o, "open invoice 1042", &empty());
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Open invoice 1042")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn bare_verb_label_does_not_beat_specific_object_match() {
    // Regression: exact_label bonus on a bare verb ("Cerrar") must not
    // outrank an object-bearing label ("Cerrar todo").
    let o = obs(vec![
        el(1, "menu_item", "Configuración del Sistema…", &["press"]),
        el(2, "menu_item", "Nuevo", &["press"]),
    ]);
    let cands = gen().generate(&o, "crear un documento nuevo", &empty());
    assert_eq!(cands.len(), 1);
    match &cands[0].action {
        Action::Click {
            target: Target::Semantic(st),
            ..
        } => assert_eq!(st.name.as_deref(), Some("Nuevo")),
        other => panic!("{other:?}"),
    }
}

#[test]
fn phrase_consumption_uses_real_word_offsets() {
    // "proceed to checkout" consumes bytes 0..19; the standalone
    // "check" later must not be swallowed — the old first-occurrence
    // find() anchored "check" inside "checkout" and dropped the verb.
    let o = obs(vec![
        el(1, "button", "Check", &["press"]),
        el(2, "button", "Box", &["press"]),
    ]);
    let cands = gen().generate(&o, "proceed to checkout, check the box", &empty());
    assert!(
        cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Check")
        )),
        "expected a click on the standalone 'check' target, got {cands:?}"
    );
}

#[test]
fn secure_field_is_editable_by_role() {
    // AX secure fields expose their role; driver action lists vary —
    // the role alone must be enough to offer an edit.
    let o = obs(vec![el(1, "secure_text_field", "Password", &[])]);
    let cands = gen().generate(&o, "enter 'hunter2' in the password field", &empty());
    assert!(
        cands
            .iter()
            .any(|c| matches!(&c.action, Action::SetValue { .. } | Action::TypeText { .. })),
        "expected an edit candidate on the secure field, got {cands:?}"
    );
}

#[test]
fn duplicate_labels_generate_index_qualified_targets() {
    // Two "Save" buttons: a bare {role, name} target can never resolve
    // (Ambiguous → fail-closed) — the generator must qualify by index
    // so each offer lands as a single hit.
    let o = obs(vec![
        el(1, "button", "Save", &["press"]),
        el(2, "button", "Save", &["press"]),
        el(3, "button", "Open", &["press"]),
    ]);
    let cands = gen().generate(&o, "save the file", &empty());
    let save_targets: Vec<&Target> = cands
        .iter()
        .filter_map(|c| match &c.action {
            Action::Click { target, .. } => Some(target),
            _ => None,
        })
        .filter(|t| matches!(t, Target::Semantic(st) if st.name.as_deref() == Some("Save")))
        .collect();
    assert_eq!(
        save_targets.len(),
        2,
        "expected both Save offers: {cands:?}"
    );

    let mut resolved = std::collections::HashSet::new();
    for t in &save_targets {
        let st = match t {
            Target::Semantic(st) => st,
            _ => unreachable!(),
        };
        assert!(st.index.is_some(), "ambiguous label must carry index");
        let hit = dexter_world_model::resolve_element(&o, t).expect("index resolves");
        resolved.insert(hit.id.0);
    }
    assert_eq!(resolved.len(), 2, "each offer resolves to its own element");
}

#[test]
fn antonym_label_is_never_offered() {
    // "Cancelar pedido" under goal "confirmar el pedido" matched the
    // object "pedido" and scored high enough to press — the polarity
    // veto drops it before scoring; only the same-polarity side is
    // offered.
    let o = obs(vec![
        el(1, "button", "Cancelar pedido", &["press"]),
        el(2, "button", "Confirmar pedido", &["press"]),
    ]);
    let cands = gen().generate(&o, "confirmar el pedido", &empty());
    assert!(
        cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Confirmar pedido")
        )),
        "{cands:?}"
    );
    assert!(
        !cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Cancelar pedido")
        )),
        "the opposite polarity must not be offered: {cands:?}"
    );
}

#[test]
fn wrong_polarity_only_world_abstains() {
    // Only "Discard" exists under "save" — nothing honest to offer.
    let o = obs(vec![el(1, "button", "Discard draft", &["press"])]);
    let cands = gen().generate(&o, "save the draft", &empty());
    assert!(cands.is_empty(), "{cands:?}");
}

#[test]
fn negated_goal_term_flips_polarity() {
    // "no guardar" wants the opposite: Descartar is offered, Guardar
    // is vetoed — the antonym of a negated term becomes a wanted term.
    let o = obs(vec![
        el(1, "button", "Guardar borrador", &["press"]),
        el(2, "button", "Descartar borrador", &["press"]),
    ]);
    let cands = gen().generate(&o, "no guardar el borrador", &empty());
    assert!(
        cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Descartar borrador")
        )),
        "{cands:?}"
    );
    assert!(
        !cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Guardar borrador")
        )),
        "the negated act must not be offered: {cands:?}"
    );
}

#[test]
fn english_contraction_negates_next_term() {
    // "don't close" tokenizes as don+t → "close" negated → its
    // antonym "open" becomes the wanted act.
    let o = obs(vec![
        el(1, "button", "Close window", &["press"]),
        el(2, "button", "Open window", &["press"]),
    ]);
    let cands = gen().generate(&o, "don't close the window", &empty());
    assert!(
        cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Open window")
        )),
        "{cands:?}"
    );
    assert!(
        !cands.iter().any(|c| matches!(
            &c.action,
            Action::Click {
                target: Target::Semantic(st),
                ..
            } if st.name.as_deref() == Some("Close window")
        )),
        "{cands:?}"
    );
}
