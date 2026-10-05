//! Real-backend tests for the AT-SPI2 observe slice. Only compiled on
//! Linux — CI (macOS) sees an empty target. They need a live a11y bus
//! with at least one registered application (a GTK app started while
//! `at-spi-bus-launcher` runs); without one they report the skip on
//! stderr rather than asserting against nothing.

#![cfg(target_os = "linux")]

use dexter_core::{AppSelector, ElementSource, ObservationScope};
use dexter_driver::{ComputerDriver, DriverError};
use dexter_linux::LinuxDriver;

/// A pid to scope against: the first registered application owning a
/// listed window. `None` (with a stderr note) when the bus or apps are
/// absent — the test then has nothing honest to assert.
fn some_window_pid(driver: &LinuxDriver) -> Option<i32> {
    match driver.windows() {
        Ok(ws) => {
            let pid = ws.iter().find(|w| w.pid > 0).map(|w| w.pid);
            if pid.is_none() {
                eprintln!("skipped: a11y bus reachable but no application registered");
            }
            pid
        }
        Err(e) => {
            eprintln!("skipped: no a11y bus ({e})");
            None
        }
    }
}

#[test]
fn capabilities_follow_the_bus() {
    let driver = LinuxDriver::new();
    let caps = driver.capabilities();
    assert_eq!(caps.name, "linux");
    // The element-tree claim is exactly "the a11y bus answers" — the
    // same probe `windows()` relies on, never a static true.
    assert_eq!(caps.element_tree, driver.windows().is_ok());
    assert!(!caps.screenshots);
    assert!(!caps.background_input);
}

#[test]
fn windows_lists_registered_frames_with_real_fields() {
    let driver = LinuxDriver::new();
    if some_window_pid(&driver).is_none() {
        return;
    }
    let windows = driver.windows().unwrap();
    assert!(!windows.is_empty());
    for w in &windows {
        assert_ne!(w.id, 0, "window id must be a real hash");
        assert!(w.pid > 0, "window without an owning pid");
        assert!(!w.app.is_empty(), "window without an application name");
        assert!(
            w.bounds.w > 0.0 && w.bounds.h > 0.0,
            "unplaced frame listed"
        );
        assert_eq!(w.layer, 0);
        assert!(w.bundle_id.is_none(), "linux has no bundle ids to claim");
    }
    let mut ids: Vec<u32> = windows.iter().map(|w| w.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), windows.len(), "window ids collide");
}

#[test]
fn observe_scoped_app_walks_atspi_tree() {
    let driver = LinuxDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let obs = driver
        .observe(&ObservationScope::for_app(AppSelector::Pid(pid)))
        .expect("observe a registered pid");
    assert_eq!(obs.pid, Some(pid));
    assert!(!obs.windows.is_empty());
    assert!(obs.windows.iter().all(|w| w.pid == pid));
    assert!(
        !obs.elements.is_empty(),
        "AT-SPI must produce elements for a registered app"
    );
    let roots: Vec<_> = obs.elements.iter().filter(|e| e.depth == 0).collect();
    assert_eq!(roots.len(), obs.windows.len(), "one root per window");
    for e in &obs.elements {
        assert!(matches!(e.source, ElementSource::Accessibility));
        if let Some(p) = e.parent {
            let parent = obs
                .elements
                .iter()
                .find(|x| x.id == p)
                .expect("parent listed");
            assert_eq!(parent.depth + 1, e.depth);
        }
        // Only showing elements are walked, so every element with
        // bounds has a real extent.
        if let Some(b) = e.bounds {
            assert!(b.w > 0.0 && b.h > 0.0);
        }
    }
    assert!(
        obs.elements.iter().any(|e| e.role.is_some()),
        "at least one role must map through atspi_role"
    );
    assert!(
        obs.elements
            .iter()
            .any(|e| e.actions.iter().any(|a| a == "press")),
        "a GTK window has at least one clickable control"
    );
    assert!(!obs.digest.is_empty());
    assert!(!obs.ax_limited);
    assert!(!obs.elements_truncated);
    eprintln!("{}", obs.digest);
}

