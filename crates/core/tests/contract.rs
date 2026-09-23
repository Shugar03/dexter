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
fn uncertain_verification_is_not_success() {
    let v = Verification::uncertain(vec!["element list was truncated".into()]);
    assert_eq!(v.status, VerificationStatus::Uncertain);
    assert_ne!(v.status, VerificationStatus::Verified);
}
