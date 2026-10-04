//! Real-backend tests for the UIA act slice. Only compiled on Windows —
//! CI (macOS/Ubuntu) sees an empty target; these run where UIA and a
//! desktop session exist. Each test drives its own `notepad.exe`
//! instance scoped by pid, so they can run in parallel.

#![cfg(windows)]

use dexter_core::{
    Action, ActionStatus, AppSelector, MouseButton, ObservationScope, SemanticTarget, Target,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_windows::WindowsDriver;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// `notepad.exe` is the stable built-in target: a plain Win32 app whose
/// edit control exposes a writable `ValuePattern` and whose title bar
/// carries Invoke-able buttons.
struct Notepad(Child);

impl Notepad {
    fn launch() -> Option<Self> {
        Command::new("notepad.exe").spawn().ok().map(Self)
    }
}

impl Drop for Notepad {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait until the child's top-level window is enumerable — UIA anchors
/// on HWNDs, so acting before the window exists resolves nothing.
fn wait_for_window(driver: &WindowsDriver, pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(wins) = driver.windows() {
            if wins.iter().any(|w| w.pid == pid) {
                return true;
            }
        }
        if Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn ctx_for(pid: i32) -> ActContext {
    ActContext {
        app: Some(AppSelector::Pid(pid)),
        allow_coordinates: false,
    }
}

/// The element that accepts `set_value` — notepad's edit/document
/// surface, whatever ControlType the local build reports.
fn find_editable(obs: &dexter_core::Observation) -> Option<dexter_core::ElementId> {
    obs.elements
        .iter()
        .find(|e| e.actions.iter().any(|a| a == "set_value"))
        .map(|e| e.id)
}

fn find_minimize(obs: &dexter_core::Observation) -> Option<&dexter_core::Element> {
    obs.elements.iter().find(|e| {
        let named = e
            .identifier
            .as_deref()
            .is_some_and(|i| i.eq_ignore_ascii_case("minimize"))
            || e.name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains("minimiz"));
        named && e.role.as_deref() == Some("button")
    })
}

#[test]
fn set_value_through_element_target_roundtrips() {
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let edit = find_editable(&obs).expect("notepad exposes a set_value element");

    let res = driver
        .act(
            &Action::SetValue {
                target: Target::Element {
                    observation: obs.id,
                    element: edit,
                },
                value: "hello from dexter".into(),
            },
            &ctx_for(pid),
        )
        .expect("set_value resolves");
    assert_eq!(res.status, ActionStatus::Success, "{res:?}");

    // Verify through a fresh observation — the write is only real if a
    // new walk reports it.
    let obs2 = driver.observe(&scope).expect("re-observe notepad");
    let edit2 = find_editable(&obs2).expect("edit still there");
    let el2 = obs2.element(edit2).expect("element lookup");
    assert!(
        el2.value
            .as_deref()
            .is_some_and(|v| v.contains("hello from dexter")),
        "value after SetValue: {:?}",
        el2.value
    );
    let _ = np.0.kill();
}

#[test]
fn press_via_element_target_invokes_minimize() {
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let min = find_minimize(&obs)
        .expect("notepad title bar has a minimize button")
        .clone();
    let win_id = obs
        .windows
        .iter()
        .find(|w| w.pid == pid)
        .map(|w| w.id)
        .expect("notepad window id");

    let res = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: min.id,
                },
                button: MouseButton::Left,
            },
            &ctx_for(pid),
        )
        .expect("click resolves");
    assert_eq!(res.status, ActionStatus::Success, "{res:?}");

    // The window must actually leave the on-screen set — a real
    // minimize, not a pattern call that returned politely.
    let deadline = Instant::now() + Duration::from_secs(5);
    let minimized = loop {
        let wins = driver.windows().expect("EnumWindows works");
        match wins.iter().find(|w| w.id == win_id) {
            Some(w) if w.on_screen => {
                if Instant::now() > deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Some(w) => break !w.on_screen,
            None => break true,
        }
    };
    assert!(minimized, "notepad window stayed on screen after Minimize");
    let _ = np.0.kill();
}

