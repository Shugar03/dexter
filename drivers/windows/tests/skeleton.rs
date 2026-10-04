//! Skeleton contracts: capability claims stay honest per platform,
//! the UIA role/control-type tables cover the control types desktop
//! apps actually surface, and every entrypoint that has no real
//! backend yet declines with `Unsupported` rather than faking it.

use dexter_core::{Action, MouseButton, SemanticTarget, Target};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_windows::{control_type_name, uia_role, WindowsDriver};

#[test]
fn capabilities_match_the_platform_backend() {
    let caps = WindowsDriver::new().capabilities();
    assert_eq!(caps.name, "windows");
    // On Windows the UIA tree is a real read path — the flag is a fact,
    // not a grant. Off Windows there is no backend to claim.
    assert_eq!(caps.element_tree, cfg!(windows));
    // Not earned yet in either world: no capture, no input slice.
    assert!(!caps.screenshots);
    assert!(!caps.background_input);
}

/// Off Windows every entrypoint must still decline honestly — the
/// skeleton contract never goes away on platforms without a backend.
#[cfg(not(windows))]
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
        .observe(&dexter_core::ObservationScope::default())
        .map(|_| ())
        .map_err(unsupported)
        .err()
        .unwrap());
}

/// Off Windows every entrypoint must still decline honestly — act is
/// part of the skeleton contract on platforms without a backend.
#[cfg(not(windows))]
#[test]
fn act_is_honestly_unsupported() {
    let driver = WindowsDriver::new();
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

/// On Windows the act slice is real — a semantic click without an app
/// scope fails closed (`NotFound`), never acts on an arbitrary app.
#[cfg(windows)]
#[test]
fn act_fails_closed_without_app_scope() {
    let driver = WindowsDriver::new();
    let err = driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    ..SemanticTarget::default()
                }),
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(err, DriverError::NotFound(_)), "{err:?}");
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

#[test]
fn control_type_ids_map_to_programmatic_names() {
    // UIA ControlType ids are stable protocol constants (UIA_BUTTONCONTROLTYPEID
    // & friends). The table must cover the set desktop providers emit.
    for (id, name) in [
        (50000, "Button"),
        (50001, "Calendar"),
        (50002, "CheckBox"),
        (50003, "ComboBox"),
        (50004, "Edit"),
        (50005, "Hyperlink"),
        (50006, "Image"),
        (50007, "ListItem"),
        (50008, "List"),
        (50009, "Menu"),
        (50010, "MenuBar"),
        (50011, "MenuItem"),
        (50012, "ProgressBar"),
        (50013, "RadioButton"),
        (50014, "ScrollBar"),
        (50015, "Slider"),
        (50016, "Spinner"),
        (50017, "StatusBar"),
        (50018, "Tab"),
        (50019, "TabItem"),
        (50020, "Text"),
        (50021, "ToolBar"),
        (50022, "ToolTip"),
        (50023, "Tree"),
        (50024, "TreeItem"),
        (50026, "Group"),
        (50027, "Thumb"),
        (50028, "DataGrid"),
        (50029, "DataItem"),
        (50030, "Document"),
        (50031, "SplitButton"),
        (50032, "Window"),
        (50033, "Pane"),
        (50034, "Header"),
        (50035, "HeaderItem"),
        (50036, "Table"),
        (50037, "TitleBar"),
    ] {
        assert_eq!(control_type_name(id), Some(name), "id {id}");
    }
    // Ids outside the enum stay unmapped — no invented names.
    assert_eq!(control_type_name(50025), Some("Custom"));
    assert_eq!(control_type_name(49999), None);
    assert_eq!(control_type_name(59999), None);
}

#[test]
fn mapped_control_types_reach_a_normalized_role() {
    // The two tables compose: every ControlType we bother naming (minus
    // the explicitly-unmapped ones) resolves to a normalized role.
    let unmapped = [50025, 50039, 50040]; // Custom, SemanticZoom, AppBar
    for id in 50000..=50037 {
        if unmapped.contains(&id) {
            continue;
        }
        let name = control_type_name(id).unwrap_or_else(|| panic!("id {id}"));
        assert!(
            uia_role(name).is_some(),
            "ControlType.{name} (id {id}) has no normalized role"
        );
    }
}
