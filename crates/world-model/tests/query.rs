//! Behavior tests for the world-model seam: semantic queries, stale-target
//! rejection, and the digest that feeds decision engines.

use dexter_core::*;
use dexter_world_model::{
    digest, digest_budget, find_elements, normalize_ax_role, resolve_element,
};

fn el(id: u64, role: &str, name: Option<&str>, depth: u32) -> Element {
    Element {
        id: ElementId(id),
        parent: None,
        depth,
        role: Some(normalize_ax_role(role)),
        raw_role: Some(role.into()),
        subrole: None,
        name: name.map(Into::into),
        value: None,
        bounds: None,
        enabled: Some(true),
        focused: false,
        actions: vec![],
        identifier: None,
        source: ElementSource::Accessibility,
    }
}

fn obs(elements: Vec<Element>) -> Observation {
    Observation {
        id: ObservationId(1),
        app: Some(AppSelector::Name("TextEdit".into())),
        pid: Some(100),
        elements,
        ..Default::default()
    }
}

#[test]
fn finds_element_by_normalized_role_and_name() {
    let o = obs(vec![
        el(1, "AXWindow", Some("Untitled"), 0),
        el(2, "AXButton", Some("Save"), 1),
        el(3, "AXButton", Some("Cancel"), 1),
    ]);
    let found = find_elements(
        &o,
        &SemanticTarget {
            role: Some("button".into()),
            name: Some("Save".into()),
            ..Default::default()
        },
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, ElementId(2));
}

#[test]
fn role_matching_accepts_raw_ax_role_and_normalized() {
    let o = obs(vec![el(1, "AXCheckBox", Some("Agree"), 0)]);
    for role in ["checkbox", "AXCheckBox"] {
        let found = find_elements(
            &o,
            &SemanticTarget {
                role: Some(role.into()),
                ..Default::default()
            },
        );
        assert_eq!(found.len(), 1, "role '{role}' should match");
    }
}

#[test]
fn substring_name_match_is_case_insensitive() {
    let o = obs(vec![el(1, "AXButton", Some("Save Document"), 0)]);
    let found = find_elements(
        &o,
        &SemanticTarget {
            name_contains: Some("save doc".into()),
            ..Default::default()
        },
    );
    assert_eq!(found.len(), 1);
}

#[test]
fn resolve_rejects_stale_observation_reference() {
    let o = obs(vec![el(42, "AXButton", Some("OK"), 0)]);
    let stale = Target::Element {
        observation: ObservationId(999),
        element: ElementId(42),
    };
    let err = resolve_element(&o, &stale).unwrap_err();
    assert!(matches!(err, DexterError::InvalidInput(_)));
}

#[test]
fn resolve_ambiguous_target_requires_index() {
    let o = obs(vec![
        el(1, "AXButton", Some("OK"), 0),
        el(2, "AXButton", Some("OK"), 0),
    ]);
    let target = Target::Semantic(SemanticTarget {
        name: Some("OK".into()),
        ..Default::default()
    });
    assert!(matches!(
        resolve_element(&o, &target),
        Err(DexterError::Ambiguous(_))
    ));

    let with_index = Target::Semantic(SemanticTarget {
        name: Some("OK".into()),
        index: Some(1),
        ..Default::default()
    });
    assert_eq!(resolve_element(&o, &with_index).unwrap().id, ElementId(2));
}

#[test]
fn focused_target_returns_focused_element() {
    let mut focused = el(9, "AXTextField", Some("Search"), 0);
    focused.focused = true;
    let o = obs(vec![el(1, "AXButton", None, 0), focused]);
    let found = resolve_element(&o, &Target::Focused).unwrap();
    assert_eq!(found.id, ElementId(9));
}

#[test]
fn digest_prioritizes_actionable_elements_and_marks_truncation() {
    // Anonymous containers are noise for a decision engine; actionable and
    // named elements must come first, and truncation must be visible.
    let mut container = el(1, "AXGroup", None, 0);
    container.actions.clear();
    let mut button = el(2, "AXButton", Some("Save"), 1);
    button.actions = vec!["press".into()];
    let mut button2 = el(3, "AXButton", Some("Cancel"), 1);
    button2.actions = vec!["press".into()];
    let o = obs(vec![container, button, button2]);

    let d = digest(&o, 50);
    assert!(d.contains("Save"), "digest should include named elements");
    assert!(
        d.contains("button"),
        "digest should include normalized roles"
    );
    assert!(!d.contains("e_1"), "anonymous container should be skipped");
    assert!(!d.contains("truncated"));

    let d2 = digest(&o, 1);
    assert!(d2.contains("truncated"), "must report truncation");
}

