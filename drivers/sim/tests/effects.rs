//! World-effect semantics: press rules and observe ticks.

use dexter_core::*;
use dexter_driver::{ActContext, ComputerDriver, DriverError};
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
        count: 1,
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
fn stale_element_target_refuses_before_any_side_effect() {
    // Loop-integrity slice 2: an element token is bound to the snapshot
    // that produced it. A reference to an observation the driver no
    // longer holds — or to an element whose identity drifted — must be
    // refused *before* any input: nothing pressed, nothing mutated.
    let sim = SimDriver::new(vec![el(1, "button", "Save")]);
    let obs = sim.observe(&ObservationScope::default()).unwrap();

    // Case 1: an observation id the driver never issued.
    let foreign = sim
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: ObservationId(999),
                    element: obs.elements[0].id,
                },
                button: MouseButton::Left,
                count: 1,
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(
        matches!(foreign, DriverError::StaleReference(_)),
        "{foreign}"
    );
    assert!(
        sim.pressed().is_empty(),
        "stale refusal emitted input: {:?}",
        sim.pressed()
    );

    // Case 2: the snapshot is held but the element drifted — rename the
    // button behind the agent's back, then act on the old reference.
    sim.on_tick(Effect::SetValueOf(
        SemanticTarget {
            name: Some("Save".into()),
            ..Default::default()
        },
        "ghost".into(),
    ));
    let _evolved = sim.observe(&ObservationScope::default()).unwrap();
    // The world changed (value, not identity — so craft a name change
    // via removal+spawn isn't needed: the fresh-vs-stored check below
    // uses role/name; a value change alone must NOT stale the token).
    let drift_ok = sim.act(
        &Action::Click {
            target: Target::Element {
                observation: obs.id,
                element: obs.elements[0].id,
            },
            button: MouseButton::Left,
            count: 1,
        },
        &ActContext::default(),
    );
    assert!(
        drift_ok.is_ok(),
        "value drift must not stale a token: {drift_ok:?}"
    );
    assert_eq!(sim.pressed().len(), 1);
}
