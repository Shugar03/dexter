//! World signature contract — the change detector behind per-act
//! verification. Order-independent, id-free, bounds on an 8px grid.

use dexter_core::*;

fn el(id: u64, role: &str, name: &str) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        ..Default::default()
    }
}

fn obs(elements: Vec<Element>) -> Observation {
    Observation {
        elements,
        ..Default::default()
    }
}

#[test]
fn signature_is_order_and_id_independent() {
    let a = obs(vec![el(1, "button", "Save"), el(2, "text_field", "q")]);
    // Reordered with fresh ids — same world, same signature.
    let b = obs(vec![el(9, "text_field", "q"), el(8, "button", "Save")]);
    assert_eq!(
        dexter_world_model::signature(&a),
        dexter_world_model::signature(&b)
    );
}

#[test]
fn signature_ignores_subcell_jitter_catches_real_moves() {
    let mut e = el(1, "button", "Save");
    e.bounds = Some(Rect {
        x: 10.0,
        y: 20.0,
        w: 40.0,
        h: 20.0,
    });
    let before = obs(vec![e.clone()]);
    // 3px move stays inside the same 8px cells — jitter, not change.
    let mut j = e.clone();
    j.bounds = Some(Rect {
        x: 12.0,
        y: 22.0,
        w: 40.0,
        h: 20.0,
    });
    assert_eq!(
        dexter_world_model::signature(&before),
        dexter_world_model::signature(&obs(vec![j]))
    );
    // 20px crosses cells — a real move (scroll-into-view, reflow).
    let mut moved = e.clone();
    moved.bounds = Some(Rect {
        x: 30.0,
        y: 20.0,
        w: 40.0,
        h: 20.0,
    });
    assert_ne!(
        dexter_world_model::signature(&before),
        dexter_world_model::signature(&obs(vec![moved]))
    );
}

#[test]
fn signature_tracks_values_screenshots_and_membership() {
    let base = obs(vec![el(1, "button", "Save")]);
    let mut changed_value = obs(vec![{
        let mut e = el(1, "button", "Save");
        e.value = Some("clicked".into());
        e
    }]);
    assert_ne!(
        dexter_world_model::signature(&base),
        dexter_world_model::signature(&changed_value)
    );
    // Screenshot presence is part of the world.
    changed_value.screenshot = Some("shot.png".into());
    assert_ne!(
        dexter_world_model::signature(&changed_value),
        dexter_world_model::signature(&obs(vec![{
            let mut e = el(1, "button", "Save");
            e.value = Some("clicked".into());
            e
        }]))
    );
    // Membership — a spawned element moves the signature even when it
    // shares role/name with nothing comparable.
    let spawned = obs(vec![el(1, "button", "Save"), el(2, "static_text", "Done")]);
    assert_ne!(
        dexter_world_model::signature(&base),
        dexter_world_model::signature(&spawned)
    );
}

#[test]
fn signature_excludes_menu_catalog() {
    let with_menu = obs(vec![
        el(1, "button", "Save"),
        {
            let mut m = el(2, "menu_item", "Print");
            m.raw_role = Some("AXMenuItem".into());
            m
        },
        {
            let mut m = el(3, "menu_bar_item", "File");
            m.raw_role = Some("AXMenuBarItem".into());
            m
        },
    ]);
    let without_menu = obs(vec![el(9, "button", "Save")]);
    // Menu presence/absence or menu-item state is catalog noise, not a
    // world change — this is what lets verification re-observes skip
    // the menu-bar walk and stay signature-comparable.
    assert_eq!(
        dexter_world_model::signature(&with_menu),
        dexter_world_model::signature(&without_menu)
    );
    // A window element still moves it.
    let changed = obs(vec![el(9, "button", "Save"), el(10, "button", "OK")]);
    assert_ne!(
        dexter_world_model::signature(&without_menu),
        dexter_world_model::signature(&changed)
    );
}
