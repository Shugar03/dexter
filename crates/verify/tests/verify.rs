//! Verifier contract: three-valued logic, completeness degradation.

use dexter_core::*;
use dexter_verify::verify;

fn el(id: u64, role: &str, name: Option<&str>) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: name.map(String::from),
        ..Default::default()
    }
}

fn obs_with(elements: Vec<Element>) -> Observation {
    Observation {
        id: ObservationId(1),
        elements,
        ..Default::default()
    }
}

#[test]
fn element_exists_verified_and_failed() {
    let o = obs_with(vec![el(1, "button", Some("Save"))]);
    let t = SemanticTarget {
        role: Some("button".into()),
        name: Some("Save".into()),
        ..Default::default()
    };
    assert_eq!(
        verify(&o, &ExpectedState::ElementExists { target: t.clone() }).status,
        VerificationStatus::Verified
    );
    let t2 = SemanticTarget {
        role: Some("button".into()),
        name: Some("Cancel".into()),
        ..Default::default()
    };
    assert_eq!(
        verify(&o, &ExpectedState::ElementExists { target: t2 }).status,
        VerificationStatus::Failed
    );
}

#[test]
fn truncated_tree_degrades_absence_to_uncertain() {
    let mut o = obs_with(vec![el(1, "button", Some("Save"))]);
    o.elements_truncated = true;
    let t = SemanticTarget {
        name: Some("Nonexistent".into()),
        ..Default::default()
    };
    // exists-check with no match + partial tree -> UNCERTAIN
    assert_eq!(
        verify(&o, &ExpectedState::ElementExists { target: t.clone() }).status,
        VerificationStatus::Uncertain
    );
    // absent-check with no match + partial tree -> UNCERTAIN
    assert_eq!(
        verify(&o, &ExpectedState::ElementAbsent { target: t }).status,
        VerificationStatus::Uncertain
    );
    // positive match stays VERIFIED even when truncated
    let t2 = SemanticTarget {
        name: Some("Save".into()),
        ..Default::default()
    };
    assert_eq!(
        verify(&o, &ExpectedState::ElementExists { target: t2 }).status,
        VerificationStatus::Verified
    );
}

#[test]
fn ax_limited_also_degrades() {
    let mut o = obs_with(vec![el(1, "menu_item", Some("Copiar"))]);
    o.ax_limited = true;
    let t = SemanticTarget {
        role: Some("button".into()),
        ..Default::default()
    };
    assert_eq!(
        verify(&o, &ExpectedState::ElementAbsent { target: t }).status,
        VerificationStatus::Uncertain
    );
}

#[test]
fn element_value_predicates() {
    let mut field = el(1, "text_field", None);
    field.value = Some("user@example.com".into());
    let o = obs_with(vec![field]);
    let t = SemanticTarget {
        role: Some("text_field".into()),
        ..Default::default()
    };
    assert_eq!(
        verify(
            &o,
            &ExpectedState::ElementValue {
                target: t.clone(),
                predicate: ValuePredicate::Contains("@".into())
            }
        )
        .status,
        VerificationStatus::Verified
    );
    assert_eq!(
        verify(
            &o,
            &ExpectedState::ElementValue {
                target: t.clone(),
                predicate: ValuePredicate::Equals("other".into())
            }
        )
        .status,
        VerificationStatus::Failed
    );
    assert_eq!(
        verify(
            &o,
            &ExpectedState::ElementValue {
                target: t,
                predicate: ValuePredicate::Matches(r"^[^@]+@[^@]+\.com$".into())
            }
        )
        .status,
        VerificationStatus::Verified
    );
}

#[test]
fn window_title_uncertain_when_all_titles_hidden() {
    let mut o = obs_with(vec![]);
    o.windows.push(Window {
        id: 1,
        pid: 1,
        app: "Finder".into(),
        title: None, // screen recording off
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
        on_screen: true,
        layer: 0,
    });
    assert_eq!(
        verify(&o, &ExpectedState::WindowTitleContains { text: "x".into() }).status,
        VerificationStatus::Uncertain
    );
}

