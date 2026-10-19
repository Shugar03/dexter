//! Real-backend tests for the UIA act slice. Only compiled on Windows —
//! CI (macOS/Ubuntu) sees an empty target; these run where UIA and a
//! desktop session exist. Each test drives its own `notepad.exe`
//! instance scoped by pid, so they can run in parallel.

#![cfg(windows)]

use dexter_core::{
    Action, ActionStatus, AppSelector, Mechanism, MouseButton, ObservationScope, SemanticTarget,
    Target,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_windows::WindowsDriver;
use std::process::{Child, Command};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// These tests all drive the same desktop: an open context menu
/// dismisses when any window takes activation, and a raise in one
/// test would race another's open menu. The lock keeps the shared
/// desktop single-tenant — poisoning is ignored so a failed test
/// can't cascade.
static DESKTOP: Mutex<()> = Mutex::new(());

fn desktop() -> MutexGuard<'static, ()> {
    DESKTOP.lock().unwrap_or_else(|e| e.into_inner())
}

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
    let _desktop = desktop();
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
    let _desktop = desktop();
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
    let _desktop = desktop();
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
    let _desktop = desktop();
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
    let _desktop = desktop();
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
    let _desktop = desktop();
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

/// Top-level HWNDs owned by `pid` — a context menu is a new top-level
/// window of the owning process (`#32768` on classic Win32, a popup
/// class on UWP), so this set is the honest proof one materialized.
fn window_hwnds_of(pid: i32) -> Vec<isize> {
    dexter_windows::win::hwnds()
        .into_iter()
        .filter(|(_, p)| *p == pid)
        .map(|(h, _)| h.0 as isize)
        .collect()
}

/// The first top-level window of `pid` that `before` did not contain.
fn wait_new_window(pid: i32, before: &[isize], secs: u64) -> Option<isize> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(h) = window_hwnds_of(pid)
            .into_iter()
            .find(|h| !before.contains(h))
        {
            return Some(h);
        }
        if Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn hwnd_alive(pid: i32, hwnd: isize) -> bool {
    dexter_windows::win::hwnds()
        .iter()
        .any(|(h, p)| *p == pid && (h.0 as isize) == hwnd)
}

/// Pid owning the foreground window — the steal detector for acts
/// that must run in the background.
fn foreground_pid() -> Option<i32> {
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
    let h = unsafe { GetForegroundWindow() };
    if h.0.is_null() {
        return None;
    }
    let mut p = 0u32;
    unsafe { GetWindowThreadProcessId(h, Some(&mut p)) };
    (p != 0).then_some(p as i32)
}