#[test]
fn digest_budget_respects_char_cap() {
    let els: Vec<Element> = (0..300)
        .map(|i| el(i + 1, "AXButton", Some(&format!("Item {i}")), 1))
        .collect();
    let o = obs(els);
    let d = digest_budget(&o, 2_000);
    assert!(d.len() <= 2_100, "budget holds: {}", d.len());
    assert!(d.contains("context budget"), "truncation is declared");
    // The header is always present — engines see the app + counts.
    assert!(d.contains("elements"));
}

#[test]
fn within_window_scopes_to_intersecting_elements() {
    use dexter_world_model::within_window;
    let mut a = el(1, "AXButton", Some("Save"), 1);
    a.bounds = Some(Rect {
        x: 10.0,
        y: 10.0,
        w: 60.0,
        h: 20.0,
    });
    let mut b = el(2, "AXButton", Some("OtherWindow"), 1);
    b.bounds = Some(Rect {
        x: 500.0,
        y: 400.0,
        w: 60.0,
        h: 20.0,
    });
    let unbound = el(3, "AXMenuItem", Some("About"), 1); // no bounds -> dropped
    let mut o = obs(vec![a, b, unbound]);
    o.windows = vec![
        Window {
            id: 7,
            pid: 100,
            app: "TextEdit".into(),
            title: None,
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 200.0,
            },
            on_screen: true,
            layer: 0,
        },
        Window {
            id: 8,
            pid: 100,
            app: "TextEdit".into(),
            title: None,
            bounds: Rect {
                x: 400.0,
                y: 300.0,
                w: 200.0,
                h: 200.0,
            },
            on_screen: true,
            layer: 0,
        },
    ];
    let scoped = within_window(&o, 7).expect("window 7 exists");
    assert_eq!(scoped.elements.len(), 1);
    assert_eq!(scoped.elements[0].id, ElementId(1), "only in-window kept");
    assert_eq!(scoped.windows.len(), 1);
    assert!(scoped.digest.contains("Save"));
    assert!(within_window(&o, 99).is_none(), "unknown window is a miss");
}

#[test]
fn scope_to_window_skips_refilter_when_driver_scoped() {
    use dexter_world_model::scope_to_window;
    // Driver-scoped obs: windows == [7] already. A subtree element
    // overflowing the window rect (popover) must survive — refiltering
    // would drop it.
    let mut popover = el(1, "AXMenu", Some("Popup"), 1);
    popover.bounds = Some(Rect {
        x: -50.0,
        y: -50.0,
        w: 30.0,
        h: 30.0,
    });
    let mut o = obs(vec![popover]);
    o.windows = vec![Window {
        id: 7,
        pid: 100,
        app: "TextEdit".into(),
        title: None,
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 200.0,
        },
        on_screen: true,
        layer: 0,
    }];
    let scoped = scope_to_window(o, 7).expect("driver-scoped");
    assert_eq!(scoped.elements.len(), 1, "popover kept — no refilter");
}

#[test]
fn scope_to_window_filters_when_driver_didnt() {
    use dexter_world_model::scope_to_window;
    let mut a = el(1, "AXButton", Some("In"), 1);
    a.bounds = Some(Rect {
        x: 10.0,
        y: 10.0,
        w: 10.0,
        h: 10.0,
    });
    let mut b = el(2, "AXButton", Some("Out"), 1);
    b.bounds = Some(Rect {
        x: 999.0,
        y: 999.0,
        w: 10.0,
        h: 10.0,
    });
    let mut o = obs(vec![a, b]);
    o.windows = vec![
        Window {
            id: 7,
            pid: 100,
            app: "A".into(),
            title: None,
            bounds: Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            on_screen: true,
            layer: 0,
        },
        Window {
            id: 8,
            pid: 100,
            app: "A".into(),
            title: None,
            bounds: Rect {
                x: 900.0,
                y: 900.0,
                w: 100.0,
                h: 100.0,
            },
            on_screen: true,
            layer: 0,
        },
    ];
    let scoped = scope_to_window(o, 7).unwrap();
    assert_eq!(scoped.elements.len(), 1);
    assert!(scope_to_window(obs(vec![]), 99).is_err());
}

#[test]
fn digest_marks_ocr_elements() {
    let mut e = el(1, "text", Some("Save document"), 1);
    e.source = ElementSource::Ocr;
    let ax = el(2, "button", Some("OK"), 0);
    let o = obs(vec![e, ax]);
    let d = digest(&o, 50);
    assert!(d.contains("[ocr] text \"Save document\""), "{d}");
    assert!(!d.contains("[ocr] button"), "{d}");
}
