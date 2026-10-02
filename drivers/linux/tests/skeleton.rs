//! Skeleton contracts: every capability claim is honest (all false),
//! every operation declines with `Unsupported` rather than faking it,
//! and the AT-SPI role tables cover the roles desktop toolkits
//! (GTK, Qt, Firefox/Chromium) actually expose.

use dexter_core::{Action, AppSelector, MouseButton, ObservationScope, SemanticTarget, Target};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_linux::{atspi_role, atspi_role_name, LinuxDriver};

#[test]
fn capabilities_admit_nothing() {
    let caps = LinuxDriver::new().capabilities();
    assert_eq!(caps.name, "linux");
    // A skeleton that claimed element trees, screenshots or input
    // would be simulating — every flag stays false until a backend
    // earns it.
    assert!(!caps.element_tree);
    assert!(!caps.screenshots);
    assert!(!caps.background_input);
}

#[test]
fn every_entrypoint_is_honestly_unsupported() {
    let driver = LinuxDriver::new();
    assert!(matches!(driver.windows(), Err(DriverError::Unsupported(_))));
    assert!(matches!(
        driver.observe(&ObservationScope::default()),
        Err(DriverError::Unsupported(_))
    ));
    assert!(matches!(
        driver.act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget::default()),
                button: MouseButton::Left,
            },
            &ActContext::default(),
        ),
        Err(DriverError::Unsupported(_))
    ));
}

#[test]
fn wake_claims_no_activation() {
    // No window-manager backend: waking can't have activated anything.
    let handle = LinuxDriver::new()
        .wake(&AppSelector::Name("gedit".into()))
        .unwrap();
    assert!(!handle.activated);
}

#[test]
fn atspi_roles_cover_desktop_widgets() {
    for (name, role) in [
        ("button", "button"),
        ("push button", "button"),
        ("toggle button", "check_box"),
        ("check box", "check_box"),
        ("switch", "check_box"),
        ("radio button", "radio_button"),
        ("combo box", "combo_box"),
        ("entry", "text_field"),
        ("password text", "secure_text_field"),
        ("text", "text_area"),
        ("label", "static_text"),
        ("static", "static_text"),
        ("link", "link"),
        ("image", "image"),
        ("icon", "image"),
        ("list", "list"),
        ("list box", "list"),
        ("list item", "list_item"),
        ("menu", "menu"),
        ("popup menu", "menu"),
        ("menu bar", "menu_bar"),
        ("menu item", "menu_item"),
        ("check menu item", "menu_item"),
        ("radio menu item", "menu_item"),
        ("page tab", "tab"),
        ("page tab list", "tab_group"),
        ("progress bar", "progress_indicator"),
        ("slider", "slider"),
        ("spin button", "stepper"),
        ("scroll bar", "scroll_bar"),
        ("scroll pane", "scroll_area"),
        ("table", "table"),
        ("table cell", "cell"),
        ("table row", "row"),
        ("tree", "outline"),
        ("tree table", "outline"),
        ("tree item", "row"),
        ("tool bar", "toolbar"),
        ("tool tip", "tooltip"),
        ("status bar", "status_bar"),
        ("dialog", "dialog"),
        ("alert", "dialog"),
        ("file chooser", "dialog"),
        ("frame", "window"),
        ("window", "window"),
        ("panel", "group"),
        ("filler", "group"),
        ("heading", "heading"),
        ("document web", "web_area"),
        ("application", "application"),
    ] {
        assert_eq!(atspi_role(name), Some(role), "{name}");
    }
    // Roles with no semantic affordance stay unmapped — the element
    // keeps its `raw_role` instead of wearing a guessed one.
    for unmapped in [
        "invalid",
        "unknown",
        "redundant object",
        "extended",
        "canvas",
        "drawing area",
        "separator",
        "terminal",
        "Push Button",
        "",
    ] {
        assert_eq!(atspi_role(unmapped), None, "{unmapped:?}");
    }
}

#[test]
fn role_ids_map_to_canonical_names() {
    // `AtspiRole` values are stable D-Bus protocol constants
    // (atspi-constants.h); `Accessible.GetRole` returns them as u32.
    for (id, name) in [
        (0, "invalid"),
        (2, "alert"),
        (7, "check box"),
        (8, "check menu item"),
        (11, "combo box"),
        (16, "dialog"),
        (23, "frame"),
        (27, "image"),
        (29, "label"),
        (31, "list"),
        (32, "list item"),
        (35, "menu item"),
        (37, "page tab"),
        (38, "page tab list"),
        (39, "panel"),
        (40, "password text"),
        (42, "progress bar"),
        (43, "button"),
        (44, "radio button"),
        (51, "slider"),
        (52, "spin button"),
        (55, "table"),
        (56, "table cell"),
        (61, "text"),
        (62, "toggle button"),
        (65, "tree"),
        (67, "unknown"),
        (69, "window"),
        (75, "application"),
        (79, "entry"),
        (83, "heading"),
        (88, "link"),
        (90, "table row"),
        (91, "tree item"),
        (95, "document web"),
        (98, "list box"),
        (104, "title bar"),
        (116, "static"),
        (129, "push button menu"),
        (130, "switch"),
    ] {
        assert_eq!(atspi_role_name(id), Some(name), "id {id}");
    }
    // `ATSPI_ROLE_LAST_DEFINED` (131) and beyond are not roles.
    assert_eq!(atspi_role_name(131), None);
    assert_eq!(atspi_role_name(u32::MAX), None);
}

#[test]
fn every_role_id_has_a_name_and_names_are_unique() {
    let names: Vec<&str> = (0..131)
        .map(|id| atspi_role_name(id).unwrap_or_else(|| panic!("id {id}")))
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), names.len(), "duplicate role names");
}

#[test]
fn interactive_role_ids_reach_a_normalized_role() {
    // The two tables compose for every role a user can act on.
    for id in [
        7, 8, 11, 16, 23, 31, 32, 33, 34, 35, 37, 38, 40, 41, 42, 43, 44, 45, 48, 51, 52, 55, 56,
        61, 62, 65, 69, 79, 88, 91, 98, 130,
    ] {
        let name = atspi_role_name(id).unwrap();
        assert!(
            atspi_role(name).is_some(),
            "AT-SPI role {name:?} (id {id}) has no normalized role"
        );
    }
}
