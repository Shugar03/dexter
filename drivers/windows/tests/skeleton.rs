//! Skeleton contracts: every capability claim is honest (all false),
//! every operation declines with `Unsupported` rather than faking it,
//! and the UIA role table covers the control types desktop apps
//! actually surface.

use dexter_core::{Action, MouseButton, ObservationScope, SemanticTarget, Target};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_windows::{uia_role, WindowsDriver};

#[test]
fn capabilities_admit_nothing() {
    let caps = WindowsDriver::new().capabilities();
    assert_eq!(caps.name, "windows");
    // A skeleton that claimed screenshots or element trees would be
    // simulating — all flags stay false until a backend earns them.
    assert!(!caps.element_tree);
    assert!(!caps.screenshots);
    assert!(!caps.background_input);
}

#[test]
fn every_entrypoint_is_honestly_unsupported() {
    let driver = WindowsDriver::new();
    let unsupported = |e: DriverError| matches!(e, DriverError::Unsupported(_));
    assert!(driver
        .windows()
        .map(|_| ())
        .map_err(unsupported)
        .err()
        .unwrap());
    assert!(driver
        .observe(&ObservationScope::default())
        .map(|_| ())
        .map_err(unsupported)
        .err()
        .unwrap());
    assert!(driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget::default()),
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .map(|_| ())
        .map_err(unsupported)
        .err()
        .unwrap());
}

#[test]
fn uia_roles_cover_desktop_control_types() {
    for (control, role) in [
        ("Button", "button"),
        ("Edit", "text_field"),
        ("Text", "static_text"),
        ("CheckBox", "check_box"),
        ("ComboBox", "combo_box"),
        ("List", "list"),
        ("ListItem", "list_item"),
        ("MenuItem", "menu_item"),
        ("ProgressBar", "progress_indicator"),
        ("Slider", "slider"),
        ("TabItem", "tab"),
        ("Window", "window"),
        ("Document", "text_area"),
        ("Hyperlink", "link"),
        ("Image", "image"),
    ] {
        assert_eq!(uia_role(control), Some(role), "{control}");
    }
    // Unknown control types stay unmapped — no guessed roles.
    assert_eq!(uia_role("Custom"), None);
    assert_eq!(uia_role("SemanticZoom"), None);
}