#[test]
fn combinators_three_valued() {
    let o = obs_with(vec![el(1, "button", Some("Save"))]);
    let present = ExpectedState::ElementExists {
        target: SemanticTarget {
            name: Some("Save".into()),
            ..Default::default()
        },
    };
    let missing = ExpectedState::ElementExists {
        target: SemanticTarget {
            name: Some("Nope".into()),
            ..Default::default()
        },
    };
    assert_eq!(
        verify(
            &o,
            &ExpectedState::All {
                all: vec![present.clone(), missing.clone()]
            }
        )
        .status,
        VerificationStatus::Failed
    );
    assert_eq!(
        verify(
            &o,
            &ExpectedState::Any {
                any: vec![present.clone(), missing.clone()]
            }
        )
        .status,
        VerificationStatus::Verified
    );
    assert_eq!(
        verify(
            &o,
            &ExpectedState::Not {
                not: Box::new(missing)
            }
        )
        .status,
        VerificationStatus::Verified
    );
    // All with an uncertain member -> uncertain
    let mut partial = o.clone();
    partial.elements_truncated = true;
    let uncertain = ExpectedState::ElementAbsent {
        target: SemanticTarget {
            name: Some("Nope".into()),
            ..Default::default()
        },
    };
    assert_eq!(
        verify(
            &partial,
            &ExpectedState::All {
                all: vec![present, uncertain]
            }
        )
        .status,
        VerificationStatus::Uncertain
    );
}

// -- loop-integrity slice 1: WorldChanged catch-all --

#[test]
fn world_changed_verifies_when_signature_differs() {
    let before = obs_with(vec![el(1, "button", Some("Save"))]);
    let from = dexter_world_model::signature(&before);
    let mut after = before.clone();
    after.elements.push(el(2, "static_text", Some("Saved")));
    assert_eq!(
        verify(&after, &ExpectedState::WorldChanged { from }).status,
        VerificationStatus::Verified
    );
}

#[test]
fn world_changed_fails_on_identical_complete_tree() {
    let before = obs_with(vec![el(1, "button", Some("Save"))]);
    let from = dexter_world_model::signature(&before);
    // Same world re-observed — the act demonstrably did nothing.
    assert_eq!(
        verify(&before, &ExpectedState::WorldChanged { from }).status,
        VerificationStatus::Failed
    );
}

#[test]
fn world_changed_is_uncertain_when_tree_is_partial() {
    let before = obs_with(vec![el(1, "button", Some("Save"))]);
    let from = dexter_world_model::signature(&before);
    let mut partial = before.clone();
    partial.elements_truncated = true;
    // Same signature on a partial tree can't claim failure honestly.
    assert_eq!(
        verify(&partial, &ExpectedState::WorldChanged { from }).status,
        VerificationStatus::Uncertain
    );
    // A visible difference is still real even on a partial tree.
    partial.elements.push(el(2, "static_text", Some("Saved")));
    assert_eq!(
        verify(&partial, &ExpectedState::WorldChanged { from }).status,
        VerificationStatus::Verified
    );
}

#[test]
fn world_changed_serializes_signature_as_string() {
    let e = ExpectedState::WorldChanged { from: u64::MAX };
    let v = serde_json::to_value(&e).unwrap();
    assert_eq!(v["type"], "world_changed");
    assert_eq!(v["from"], "18446744073709551615");
    let back: ExpectedState = serde_json::from_value(v).unwrap();
    assert_eq!(back, e);
    // Numbers deserialize too — Rust-native callers may emit them.
    let n: ExpectedState = serde_json::from_str(r#"{"type":"world_changed","from":42}"#).unwrap();
    assert_eq!(n, ExpectedState::WorldChanged { from: 42 });
}

#[test]
fn uncertain_verdicts_carry_their_reason() {
    // TreePartial: absence-shaped verdict on a truncated walk.
    let mut partial = obs_with(vec![el(1, "button", Some("Save"))]);
    partial.elements_truncated = true;
    let v = verify(
        &partial,
        &ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Nope".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(v.status, VerificationStatus::Uncertain);
    assert_eq!(v.unknown_reason, Some(UnknownReason::TreePartial));

    // NoFocusedElement: nothing claims focus.
    let v = verify(
        &obs_with(vec![el(1, "button", Some("Save"))]),
        &ExpectedState::FocusedElement {
            target: SemanticTarget {
                name: Some("Save".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(v.status, VerificationStatus::Uncertain);
    assert_eq!(v.unknown_reason, Some(UnknownReason::NoFocusedElement));

    // NoWindowTitle: no window exposes a title.
    let v = verify(
        &obs_with(vec![el(1, "button", Some("Save"))]),
        &ExpectedState::WindowTitleContains {
            text: "anything".into(),
        },
    );
    assert_eq!(v.status, VerificationStatus::Uncertain);
    assert_eq!(v.unknown_reason, Some(UnknownReason::NoWindowTitle));

    // A definite verdict never carries a stranded reason.
    let v = verify(
        &obs_with(vec![el(1, "button", Some("Save"))]),
        &ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Save".into()),
                ..Default::default()
            },
        },
    );
    assert_eq!(v.status, VerificationStatus::Verified);
    assert_eq!(v.unknown_reason, None);
}
