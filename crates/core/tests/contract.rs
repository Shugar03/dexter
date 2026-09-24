//! Contract tests for dexter-core public types.
//!
//! These pin the serialized shapes every driver, engine, CLI and MCP client
//! will exchange. Expected JSON is written by hand from the contract, never
//! re-derived from the implementation.

use dexter_core::*;

#[test]
fn click_action_defaults_to_left_button() {
    let action: Action =
        serde_json::from_str(r#"{"type":"click","target":{"x":10.0,"y":20.0}}"#).unwrap();
    match action {
        Action::Click { target, button } => {
            assert_eq!(button, MouseButton::Left);
            assert_eq!(target, Target::Point { x: 10.0, y: 20.0 });
        }
        other => panic!("expected click, got {other:?}"),
    }
}

#[test]
fn app_selector_roundtrip_keeps_name_and_bundle_distinct() {
    let name = AppSelector::Name("TextEdit".into());
    let bundle = AppSelector::BundleId("com.apple.TextEdit".into());
    let pid = AppSelector::Pid(4242);

    assert_eq!(
        serde_json::from_value::<AppSelector>(serde_json::to_value(&name).unwrap()).unwrap(),
        name
    );
    assert_eq!(
        serde_json::from_value::<AppSelector>(serde_json::to_value(&bundle).unwrap()).unwrap(),
        bundle
    );
    assert_eq!(
        serde_json::from_value::<AppSelector>(serde_json::to_value(&pid).unwrap()).unwrap(),
        pid
    );
}

#[test]
fn expected_state_combinators_roundtrip() {
    let expected = ExpectedState::All {
        all: vec![
            ExpectedState::ElementExists {
                target: SemanticTarget {
                    role: Some("button".into()),
                    name_contains: Some("Save".into()),
                    ..Default::default()
                },
            },
            ExpectedState::Not {
                not: Box::new(ExpectedState::TextPresent {
                    text: "error".into(),
                }),
            },
        ],
    };

    let json = serde_json::to_value(&expected).unwrap();
    assert_eq!(json["type"], "all");
    assert_eq!(json["all"][0]["type"], "element_exists");
    assert_eq!(json["all"][1]["type"], "not");

    let back: ExpectedState = serde_json::from_value(json).unwrap();
    assert_eq!(back, expected);
}

#[test]
fn element_target_references_its_observation() {
    // An ElementId is meaningless without the Observation it came from;
    // the contract must carry both so stale references can be rejected.
    let target = Target::Element {
        observation: ObservationId(7),
        element: ElementId(42),
    };
    let json = serde_json::to_value(&target).unwrap();
    let back: Target = serde_json::from_value(json).unwrap();
    assert_eq!(back, target);
}

#[test]
fn key_chord_parses_modifiers_and_rejects_missing_key() {
    let chord = KeyChord::parse("cmd+shift+s").unwrap();
    assert_eq!(chord.key, "s");
    assert_eq!(chord.modifiers, vec!["cmd", "shift"]);

    assert!(KeyChord::parse("cmd+shift").is_err());
    assert!(KeyChord::parse("a+b").is_err());
}

#[test]
fn intrusiveness_follows_the_target_not_the_verb() {
    // The same verb is background on an element and physical on raw
    // coordinates — intrusiveness is derived, never model-declared.
    let semantic = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name_contains: Some("Save".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let coordinate = Action::Click {
        target: Target::Point { x: 100.0, y: 200.0 },
        button: MouseButton::Left,
    };
    assert_eq!(semantic.intrusiveness(), Intrusiveness::Background);
    assert_eq!(coordinate.intrusiveness(), Intrusiveness::Physical);
}

#[test]
fn intrusiveness_covers_every_action() {
    use Intrusiveness::*;
    let cases: &[(Action, Intrusiveness)] = &[
        (Action::Observe, Background),
        (Action::Wait { millis: 10 }, Background),
        (
            Action::SetValue {
                target: Target::Focused,
                value: "x".into(),
            },
            Background,
        ),
        (
            Action::TypeText {
                text: "hi".into(),
                target: Some(Target::Focused),
            },
            Background,
        ),
        (
            Action::TypeText {
                text: "hi".into(),
                target: None,
            },
            Physical, // types into whatever is focused — real keystrokes
        ),
        (
            Action::Key {
                chord: KeyChord::parse("cmd+s").unwrap(),
            },
            Physical,
        ),
        (
            Action::Scroll {
                delta: ScrollDelta { dx: 0.0, dy: 100.0 },
                target: None,
            },
            Physical, // scrolls at the user's pointer location
        ),
        (
            Action::Navigate {
                url: "https://x".into(),
            },
            Visual,
        ),
        (
            Action::Focus {
                target: Target::Window { window_id: 1 },
            },
            Visual,
        ),
        (
            Action::Click {
                target: Target::Window { window_id: 1 },
                button: MouseButton::Left,
            },
            Visual, // activate/raise — visible, captures nothing
        ),
        (
            Action::Click {
                target: Target::Focused,
                button: MouseButton::Left,
            },
            Background,
        ),
    ];
    for (action, expected) in cases {
        assert_eq!(action.intrusiveness(), *expected, "{action:?}");
    }
}

#[test]
fn uncertain_verification_is_not_success() {
    let v = Verification::uncertain(vec!["element list was truncated".into()]);
    assert_eq!(v.status, VerificationStatus::Uncertain);
    assert_ne!(v.status, VerificationStatus::Verified);
}

#[test]
fn element_id_accepts_v1_number_and_v2_string() {
    let v1: ElementId = serde_json::from_str("4").unwrap();
    let v2: ElementId = serde_json::from_str("\"e_4\"").unwrap();
    assert_eq!(v1, ElementId(4));
    assert_eq!(v2, ElementId(4));
    assert_eq!(serde_json::to_string(&ElementId(4)).unwrap(), "\"e_4\"");
}

// -- loop-integrity slice 0: Effect/Escalation vocabulary --

#[test]
fn classify_effect_covers_the_four_cases() {
    use dexter_core::{classify_effect, Effect};
    // Confirmed requires a read-back that shows change.
    assert_eq!(classify_effect(true, true), Effect::Confirmed);
    // Changed-or-unproven is unverifiable — the driver saw no read-back.
    assert_eq!(classify_effect(true, false), Effect::Unverifiable);
    assert_eq!(classify_effect(false, false), Effect::Unverifiable);
    // Read-back exists and shows nothing moved: the absorbed click.
    assert_eq!(classify_effect(false, true), Effect::SuspectedNoop);
}

#[test]
fn confirmed_requires_evidence_and_success() {
    use dexter_core::{ActionResult, ActionStatus, Effect, Mechanism};
    // A Confirmed claim without a read-back is a lie — rejected.
    assert!(ActionResult::classified(
        ActionStatus::Success,
        Mechanism::Accessibility,
        None,
        Effect::Confirmed,
        None,
        None,
    )
    .is_err());
    // Confirmed on a failure status is a contradiction.
    assert!(ActionResult::classified(
        ActionStatus::Failed,
        Mechanism::Accessibility,
        None,
        Effect::Confirmed,
        Some("value became 7".into()),
        None,
    )
    .is_err());
    let ok = ActionResult::classified(
        ActionStatus::Success,
        Mechanism::Accessibility,
        None,
        Effect::Confirmed,
        Some("value became 7".into()),
        None,
    )
    .unwrap();
    assert_eq!(ok.effect, Some(Effect::Confirmed));
}

#[test]
fn refused_admits_no_delivery_nor_evidence() {
    use dexter_core::{ActionResult, ActionStatus, Effect, Mechanism};
    // Refused means nothing ran — it cannot claim Success.
    assert!(ActionResult::classified(
        ActionStatus::Success,
        Mechanism::Accessibility,
        None,
        Effect::Refused,
        None,
        None,
    )
    .is_err());
    // And it cannot carry evidence — nothing delivered, nothing read.
    assert!(ActionResult::classified(
        ActionStatus::PermissionDenied,
        Mechanism::Accessibility,
        None,
        Effect::Refused,
        Some("saw it anyway".into()),
        None,
    )
    .is_err());
    let refused = ActionResult::classified(
        ActionStatus::PermissionDenied,
        Mechanism::Accessibility,
        None,
        Effect::Refused,
        None,
        None,
    )
    .unwrap();
    assert_eq!(refused.effect, Some(Effect::Refused));
    assert!(refused.evidence.is_none());
}

#[test]
fn classified_result_serializes_snake_case_and_omits_absent_fields() {
    use dexter_core::{
        ActionResult, ActionStatus, Effect, Escalation, EscalationReason, EscalationTarget,
        Mechanism,
    };
    let r = ActionResult::classified(
        ActionStatus::Success,
        Mechanism::Dom,
        None,
        Effect::SuspectedNoop,
        None,
        Some(Escalation {
            target: EscalationTarget::Px,
            reason: EscalationReason::SuspectedNoop,
        }),
    )
    .unwrap();
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["effect"], "suspected_noop");
    assert_eq!(v["escalation"]["target"], "px");
    assert_eq!(v["escalation"]["reason"], "suspected_noop");
    // Absent optional fields stay out of the wire — v1 payloads unaffected.
    let plain = ActionResult::success(Mechanism::Dom, None);
    let pv = serde_json::to_value(&plain).unwrap();
    assert!(pv.get("effect").is_none());
    assert!(pv.get("evidence").is_none());
    assert!(pv.get("escalation").is_none());
}
