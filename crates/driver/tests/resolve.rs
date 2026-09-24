//! Shared resolve contract — the one stale/ambiguity semantics every
//! adapter delegates to. Parametric over the cases: unknown snapshot,
//! foreign element, vanished node, identity drift, evidence-only
//! sources. Any driver that reimplements these rules differently is a
//! bug; the rules live here.

use dexter_core::*;
use dexter_driver::resolve::*;
use dexter_driver::DriverError;

fn el(id: u64, role: &str, name: &str) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        ..Default::default()
    }
}

fn stale(e: &DriverError) -> bool {
    matches!(e, DriverError::StaleReference(_))
}

#[test]
fn token_contract_table() {
    let obs = ObservationId(7);
    let stored = vec![el(1, "button", "Save"), el(2, "text_field", "q")];
    let fresh = stored.clone();

    // (name, stored, fresh, element, want_ok)
    type Case<'e> = (
        &'static str,
        Option<&'e [Element]>,
        &'e [Element],
        u64,
        bool,
    );
    let cases: Vec<Case> = vec![
        // Snapshot never held or already evicted → refuse.
        ("unknown observation", None, &fresh, 1, false),
        // Element id that was never part of the held snapshot — a
        // foreign reference, not a NotFound.
        ("foreign element id", Some(&stored), &fresh, 99, false),
        // Node vanished from the fresh walk.
        ("vanished element", Some(&stored), &[], 1, false),
        // Everything matches → resolves to the fresh index.
        ("stable element", Some(&stored), &fresh, 1, true),
        ("stable second element", Some(&stored), &fresh, 2, true),
    ];

    for (name, stored, fresh, element, want_ok) in cases {
        let got = resolve_element_ref(stored, fresh, obs, ElementId(element));
        match want_ok {
            true => assert!(got.is_ok(), "{name}: {got:?}"),
            false => assert!(stale(got.as_ref().unwrap_err()), "{name}: {got:?}"),
        }
    }
}

#[test]
fn ocr_elements_refuse_with_actionable_message() {
    let mut ocr = el(1, "static_text", "7");
    ocr.source = ElementSource::Ocr;
    let stored = vec![ocr.clone()];
    let err =
        resolve_element_ref(Some(&stored), &[ocr], ObservationId(1), ElementId(1)).unwrap_err();
    assert!(stale(&err), "{err}");
    // The refusal tells the agent what to do instead — point at bounds.
    assert!(err.to_string().contains("Point"), "{err}");
}

#[test]
fn identity_drift_is_stale_not_notfound() {
    let obs = ObservationId(3);
    let stored = vec![el(1, "button", "Save")];

    // Role change → stale.
    let drifted = vec![el(1, "checkbox", "Save")];
    let err = resolve_element_ref(Some(&stored), &drifted, obs, ElementId(1)).unwrap_err();
    assert!(stale(&err), "{err}");

    // Name change → stale.
    let renamed = vec![el(1, "button", "Save all")];
    let err = resolve_element_ref(Some(&stored), &renamed, obs, ElementId(1)).unwrap_err();
    assert!(stale(&err), "{err}");

    // Depth change → stale (reparented).
    let mut d = el(1, "button", "Save");
    d.depth = 4;
    let err = resolve_element_ref(Some(&stored), &[d], obs, ElementId(1)).unwrap_err();
    assert!(stale(&err), "{err}");
}

#[test]
fn bounds_identity_tolerance_is_two_px() {
    let obs = ObservationId(5);
    let mut e = el(1, "button", "Save");
    e.bounds = Some(Rect {
        x: 10.0,
        y: 10.0,
        w: 50.0,
        h: 20.0,
    });
    let stored = vec![e.clone()];

    // 1.5px jitter → same element.
    let mut near = e.clone();
    near.bounds = Some(Rect {
        x: 11.5,
        y: 11.0,
        w: 50.0,
        h: 20.0,
    });
    assert!(resolve_element_ref(Some(&stored), &[near], obs, ElementId(1)).is_ok());

    // 5px drift → a different element; the token is stale.
    let mut far = e.clone();
    far.bounds = Some(Rect {
        x: 15.0,
        y: 10.0,
        w: 50.0,
        h: 20.0,
    });
    let err = resolve_element_ref(Some(&stored), &[far], obs, ElementId(1)).unwrap_err();
    assert!(stale(&err), "{err}");

    // Bounds present then absent → identity can't be confirmed → stale.
    let unpositioned = el(1, "button", "Save");
    let err = resolve_element_ref(Some(&stored), &[unpositioned], obs, ElementId(1)).unwrap_err();
    assert!(stale(&err), "{err}");
}

#[test]
fn semantic_resolution_maps_errors_identically() {
    let two_saves = vec![el(1, "button", "Save"), el(2, "button", "Save")];
    let obs = Observation {
        elements: two_saves.clone(),
        ..Default::default()
    };
    let ambiguous = Target::Semantic(SemanticTarget {
        name: Some("Save".into()),
        ..Default::default()
    });
    assert!(matches!(
        resolve_semantic(&obs, &ambiguous).unwrap_err(),
        DriverError::Ambiguous(_)
    ));

    let missing = Target::Semantic(SemanticTarget {
        name: Some("Nope".into()),
        ..Default::default()
    });
    assert!(matches!(
        resolve_semantic(&obs, &missing).unwrap_err(),
        DriverError::NotFound(_)
    ));

    // Truncated trees annotate the miss — never definitive-looking.
    let mut partial = obs.clone();
    partial.elements_truncated = true;
    let err = resolve_semantic(&partial, &missing)
        .unwrap_err()
        .to_string();
    assert!(err.contains("truncated"), "{err}");

    // Index disambiguation resolves through the shared path too.
    let second = Target::Semantic(SemanticTarget {
        name: Some("Save".into()),
        index: Some(1),
        ..Default::default()
    });
    assert_eq!(resolve_semantic(&obs, &second).unwrap().id, ElementId(2));
}
