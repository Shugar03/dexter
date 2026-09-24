//! AppMap tests: the automatic interface cartography that answers
//! "what is this app and what can it do" from a single observation —
//! no hand-authored per-app knowledge.

use dexter_core::*;
use dexter_world_model::{app_map, normalize_ax_role};

fn el(id: u64, role: &str, name: Option<&str>) -> Element {
    Element {
        id: ElementId(id),
        role: Some(normalize_ax_role(role)),
        raw_role: Some(role.into()),
        name: name.map(Into::into),
        actions: vec!["press".into()],
        enabled: Some(true),
        ..Default::default()
    }
}

fn obs(elements: Vec<Element>) -> Observation {
    Observation {
        id: ObservationId(1),
        app: Some(AppSelector::Name("Test".into())),
        elements,
        ..Default::default()
    }
}

#[test]
fn calculator_surface_infers_calculator_capability() {
    let mut els = vec![el(1, "AXWindow", Some("Calculadora"))];
    for (i, d) in "0123456789".chars().enumerate() {
        els.push(el(10 + i as u64, "AXButton", Some(&d.to_string())));
    }
    for (i, op) in ["Sumar", "Restar", "Multiplicar", "Dividir", "Es igual a"]
        .iter()
        .enumerate()
    {
        els.push(el(30 + i as u64, "AXButton", Some(op)));
    }
    els.push(el(50, "AXStaticText", Some("0")));
    let map = app_map(&obs(els));
    assert!(
        map.capabilities.iter().any(|c| c.contains("calculator")),
        "expected calculator inference, got {:?}",
        map.capabilities
    );
}

#[test]
fn text_area_plus_save_menu_infers_document_editor() {
    let els = vec![
        el(1, "AXWindow", Some("Sin título")),
        el(2, "AXTextArea", None),
        el(3, "AXMenuItem", Some("Guardar")),
        el(4, "AXMenuItem", Some("Exportar como PDF…")),
    ];
    let map = app_map(&obs(els));
    assert!(map.capabilities.iter().any(|c| c.contains("document")));
}

#[test]
fn empty_tree_maps_to_empty_not_panic() {
    let map = app_map(&obs(vec![]));
    assert!(map.capabilities.is_empty());
    assert!(map.menu_verbs.is_empty());
    assert!(map.role_counts.is_empty());
}

#[test]
fn collects_verbs_controls_and_editables() {
    let els = vec![
        el(1, "AXWindow", Some("W")),
        el(2, "AXMenuItem", Some("Nuevo")),
        el(3, "AXMenuItem", Some("Abrir")),
        el(4, "AXButton", Some("Continuar")),
        el(5, "AXTextField", Some("Buscar")),
        el(6, "AXRadioButton", Some("Cronómetro")),
    ];
    let map = app_map(&obs(els));
    assert!(map.menu_verbs.contains(&"Nuevo".to_string()));
    assert!(map.controls.iter().any(|c| c.contains("Continuar")));
    assert!(map.editable.iter().any(|c| c.contains("Buscar")));
    assert!(map.navigation.iter().any(|c| c.contains("Cronómetro")));
}

#[test]
fn cg_window_without_ax_window_marks_limited() {
    let mut o = obs(vec![el(1, "AXMenuItem", Some("Nuevo"))]);
    o.windows = vec![Window {
        id: 1,
        pid: 1,
        app: "Test".into(),
        title: Some("W".into()),
        bounds: Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        },
        on_screen: true,
        layer: 0,
    }];
    let map = app_map(&o);
    assert!(map.ax_limited);
    // A genuinely empty screen (no CG windows) is not "limited".
    assert!(!app_map(&obs(vec![])).ax_limited);
}