#[test]
fn right_click_opens_context_menu_and_item_press_executes() {
    let _desktop = desktop();
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let edit = find_editable(&obs).expect("edit surface");

    // Fill the edit without touching focus: a posted WM_SETTEXT is
    // marshaled across threads (system range), so "Select All" is a
    // live item while the window stays in the background. (SetValue
    // would activate notepad — the OLEACC edit proxy focuses first.)
    let frame = obs
        .windows
        .iter()
        .find(|w| w.pid == pid && w.on_screen)
        .map(|w| w.id)
        .expect("notepad frame window id");
    let frame_hwnd = dexter_windows::win::hwnd_of(frame);
    let edit_hwnd = unsafe {
        windows::Win32::UI::WindowsAndMessaging::FindWindowExW(
            Some(frame_hwnd),
            None,
            windows::core::w!("Edit"),
            None,
        )
    }
    .expect("edit child hwnd");
    let text = windows::core::HSTRING::from("context menu");
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SendMessageW(
            edit_hwnd,
            windows::Win32::UI::WindowsAndMessaging::WM_SETTEXT,
            Some(windows::Win32::Foundation::WPARAM(0)),
            Some(windows::Win32::Foundation::LPARAM(text.as_ptr() as isize)),
        )
    };
    std::thread::sleep(Duration::from_millis(300));

    // Background-input proof anchors: the semantic menu flow must move
    // neither the OS cursor nor the foreground window.
    let mut cursor = POINT::default();
    unsafe { GetCursorPos(&mut cursor) }.expect("cursor pos");
    let fg_before = foreground_pid();
    let wins_before = window_hwnds_of(pid);

    let res = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: edit,
                },
                button: MouseButton::Right,
            },
            &ctx_for(pid),
        )
        .expect("right click resolves");
    assert_eq!(res.status, ActionStatus::Success, "{res:?}");
    // The open is a background act: whoever owned the foreground still
    // does — a posted WM_CONTEXTMENU never activates its target.
    assert_eq!(
        foreground_pid(),
        fg_before,
        "context-menu open changed the foreground owner"
    );

    // A real menu materialized — not a pattern call that returned
    // politely: a new top-level window must exist in notepad's pid.
    let menu_hwnd =
        wait_new_window(pid, &wins_before, 5).expect("no context menu window appeared for the pid");

    // Its items are ordinary elements of the app's pid — the same
    // observe machinery resolves them for a semantic pick.
    let obs2 = driver.observe(&scope).expect("observe with menu open");
    assert!(
        obs2.elements.iter().any(|e| {
            e.role.as_deref() == Some("menu_item")
                && e.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains("select all"))
        }),
        "menu item 'Select all' did not resolve in the open menu"
    );

    let res = driver
        .act(
            &Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("menu_item".into()),
                    name_contains: Some("select all".into()),
                    ..SemanticTarget::default()
                }),
                button: MouseButton::Left,
            },
            &ctx_for(pid),
        )
        .expect("menu item press resolves");
    assert_eq!(res.status, ActionStatus::Success, "{res:?}");

    // A real item press dismisses its menu — the window must be gone.
    let deadline = Instant::now() + Duration::from_secs(3);
    let menu_gone = loop {
        if !hwnd_alive(pid, menu_hwnd) {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(menu_gone, "context menu stayed open after item press");

    let mut cursor_after = POINT::default();
    unsafe { GetCursorPos(&mut cursor_after) }.expect("cursor pos");
    assert_eq!(
        (cursor_after.x, cursor_after.y),
        (cursor.x, cursor.y),
        "semantic context-menu flow moved the OS cursor"
    );
    // Executing a menu command can surface the app — Windows' own
    // command dispatch raises the owner, whatever drove the pick — so
    // the foreground is not asserted past the open. The open itself,
    // above, is the act this slice adds and it is proven background.
    let _ = np.0.kill();
}

#[test]
fn right_click_without_menu_fails_closed() {
    let _desktop = desktop();
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    // A status-bar text owns no window to post WM_CONTEXTMENU at and
    // offers no menu pattern — without the coords flag the verdict is
    // an honest Unsupported, not a pointer click.
    let text = obs
        .elements
        .iter()
        .find(|e| e.role.as_deref() == Some("static_text") && e.name.as_deref().is_some())
        .expect("status bar text element")
        .id;

    let res = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: text,
                },
                button: MouseButton::Right,
            },
            &ctx_for(pid),
        )
        .expect("right click resolves");
    assert_eq!(res.status, ActionStatus::Unsupported, "{res:?}");
    let _ = np.0.kill();
}

#[test]
fn middle_click_has_no_semantic_path() {
    let _desktop = desktop();
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
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: edit,
                },
                button: MouseButton::Middle,
            },
            &ctx_for(pid),
        )
        .expect("middle click resolves");
    assert_eq!(res.status, ActionStatus::Unsupported, "{res:?}");
    // There is no semantic middle verb — the only path is disabled
    // pointer input, so the verdict is the same one a Point target
    // reports, not a context-menu report.
    assert_eq!(res.mechanism, Mechanism::Coordinates, "{res:?}");
    assert!(
        res.detail
            .as_deref()
            .is_some_and(|d| d.contains("coordinate input disabled")),
        "unexpected detail: {res:?}"
    );
    let _ = np.0.kill();
}

