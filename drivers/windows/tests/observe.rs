//! Real-backend tests for the UIA observe slice. Only compiled on
//! Windows — CI (macOS) sees an empty target; these run where the
//! backend is real.

#![cfg(windows)]

use dexter_core::{AppSelector, ElementSource, ObservationScope};
use dexter_driver::{ComputerDriver, DriverError};
use dexter_windows::WindowsDriver;

/// A windowed pid to scope against: the first visible top-level window
/// the system reports. Desktop sessions always have at least one
/// (shell/taskbar windows); asserts skip gracefully otherwise.
fn some_window_pid(driver: &WindowsDriver) -> Option<i32> {
    driver
        .windows()
        .ok()?
        .into_iter()
        .find(|w| w.pid > 0)
        .map(|w| w.pid)
}

#[test]
fn windows_lists_top_level_windows_with_real_fields() {
    let windows = WindowsDriver::new().windows().expect("EnumWindows works");
    assert!(
        !windows.is_empty(),
        "a desktop session has top-level windows"
    );
    for w in &windows {
        assert_ne!(w.id, 0, "HWND truncated to zero");
        assert!(w.pid > 0, "window without an owning pid");
        assert!(!w.app.is_empty(), "window without a process name");
    }
}

#[test]
fn observe_scoped_app_walks_uia_tree() {
    let driver = WindowsDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let obs = driver
        .observe(&ObservationScope::for_app(AppSelector::Pid(pid)))
        .expect("observe a windowed pid");
    assert_eq!(obs.pid, Some(pid));
    assert!(!obs.windows.is_empty());
    assert!(obs.windows.iter().all(|w| w.pid == pid));
    assert!(
        !obs.elements.is_empty(),
        "UIA must produce elements for a windowed app"
    );
    // Roots are the app's own windows, at depth 0, from UIA.
    let roots: Vec<_> = obs.elements.iter().filter(|e| e.depth == 0).collect();
    assert!(!roots.is_empty());
    for e in &obs.elements {
        assert!(matches!(e.source, ElementSource::Accessibility));
    }
    assert!(
        obs.elements.iter().any(|e| e.role.is_some()),
        "at least one ControlType must map through uia_role"
    );
    assert!(!obs.digest.is_empty());
    // Windows whose UIA tree came back get listed; an app with windows
    // but zero elements must degrade honestly, not read as "empty app".
    assert!(!obs.ax_limited);
}

#[test]
fn observe_window_scope_is_native() {
    let driver = WindowsDriver::new();
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
    // Natively scoped: exactly the requested window remains, so the
    // caller's `scope_to_window` post-filter is a verified no-op.
    assert_eq!(obs.windows.len(), 1);
    assert_eq!(obs.windows[0].id, win.id);
    assert!(!obs.elements.is_empty());
    assert!(dexter_world_model::scope_to_window(obs, win.id).is_ok());
}

#[test]
fn observe_unknown_app_fails_closed() {
    let driver = WindowsDriver::new();
    let err = driver
        .observe(&ObservationScope::for_app(AppSelector::Name(
            "dexter-definitely-not-running-app".into(),
        )))
        .unwrap_err();
    assert!(matches!(err, DriverError::AppNotFound(_)), "{err:?}");
}

#[test]
fn observe_unknown_window_fails_closed() {
    let driver = WindowsDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let mut scope = ObservationScope::for_app(AppSelector::Pid(pid));
    scope.window = Some(u32::MAX);
    let err = driver.observe(&scope).unwrap_err();
    assert!(matches!(err, DriverError::NotFound(_)), "{err:?}");
}

#[test]
fn perception_scopes_capture_or_fail_closed() {
    let driver = WindowsDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };

    // scope.screenshot: a capturable window on a display-carrying box
    // produces a real PNG at the contracted path; a displayless host
    // declines with a platform error — never a partial or guessed image.
    let mut scope = ObservationScope::for_app(AppSelector::Pid(pid));
    scope.screenshot = true;
    match driver.observe(&scope) {
        Ok(obs) => {
            let path = obs.screenshot.expect("screenshot scope sets a path");
            let png = std::fs::read(&path).expect("screenshot file exists");
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "not a PNG");
        }
        Err(DriverError::Platform(_)) | Err(DriverError::NotFound(_)) => {}
        Err(e) => panic!("screenshot declined dishonestly: {e:?}"),
    }

    // scope.vision: narrowed to one on-screen window so the opt-in
    // fires. The pass degrades via collection_errors rather than
    // failing, and whatever `source: ocr` elements land are inert —
    // evidence, never agency.
    let first = driver
        .observe(&ObservationScope::for_app(AppSelector::Pid(pid)))
        .expect("windowed pid observes");
    let Some(wid) = first.windows.iter().find(|w| w.on_screen).map(|w| w.id) else {
        return;
    };
    let mut scope = ObservationScope::for_app(AppSelector::Pid(pid));
    scope.vision = true;
    scope.window = Some(wid);
    let obs = driver
        .observe(&scope)
        .expect("vision degrades, never fails");
    for e in obs
        .elements
        .iter()
        .filter(|e| e.source == ElementSource::Ocr)
    {
        assert!(e.actions.is_empty(), "ocr elements are inert: {e:?}");
    }
}

#[test]
fn name_selector_resolves_windowed_process() {
    let driver = WindowsDriver::new();
    let Some(pid) = some_window_pid(&driver) else {
        return;
    };
    let app = driver
        .windows()
        .unwrap()
        .into_iter()
        .find(|w| w.pid == pid)
        .unwrap()
        .app;
    let obs = driver
        .observe(&ObservationScope::for_app(AppSelector::Name(app)))
        .expect("name resolves to the same pid");
    assert_eq!(obs.pid, Some(pid));
}
