//! World-effect semantics: press rules and observe ticks.

use dexter_core::*;
use dexter_driver::{ActContext, ComputerDriver};
use dexter_sim::{Effect, SimDriver};

fn el(id: u64, role: &str, name: &str) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: vec!["press".into()],
        enabled: Some(true),
        ..Default::default()
    }
}

fn click(target: SemanticTarget) -> Action {
    Action::Click {
        target: Target::Semantic(target),
        button: MouseButton::Left,
    }
}

fn by_name(name: &str) -> SemanticTarget {
    SemanticTarget {
        name: Some(name.into()),
        ..Default::default()
    }
}

fn enabled(sim: &SimDriver, name: &str) -> Option<bool> {
    sim.elements()
        .iter()
        .find(|e| e.name.as_deref() == Some(name))
        .and_then(|e| e.enabled)
}

fn value(sim: &SimDriver, role: &str) -> Option<String> {
    sim.elements()
        .iter()
        .find(|e| e.role.as_deref() == Some(role))
        .and_then(|e| e.value.clone())
}

#[test]
fn set_enabled_of_flips_another_element() {
    // The wizard contract: ticking the license box enables "Siguiente".
    let mut next = el(8, "button", "Siguiente");
    next.enabled = Some(false);
    let sim = SimDriver::new(vec![el(6, "check_box", "Acepto los términos"), next]);
    sim.on_press(
        by_name("Acepto los términos"),
        Effect::SetEnabledOf(by_name("Siguiente"), true),
    );

    assert_eq!(enabled(&sim, "Siguiente"), Some(false));
    sim.act(
        &click(by_name("Acepto los términos")),
        &ActContext::default(),
    )
    .unwrap();
    assert_eq!(enabled(&sim, "Siguiente"), Some(true));
}

#[test]
fn cycle_value_of_advances_per_observe_tick() {
    // A progress indicator that moves while the agent waits/reobserves.
    let mut bar = el(4, "progress_indicator", "");
    bar.value = Some("62".into());
    let sim = SimDriver::new(vec![bar]);
    sim.on_tick(Effect::CycleValueOf(
        SemanticTarget {
            role: Some("progress_indicator".into()),
            ..Default::default()
        },
        vec!["81".into(), "100".into()],
    ));

    sim.observe(&ObservationScope::default()).unwrap();
    assert_eq!(value(&sim, "progress_indicator").as_deref(), Some("81"));
    sim.observe(&ObservationScope::default()).unwrap();
    assert_eq!(value(&sim, "progress_indicator").as_deref(), Some("100"));
    // Cycle is exhausted — stays at the last value, no wraparound.
    sim.observe(&ObservationScope::default()).unwrap();
    assert_eq!(value(&sim, "progress_indicator").as_deref(), Some("100"));
}

#[test]
fn untargeted_scroll_requires_coordinate_opt_in() {
    // Same gate as macOS `Action::Scroll` and sim `Action::Key`:
    // target-less scroll is physical input — never implied.
    let sim = SimDriver::new(vec![el(1, "list", "items")]);
    let scroll = Action::Scroll {
        delta: ScrollDelta {
            dx: 0.0,
            dy: -100.0,
        },
        target: None,
    };
    let denied = sim.act(&scroll, &ActContext::default()).unwrap();
    assert!(
        matches!(denied.status, ActionStatus::Unsupported),
        "untargeted scroll must fail closed without coords, got {denied:?}"
    );
    let ok = sim
        .act(
            &scroll,
            &ActContext {
                allow_coordinates: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert!(matches!(ok.status, ActionStatus::Success));
}

#[test]
fn remove_by_target_vanishes_an_element_on_tick() {
    // World changes under the agent: the target disappears between
    // observations — the stale-reference path has to recover.
    let sim = SimDriver::new(vec![
        el(10, "row", "factura_marzo.pdf"),
        el(11, "row", "notas_viaje.txt"),
    ]);
    sim.on_tick(Effect::Remove(by_name("factura_marzo.pdf")));

    assert_eq!(sim.elements().len(), 2);
    sim.observe(&ObservationScope::default()).unwrap();
    let names: Vec<_> = sim
        .elements()
        .iter()
        .filter_map(|e| e.name.clone())
        .collect();
    assert_eq!(names, vec!["notas_viaje.txt"]);
}

#[test]
fn session_export_is_unsupported_outside_web_drivers() {
    // The trait default is honest: sim has no credential store.
    let sim = SimDriver::new(vec![]);
    assert!(matches!(
        sim.export_session(),
        Err(dexter_driver::DriverError::Unsupported(_))
    ));
    assert!(matches!(
        sim.import_session(&serde_json::json!({})),
        Err(dexter_driver::DriverError::Unsupported(_))
    ));
}
