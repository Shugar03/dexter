//! SimDriver honors `ObservationScope.window` like a real driver.

use dexter_core::*;
use dexter_driver::ComputerDriver;
use dexter_sim::SimDriver;

fn el(id: u64, role: &str, name: &str, bounds: Option<Rect>) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        bounds,
        enabled: Some(true),
        ..Default::default()
    }
}

#[test]
fn observe_scopes_to_window_natively() {
    let sim = SimDriver::new(vec![
        el(
            1,
            "button",
            "Inside",
            Some(Rect {
                x: 5.0,
                y: 5.0,
                w: 10.0,
                h: 10.0,
            }),
        ),
        el(
            2,
            "button",
            "Outside",
            Some(Rect {
                x: 900.0,
                y: 900.0,
                w: 10.0,
                h: 10.0,
            }),
        ),
    ]);
    let obs = sim
        .observe(&ObservationScope {
            window: Some(1),
            ..Default::default()
        })
        .unwrap();
    assert_eq!(obs.windows.len(), 1);
    assert_eq!(obs.elements.len(), 1);
    assert_eq!(obs.elements[0].id, ElementId(1));
    assert!(obs.digest.contains("Inside"));

    let miss = sim.observe(&ObservationScope {
        window: Some(99),
        ..Default::default()
    });
    assert!(miss.is_err(), "unknown window -> error, not empty obs");
}

/// `vision` is a macOS-driver concern — on sim it's an inert flag: same
/// elements, no errors, no screenshot invented.
#[test]
fn vision_flag_is_a_no_op_on_sim() {
    let sim = SimDriver::new(vec![el(
        1,
        "button",
        "Save",
        Some(Rect {
            x: 5.0,
            y: 5.0,
            w: 10.0,
            h: 10.0,
        }),
    )]);
    let scope = ObservationScope {
        vision: true,
        ..ObservationScope::default()
    };
    let obs = sim.observe(&scope).unwrap();
    assert_eq!(obs.elements.len(), 1);
    assert_eq!(obs.collection_errors, 0);
    assert!(obs.screenshot.is_none());
    assert!(obs
        .elements
        .iter()
        .all(|e| e.source == ElementSource::Accessibility));
}
