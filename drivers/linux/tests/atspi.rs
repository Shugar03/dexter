//! Protocol-level contracts of the AT-SPI2 observe slice — decidable
//! on every host: the `au` state bitfield, `GetExtents` → `Rect`,
//! `Action.GetName` → the shared action vocabulary, toggle values,
//! window ids and app-name matching never touch a bus.

use dexter_linux::atspi::{
    action_name, app_name_matches, extents_rect, toggle_value, window_id, States,
};

/// gtk3-widget-factory's main frame as the bus reports it: ACTIVE,
/// ENABLED, RESIZABLE, SENSITIVE, SHOWING, VISIBLE.
const FRAME: [u32; 2] = [1_126_170_882, 0];

#[test]
fn states_decode_the_two_word_bitfield() {
    let s = States::from_words(&FRAME);
    assert!(s.has(States::ACTIVE));
    assert!(s.has(States::ENABLED));
    assert!(s.has(States::SENSITIVE));
    assert!(s.has(States::SHOWING));
    assert!(s.has(States::VISIBLE));
    assert!(!s.has(States::FOCUSED));
    assert!(!s.has(States::ICONIFIED));
    assert!(!s.has(States::DEFUNCT));
    // Bits 32+ live in the second word.
    let s = States::from_words(&[0, 1 << (States::INDETERMINATE - 32)]);
    assert!(s.has(States::INDETERMINATE));
    assert!(!s.has(States::CHECKED));
    // A short or empty array is "no states", never a panic.
    assert!(!States::from_words(&[]).has(States::VISIBLE));
    assert!(!States::from_words(&[1 << States::VISIBLE]).has(States::SHOWING));
}

#[test]
fn enabled_needs_both_enabled_and_sensitive() {
    // GTK clears SENSITIVE (not ENABLED) on insensitive widgets —
    // either missing means the control will not take input.
    let both = States::from_words(&[(1 << States::ENABLED) | (1 << States::SENSITIVE)]);
    assert_eq!(both.enabled(), Some(true));
    let only_enabled = States::from_words(&[1 << States::ENABLED]);
    assert_eq!(only_enabled.enabled(), Some(false));
    let only_sensitive = States::from_words(&[1 << States::SENSITIVE]);
    assert_eq!(only_sensitive.enabled(), Some(false));
}

#[test]
fn showing_and_on_screen_rules() {
    let showing = States::from_words(&[(1 << States::SHOWING) | (1 << States::VISIBLE)]);
    assert!(showing.showing());
    assert!(showing.on_screen());
    // VISIBLE without SHOWING: a widget inside a hidden page/dialog —
    // GTK reports the i32::MIN sentinel for its extents.
    let hidden = States::from_words(&[1 << States::VISIBLE]);
    assert!(!hidden.showing());
    // An iconified frame still "shows" in AT-SPI terms but is not on
    // screen — same honesty as a minimized HWND.
    let iconified = States::from_words(&[(1 << States::SHOWING)
        | (1 << States::VISIBLE)
        | (1 << States::ICONIFIED)]);
    assert!(iconified.showing());
    assert!(!iconified.on_screen());
}

#[test]
fn extents_reject_unplaced_and_degenerate_rects() {
    assert_eq!(
        extents_rect((896, 789, 1408, 784)),
        Some(dexter_core::Rect {
            x: 896.0,
            y: 789.0,
            w: 1408.0,
            h: 784.0
        })
    );
    // GTK's "not realized" sentinel: G_MININT origin, 1×1 — a hidden
    // dialog, not a pixel at the far corner of the universe.
    assert_eq!(extents_rect((i32::MIN, i32::MIN, 1, 1)), None);
    assert_eq!(extents_rect((10, 10, 0, 30)), None);
    assert_eq!(extents_rect((10, 10, 30, -1)), None);
}

#[test]
fn action_names_normalize_to_the_shared_vocabulary() {
    // GTK/ATK verbs → the vocabulary the engine, AX, DOM and UIA
    // walkers share.
    for (raw, want) in [
        ("click", "press"),
        ("press", "press"),
        ("activate", "press"),
        ("toggle", "press"),
        ("jump", "press"),
        ("release", "press"),
        ("expand or contract", "expand_collapse"),
        ("expand", "expand_collapse"),
        ("collapse", "expand_collapse"),
        ("menu", "show_menu"),
        ("show menu", "show_menu"),
        ("showMenu", "show_menu"),
        ("Click", "press"),
    ] {
        assert_eq!(action_name(raw), Some(want), "{raw:?}");
    }
    // Unknown verbs are not guessed into `press`.
    for raw in ["", "dance", "customAction"] {
        assert_eq!(action_name(raw), None, "{raw:?}");
    }
}

#[test]
fn toggle_values_only_for_toggle_roles() {
    let checked = States::from_words(&[1 << States::CHECKED]);
    let pressed = States::from_words(&[1 << States::PRESSED]);
    let indeterminate = States::from_words(&[0, 1 << (States::INDETERMINATE - 32)]);
    let none = States::from_words(&[1 << States::ENABLED]);
    for raw in [
        "check box",
        "radio button",
        "toggle button",
        "switch",
        "check menu item",
        "radio menu item",
    ] {
        assert_eq!(toggle_value(raw, checked).as_deref(), Some("on"), "{raw}");
        assert_eq!(toggle_value(raw, pressed).as_deref(), Some("on"), "{raw}");
        assert_eq!(toggle_value(raw, none).as_deref(), Some("off"), "{raw}");
        assert_eq!(
            toggle_value(raw, indeterminate).as_deref(),
            Some("indeterminate"),
            "{raw}"
        );
    }
    // A plain button that happens to carry PRESSED mid-click is not a
    // toggle — no invented "on"/"off".
    assert_eq!(toggle_value("button", pressed), None);
    assert_eq!(toggle_value("label", checked), None);
}

#[test]
fn window_ids_are_stable_and_distinct() {
    let a = window_id(":1.7", "/org/a11y/atspi/accessible/1");
    assert_eq!(a, window_id(":1.7", "/org/a11y/atspi/accessible/1"));
    assert_ne!(a, 0, "zero is the 'no window' id in callers");
    assert_ne!(a, window_id(":1.7", "/org/a11y/atspi/accessible/2"));
    assert_ne!(a, window_id(":1.8", "/org/a11y/atspi/accessible/1"));
}

#[test]
fn app_name_matches_toolkit_name_or_comm_case_insensitively() {
    assert!(app_name_matches("gedit", Some("gedit"), None));
    assert!(app_name_matches("GEDIT", Some("gedit"), None));
    assert!(!app_name_matches(
        "gtk3-widget-factory",
        None,
        Some("gtk3-widget-fac")
    ));
    assert!(app_name_matches(
        "gtk3-widget-fac",
        None,
        Some("gtk3-widget-fac")
    ));
    assert!(!app_name_matches(
        "gedit",
        Some("gnome-text-editor"),
        Some("gnome-text-edi")
    ));
    // Nothing known about the app → never a match.
    assert!(!app_name_matches("gedit", None, None));
    assert!(!app_name_matches("", Some(""), None));
}