#[test]
fn semantic_target_resolves_and_focuses() {
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let edit = obs
        .elements
        .iter()
        .find(|e| e.actions.iter().any(|a| a == "set_value"))
        .expect("notepad edit surface");
    let role = edit.role.clone().expect("edit has a role");
    let win_id = obs
        .windows
        .iter()
        .find(|w| w.pid == pid)
        .map(|w| w.id)
        .expect("notepad window id");

    // Keyboard focus only lands inside an active window — raise first,
    // retrying a little: foreground rights can free up between attempts.
    // Some sessions (headless, service window stations) refuse every
    // raise: there the honest verdict is `Failed`, and real keyboard
    // focus cannot be delivered at all — the focused-flag check only
    // makes sense once a raise actually succeeded.
    let raise_deadline = Instant::now() + Duration::from_secs(2);
    let raised = loop {
        let r = driver
            .act(
                &Action::Focus {
                    target: Target::Window { window_id: win_id },
                },
                &ctx_for(pid),
            )
            .expect("window focus resolves");
        if r.status == ActionStatus::Success || Instant::now() > raise_deadline {
            break r;
        }
        std::thread::sleep(Duration::from_millis(150));
    };

    let res = driver
        .act(
            &Action::Focus {
                target: Target::Semantic(SemanticTarget {
                    role: Some(role.clone()),
                    ..SemanticTarget::default()
                }),
            },
            &ctx_for(pid),
        )
        .expect("focus resolves");
    assert_eq!(res.status, ActionStatus::Success, "{res:?}");

    match raised.status {
        ActionStatus::Success => {
            // Focus delivery is async (the app pumps WM_SETFOCUS) —
            // re-observe until the flag lands, then it must be real.
            let deadline = Instant::now() + Duration::from_secs(3);
            let focused = loop {
                let obs2 = driver.observe(&scope).expect("re-observe");
                if let Some(e) = obs2
                    .elements
                    .iter()
                    .find(|e| e.role.as_deref() == Some(role.as_str()) && e.focused)
                {
                    break Some(e.id);
                }
                if Instant::now() > deadline {
                    break None;
                }
                std::thread::sleep(Duration::from_millis(150));
            };
            assert!(focused.is_some(), "edit never gained keyboard focus");
        }
        status => {
            assert_eq!(
                status,
                ActionStatus::Failed,
                "raise verdict must be an honest Success-or-Failed, got {status:?}"
            );
            eprintln!("session refuses foreground raises — focused-flag check skipped");
        }
    }
    let _ = np.0.kill();
}

#[test]
fn typetext_writes_through_value_pattern() {
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let edit = find_editable(&obs).expect("edit surface");

    let res = driver
        .act(
            &Action::TypeText {
                text: "typed semantically".into(),
                target: Some(Target::Element {
                    observation: obs.id,
                    element: edit,
                }),
            },
            &ctx_for(pid),
        )
        .expect("typetext resolves");
    assert_eq!(res.status, ActionStatus::Success, "{res:?}");

    let obs2 = driver.observe(&scope).expect("re-observe");
    let el = obs2
        .element(find_editable(&obs2).unwrap())
        .expect("edit lookup");
    assert!(
        el.value
            .as_deref()
            .is_some_and(|v| v.contains("typed semantically")),
        "value after TypeText: {:?}",
        el.value
    );
    let _ = np.0.kill();
}

#[test]
fn stale_reference_fails_closed() {
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let edit = find_editable(&obs).expect("edit surface");

    // The app is gone: the cached element id is a stale reference.
    np.0.kill().expect("kill notepad");
    let _ = np.0.wait();
    std::thread::sleep(Duration::from_millis(500));

    let err = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: edit,
                },
                button: MouseButton::Left,
            },
            &ctx_for(pid),
        )
        .expect_err("dead pid must not resolve");
    assert!(matches!(err, DriverError::StaleReference(_)), "{err:?}");
}

#[test]
fn physical_input_is_opt_in() {
    let driver = WindowsDriver::new();
    // No coordinates flag: every physical-input path declines as data,
    // never a simulated click.
    for action in [
        Action::Click {
            target: Target::Point { x: 10.0, y: 10.0 },
            button: MouseButton::Left,
        },
        Action::Key {
            chord: dexter_core::KeyChord {
                key: "a".into(),
                modifiers: vec!["ctrl".into()],
            },
        },
        Action::Scroll {
            delta: dexter_core::ScrollDelta {
                dx: 0.0,
                dy: -120.0,
            },
            target: None,
        },
    ] {
        let res = driver.act(&action, &ActContext::default());
        match res {
            Ok(r) => assert_eq!(r.status, ActionStatus::Unsupported, "{r:?}"),
            Err(DriverError::Unsupported(_)) => {}
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }
}