#[test]
fn observe_window_scope_is_native() {
    let driver = LinuxDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let wins = driver.windows().unwrap();
    let win = wins
        .iter()
        .find(|w| w.pid == pid && w.on_screen)
        .or_else(|| wins.iter().find(|w| w.pid == pid))
        .expect("the pid we chose has a window");
    let mut scope = ObservationScope::for_app(AppSelector::Pid(pid));
    scope.window = Some(win.id);
    let obs = driver.observe(&scope).expect("window-scoped observe");
    assert_eq!(obs.windows.len(), 1);
    assert_eq!(obs.windows[0].id, win.id);
    assert!(!obs.elements.is_empty());
    assert_eq!(obs.elements.iter().filter(|e| e.depth == 0).count(), 1);
    assert!(dexter_world_model::scope_to_window(obs, win.id).is_ok());
}

#[test]
fn truncation_is_explicit() {
    let driver = LinuxDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let mut scope = ObservationScope::for_app(AppSelector::Pid(pid));
    scope.max_elements = 3;
    let obs = driver.observe(&scope).expect("capped observe");
    assert!(obs.elements.len() <= 3);
    assert!(
        obs.elements_truncated,
        "a 3-element cap on a GTK window must truncate"
    );
}

#[test]
fn observe_unknown_app_fails_closed() {
    let driver = LinuxDriver::new();
    if some_window_pid(&driver).is_none() {
        return;
    }
    let err = driver
        .observe(&ObservationScope::for_app(AppSelector::Name(
            "dexter-definitely-not-running-app".into(),
        )))
        .unwrap_err();
    assert!(matches!(err, DriverError::AppNotFound(_)), "{err:?}");
    // A pid that owns no registered application is not observable.
    let err = driver
        .observe(&ObservationScope::for_app(AppSelector::Pid(i32::MAX)))
        .unwrap_err();
    assert!(matches!(err, DriverError::AppNotFound(_)), "{err:?}");
}

#[test]
fn bundle_ids_are_unsupported_not_guessed() {
    let driver = LinuxDriver::new();
    if some_window_pid(&driver).is_none() {
        return;
    }
    let err = driver
        .observe(&ObservationScope::for_app(AppSelector::BundleId(
            "org.gnome.gedit".into(),
        )))
        .unwrap_err();
    assert!(matches!(err, DriverError::Unsupported(_)), "{err:?}");
}

#[test]
fn observe_unknown_window_fails_closed() {
    let driver = LinuxDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let mut scope = ObservationScope::for_app(AppSelector::Pid(pid));
    scope.window = Some(u32::MAX);
    let err = driver.observe(&scope).unwrap_err();
    assert!(matches!(err, DriverError::NotFound(_)), "{err:?}");
}

#[test]
fn name_selector_resolves_registered_app() {
    let driver = LinuxDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let windows = driver.windows().unwrap();
    let app = windows.iter().find(|w| w.pid == pid).unwrap().app.clone();
    let mut pids: Vec<i32> = windows
        .iter()
        .filter(|w| w.app.eq_ignore_ascii_case(&app))
        .map(|w| w.pid)
        .collect();
    pids.sort_unstable();
    pids.dedup();
    let result = driver.observe(&ObservationScope::for_app(AppSelector::Name(app)));
    if pids.len() == 1 {
        assert_eq!(
            result.expect("name resolves to the same pid").pid,
            Some(pid)
        );
    } else {
        // Two instances of the same app: a name is not a target.
        assert!(
            matches!(result, Err(DriverError::Ambiguous(_))),
            "{result:?}"
        );
    }
}

#[test]
fn act_stays_unsupported() {
    use dexter_core::{Action, MouseButton, SemanticTarget, Target};
    use dexter_driver::ActContext;
    let err = LinuxDriver::new()
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget::default()),
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(err, DriverError::Unsupported(_)));
}