/// Injected button events seen by the low-level mouse hook —
/// `(wParam, x, y)` in screen coordinates.
static MOUSE_EVENTS: Mutex<Vec<(u32, i32, i32)>> = Mutex::new(Vec::new());

/// `WH_MOUSE_LL` recorder. The callback runs in the installing
/// thread's context once that thread pumps its message queue.
/// `LLMHF_INJECTED` flags only the events the OS considers injected —
/// exactly what `SendInput` produces.
unsafe extern "system" fn record_mouse_hook(
    code: i32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::{CallNextHookEx, LLMHF_INJECTED, MSLLHOOKSTRUCT};
    if code >= 0 {
        let s = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        if s.flags & LLMHF_INJECTED != 0 {
            MOUSE_EVENTS
                .lock()
                .unwrap()
                .push((wparam.0 as u32, s.pt.x, s.pt.y));
        }
    }
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Pump this thread's queue — low-level hook callbacks are delivered
/// through it, so the injected click only becomes visible here.
fn pump_events() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

#[test]
fn middle_click_sendinput_delivers_middle_button_events() {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowsHookExW, UnhookWindowsHookEx, WH_MOUSE_LL, WM_MBUTTONDOWN, WM_MBUTTONUP,
    };

    let _desktop = desktop();
    let driver = WindowsDriver::new();
    let Some(mut np) = Notepad::launch() else {
        return;
    };
    let pid = np.0.id() as i32;
    assert!(wait_for_window(&driver, pid), "notepad window appeared");

    let scope = ObservationScope::for_app(AppSelector::Pid(pid));
    let obs = driver.observe(&scope).expect("observe notepad");
    let edit = obs
        .element(find_editable(&obs).expect("edit surface"))
        .expect("edit lookup")
        .clone();
    let center = edit.bounds.expect("edit has bounds").center();

    MOUSE_EVENTS.lock().unwrap().clear();
    let hook = unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(record_mouse_hook), None, 0) }
        .expect("WH_MOUSE_LL hook installs");

    let res = driver
        .act(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: edit.id,
                },
                button: MouseButton::Middle,
            },
            &ActContext {
                app: Some(AppSelector::Pid(pid)),
                allow_coordinates: true,
            },
        )
        .expect("middle click resolves");

    // The hook is notified through this thread's queue — pump until
    // the injected pair arrives or the window for it closes.
    let deadline = Instant::now() + Duration::from_secs(2);
    let events = loop {
        pump_events();
        let evs = MOUSE_EVENTS.lock().unwrap().clone();
        if evs
            .iter()
            .filter(|(w, _, _)| *w == WM_MBUTTONDOWN || *w == WM_MBUTTONUP)
            .count()
            >= 2
            || Instant::now() > deadline
        {
            break evs;
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    let _ = unsafe { UnhookWindowsHookEx(hook) };

    assert_eq!(res.status, ActionStatus::Success, "{res:?}");
    assert_eq!(res.mechanism, Mechanism::Coordinates, "{res:?}");
    let seq: Vec<u32> = events.iter().map(|e| e.0).collect();
    let down = seq
        .iter()
        .position(|w| *w == WM_MBUTTONDOWN)
        .unwrap_or_else(|| panic!("no injected WM_MBUTTONDOWN recorded — events {events:?}"));
    assert!(
        seq.iter().skip(down + 1).any(|w| *w == WM_MBUTTONUP),
        "no WM_MBUTTONUP after the down — events {events:?}"
    );
    // And the pair must land where SetCursorPos put the pointer: the
    // element's bounds center.
    let (x, y) = (center.x as i32, center.y as i32);
    assert!(
        events.iter().any(|(w, ex, ey)| *w == WM_MBUTTONDOWN
            && (*ex - x).abs() <= 1
            && (*ey - y).abs() <= 1),
        "middle-down not at element center ({x},{y}) — events {events:?}"
    );
    let _ = np.0.kill();
}
