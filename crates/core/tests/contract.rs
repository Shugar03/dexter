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
fn app_selector_parse_distinguishes_names_from_bundle_ids() {
    use dexter_core::AppSelector;
    assert_eq!(AppSelector::parse("4242"), AppSelector::Pid(4242));
    assert_eq!(
        AppSelector::parse("com.apple.TextEdit"),
        AppSelector::BundleId("com.apple.TextEdit".into())
    );
    // Dotted display names are names, not bundle ids.
    assert_eq!(
        AppSelector::parse("TextEdit 1.2"),
        AppSelector::Name("TextEdit 1.2".into())
    );
    assert_eq!(
        AppSelector::parse("My App.v2"),
        AppSelector::Name("My App.v2".into())
    );
}

fn window(pid: i32, app: &str, bundle_id: Option<&str>) -> Window {
    Window {
        id: 1,
        pid,
        app: app.into(),
        bundle_id: bundle_id.map(Into::into),
        title: None,
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: 10.0,
            h: 10.0,
        },
        on_screen: true,
        layer: 0,
    }
}

#[test]
fn bundle_selector_matches_window_bundle_id_not_owner_name() {
    let sel = AppSelector::BundleId("com.apple.TextEdit".into());
    assert!(sel.matches_window(&window(7, "TextEdit", Some("com.apple.TextEdit"))));
    assert!(sel.matches_window(&window(7, "TextEdit", Some("COM.APPLE.TEXTEDIT"))));
    // An owner name that happens to contain the bundle string is not a match.
    assert!(!sel.matches_window(&window(
        8,
        "com.apple.TextEdit helper",
        Some("com.x.Helper")
    )));
    // A different app whose bundle id merely contains the needle.
    assert!(!sel.matches_window(&window(9, "Other", Some("com.apple.TextEdit.Helper"))));
}

#[test]
fn bundle_selector_fails_closed_without_driver_bundle_id() {
    let sel = AppSelector::BundleId("com.apple.TextEdit".into());
    assert!(!sel.matches_window(&window(7, "com.apple.TextEdit", None)));
}

#[test]
fn pid_and_name_selectors_match_windows() {
    let w = window(7, "TextEdit", Some("com.apple.TextEdit"));
    assert!(AppSelector::Pid(7).matches_window(&w));
    assert!(!AppSelector::Pid(8).matches_window(&w));
    assert!(AppSelector::Name("textedit".into()).matches_window(&w));
    assert!(AppSelector::Name("Text".into()).matches_window(&w));
    assert!(!AppSelector::Name("Safari".into()).matches_window(&w));
}

#[test]
fn window_bundle_id_is_optional_on_the_wire() {
    let json = r#"{"id":1,"pid":7,"app":"TextEdit","title":null,
        "bounds":{"x":0.0,"y":0.0,"w":10.0,"h":10.0},"on_screen":true,"layer":0}"#;
    let w: Window = serde_json::from_str(json).unwrap();
    assert_eq!(w.bundle_id, None);
    let v = serde_json::to_value(&w).unwrap();
    assert!(v.get("bundle_id").is_none(), "absent bundle id is omitted");
    let v = serde_json::to_value(window(7, "TextEdit", Some("com.apple.TextEdit"))).unwrap();
    assert_eq!(v["bundle_id"], "com.apple.TextEdit");
}
