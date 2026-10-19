#![cfg(target_os = "linux")]

use dexter_core::{
    Action, ActionStatus, AppSelector, Element, ElementId, Mechanism, MouseButton, Observation,
    ObservationId, ObservationScope, ScrollDelta, SemanticTarget, Target,
};
use dexter_driver::{ActContext, ComputerDriver, DriverError};
use dexter_linux::LinuxDriver;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

static FIXTURE_LOCK: Mutex<()> = Mutex::new(());

struct Fixture {
    _child: ChildGuard,
    driver: LinuxDriver,
    pid: i32,
    _guard: MutexGuard<'static, ()>,
}

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

impl Fixture {
    fn context(&self) -> ActContext {
        ActContext {
            app: Some(AppSelector::Pid(self.pid)),
            allow_coordinates: false,
        }
    }

    fn observe(&self) -> Observation {
        self.driver
            .observe(&ObservationScope::for_app(AppSelector::Pid(self.pid)))
            .expect("observe the GTK fixture")
    }
}

fn fixture() -> Option<Fixture> {
    fixture_with_scale(None)
}

fn fixture_with_scale(gdk_scale: Option<&str>) -> Option<Fixture> {
    let gtk3 = Command::new("python3")
        .args([
            "-c",
            "import gi; gi.require_version('Gtk', '3.0'); from gi.repository import Gtk",
        ])
        .output()
        .is_ok_and(|output| output.status.success());
    if !gtk3 {
        eprintln!("skipped: python3 GTK3 bindings unavailable");
        return None;
    }

    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_none() {
        eprintln!("skipped: GTK fixture requires a D-Bus session");
        return None;
    }

    let guard = FIXTURE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let driver = LinuxDriver::new();
    if let Err(error) = driver.windows() {
        eprintln!("skipped: no a11y bus ({error})");
        return None;
    }

    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/act_app.py");
    let mut command = Command::new("python3");
    command
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("NO_AT_BRIDGE", "0")
        .env("GTK_MODULES", "gail:atk-bridge");
    if let Some(scale) = gdk_scale {
        command.env("GDK_SCALE", scale);
    }
    let mut child = ChildGuard(command.spawn().expect("spawn GTK fixture"));
    let pid = i32::try_from(child.0.id()).expect("fixture pid fits i32");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if driver
            .windows()
            .is_ok_and(|windows| windows.iter().any(|window| window.pid == pid))
        {
            return Some(Fixture {
                _child: child,
                driver,
                pid,
                _guard: guard,
            });
        }
        if let Some(status) = child.0.try_wait().expect("poll GTK fixture") {
            let mut stderr = String::new();
            if let Some(mut stream) = child.0.stderr.take() {
                let _ = stream.read_to_string(&mut stderr);
            }
            panic!("GTK fixture exited {status}: {}", stderr);
        }
        assert!(
            Instant::now() < deadline,
            "GTK fixture pid {pid} did not appear on the a11y bus"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

fn semantic(role: &str, name: &str) -> Target {
    Target::Semantic(SemanticTarget {
        role: Some(role.into()),
        name: Some(name.into()),
        ..SemanticTarget::default()
    })
}

fn element<'a>(observation: &'a Observation, role: &str, name: &str) -> &'a Element {
    observation
        .elements
        .iter()
        .find(|element| {
            element.role.as_deref() == Some(role) && element.name.as_deref() == Some(name)
        })
        .unwrap_or_else(|| {
            panic!(
                "no {role} named {name:?} in observation:\n{}",
                observation.digest
            )
        })
}

fn window_id(fixture: &Fixture, title: &str) -> u32 {
    fixture
        .driver
        .windows()
        .expect("list fixture windows")
        .into_iter()
        .find(|window| window.title.as_deref() == Some(title))
        .unwrap_or_else(|| panic!("no window titled {title:?}"))
        .id
}

fn allow_coordinates(fixture: &Fixture) -> ActContext {
    ActContext {
        allow_coordinates: true,
        ..fixture.context()
    }
}

fn assert_verdict(result: &dexter_core::ActionResult, status: ActionStatus, mechanism: Mechanism) {
    assert_eq!(result.status, status, "{result:?}");
    assert_eq!(result.mechanism, mechanism, "{result:?}");
}

#[test]
fn semantic_press_updates_the_fixture_label() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::Click {
                target: semantic("button", "Greet"),
                button: MouseButton::Left,
            },
            &fixture.context(),
        )
        .expect("press Greet");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    let after = fixture.observe();
    assert!(after.elements.iter().any(|element| {
        element.name.as_deref() == Some("Hello") || element.value.as_deref() == Some("Hello")
    }));
}

#[test]
fn semantic_press_toggles_a_check_button() {
    let Some(fixture) = fixture() else { return };
    let target = semantic("check_box", "Agree");
    let before = fixture.observe();
    assert_eq!(
        element(&before, "check_box", "Agree").value.as_deref(),
        Some("off")
    );
    let result = fixture
        .driver
        .act(
            &Action::Click {
                target,
                button: MouseButton::Left,
            },
            &fixture.context(),
        )
        .expect("toggle Agree");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    assert_eq!(
        element(&fixture.observe(), "check_box", "Agree")
            .value
            .as_deref(),
        Some("on")
    );
}

#[test]
fn set_value_writes_editable_text() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::SetValue {
                target: semantic("text_area", "Name"),
                value: "Ada Lovelace".into(),
            },
            &fixture.context(),
        )
        .expect("set entry value");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    assert_eq!(
        element(&fixture.observe(), "text_area", "Name")
            .value
            .as_deref(),
        Some("Ada Lovelace")
    );
}

#[test]
fn type_text_uses_focused_editable_text() {
    let Some(fixture) = fixture() else { return };
    let context = fixture.context();
    fixture
        .driver
        .act(
            &Action::Focus {
                target: semantic("text_area", "Name"),
            },
            &context,
        )
        .expect("focus entry");
    let result = fixture
        .driver
        .act(
            &Action::TypeText {
                text: "Grace Hopper".into(),
                target: Some(Target::Focused),
            },
            &context,
        )
        .expect("set focused entry text");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    assert_eq!(
        element(&fixture.observe(), "text_area", "Name")
            .value
            .as_deref(),
        Some("Grace Hopper")
    );
}

#[test]
fn set_value_writes_numeric_atspi_value() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::SetValue {
                target: semantic("stepper", "Amount"),
                value: "42.5".into(),
            },
            &fixture.context(),
        )
        .expect("set spin button value");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    let observation = fixture.observe();
    let value = element(&observation, "stepper", "Amount")
        .value
        .as_deref()
        .expect("spin button value");
    assert!(
        (value.parse::<f64>().unwrap() - 42.5).abs() < 0.01,
        "{value}"
    );
}

#[test]
fn focus_marks_the_entry_focused() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::Focus {
                target: semantic("text_area", "Name"),
            },
            &fixture.context(),
        )
        .expect("focus entry");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    assert!(element(&fixture.observe(), "text_area", "Name").focused);
}

#[test]
fn disabled_button_fails_before_pressing() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::Click {
                target: semantic("button", "Locked"),
                button: MouseButton::Left,
            },
            &fixture.context(),
        )
        .expect("disabled target returns a failure verdict");
    assert_verdict(&result, ActionStatus::Failed, Mechanism::Accessibility);
    assert!(result.detail.as_deref().unwrap().contains("is disabled"));
}

#[test]
fn read_only_text_is_rejected() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::SetValue {
                target: semantic("text_area", "Read only"),
                value: "must not be written".into(),
            },
            &fixture.context(),
        )
        .expect("read-only target returns a failure verdict");
    assert_verdict(&result, ActionStatus::Failed, Mechanism::Accessibility);
    assert!(result.detail.as_deref().unwrap().contains("is read-only"));
}

#[test]
fn duplicate_semantic_buttons_are_ambiguous() {
    let Some(fixture) = fixture() else { return };
    let error = fixture
        .driver
        .act(
            &Action::Click {
                target: semantic("button", "OK"),
                button: MouseButton::Left,
            },
            &fixture.context(),
        )
        .unwrap_err();
    assert!(matches!(error, DriverError::Ambiguous(_)), "{error:?}");
}

#[test]
fn element_target_is_stale_after_removing_its_widget() {
    let Some(fixture) = fixture() else { return };
    let observation = fixture.observe();
    let name = element(&observation, "text_area", "Name");
    let target = Target::Element {
        observation: observation.id,
        element: name.id,
    };
    fixture
        .driver
        .act(
            &Action::Click {
                target: semantic("button", "Remove Name"),
                button: MouseButton::Left,
            },
            &fixture.context(),
        )
        .expect("remove entry from the tree");
    let error = fixture
        .driver
        .act(&Action::Focus { target }, &fixture.context())
        .unwrap_err();
    assert!(matches!(error, DriverError::StaleReference(_)), "{error:?}");
}

#[test]
fn unknown_observation_id_is_stale() {
    let Some(fixture) = fixture() else { return };
    let error = fixture
        .driver
        .act(
            &Action::Focus {
                target: Target::Element {
                    observation: ObservationId(u64::MAX),
                    element: ElementId(1),
                },
            },
            &fixture.context(),
        )
        .unwrap_err();
    assert!(matches!(error, DriverError::StaleReference(_)), "{error:?}");
}

#[test]
fn point_click_requires_coordinates_and_hits_the_button_when_enabled() {
    let Some(fixture) = fixture() else { return };
    let observation = fixture.observe();
    let bounds = element(&observation, "button", "Greet")
        .bounds
        .expect("button bounds");
    let point = Target::Point {
        x: bounds.x + bounds.w / 2.0,
        y: bounds.y + bounds.h / 2.0,
    };
    let click = Action::Click {
        target: point,
        button: MouseButton::Left,
    };
    let disabled = fixture
        .driver
        .act(&click, &fixture.context())
        .expect("disabled coordinate click");
    assert_verdict(&disabled, ActionStatus::Unsupported, Mechanism::Coordinates);
    assert_eq!(
        disabled.detail.as_deref(),
        Some("coordinate input disabled — pass the explicit coords flag")
    );
    assert!(fixture.observe().elements.iter().any(|element| {
        element.name.as_deref() == Some("Ready") || element.value.as_deref() == Some("Ready")
    }));

    let enabled = fixture
        .driver
        .act(&click, &allow_coordinates(&fixture))
        .expect("XTest click");
    assert_verdict(&enabled, ActionStatus::Success, Mechanism::Coordinates);
    let expected = format!(
        "clicked left at ({},{})",
        bounds.x + bounds.w / 2.0,
        bounds.y + bounds.h / 2.0
    );
    assert_eq!(enabled.detail.as_deref(), Some(expected.as_str()));
    assert!(fixture.observe().elements.iter().any(|element| {
        element.name.as_deref() == Some("Hello") || element.value.as_deref() == Some("Hello")
    }));
}

#[test]
fn non_left_element_click_uses_its_bounds_with_coordinates_enabled() {
    let Some(fixture) = fixture() else { return };
    let result = fixture
        .driver
        .act(
            &Action::Click {
                target: semantic("button", "Greet"),
                button: MouseButton::Right,
            },
            &allow_coordinates(&fixture),
        )
        .expect("right-click element bounds");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Coordinates);
    assert!(
        result
            .detail
            .as_deref()
            .unwrap_or_default()
            .starts_with("clicked right at ("),
        "{result:?}"
    );
}

#[test]
fn key_types_into_the_focused_entry_via_xtest() {
    let Some(fixture) = fixture() else { return };
    let context = allow_coordinates(&fixture);
    let main_window = window_id(&fixture, "dexter-act-fixture");
    fixture
        .driver
        .act(
            &Action::Focus {
                target: Target::Window {
                    window_id: main_window,
                },
            },
            &context,
        )
        .expect("activate the main fixture window");
    fixture
        .driver
        .act(
            &Action::SetValue {
                target: semantic("text_area", "Name"),
                value: String::new(),
            },
            &context,
        )
        .expect("clear entry");
    fixture
        .driver
        .act(
            &Action::Focus {
                target: semantic("text_area", "Name"),
            },
            &context,
        )
        .expect("focus entry");
    let result = fixture
        .driver
        .act(
            &Action::Key {
                chord: dexter_core::KeyChord {
                    key: "z".into(),
                    modifiers: Vec::new(),
                },
            },
            &context,
        )
        .expect("send key");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Coordinates);
    assert_eq!(
        element(&fixture.observe(), "text_area", "Name")
            .value
            .as_deref(),
        Some("z")
    );
}

#[test]
fn key_requires_the_scoped_application_to_be_foreground() {
    let Some(fixture) = fixture() else { return };
    let windows = Command::new("xdotool")
        .args(["search", "--pid", &fixture.pid.to_string()])
        .output();
    let Ok(windows) = windows else {
        eprintln!("skipped: xdotool unavailable for foreground test");
        return;
    };
    assert!(windows.status.success(), "xdotool search failed");
    let ids = String::from_utf8(windows.stdout).expect("xdotool output is UTF-8");
    assert!(!ids.trim().is_empty(), "fixture has no X windows");
    for id in ids.lines() {
        let output = Command::new("xdotool")
            .args(["windowminimize", id])
            .output()
            .expect("minimize fixture window");
        assert!(output.status.success(), "xdotool windowminimize failed");
    }
    thread::sleep(Duration::from_millis(100));

    let result = fixture
        .driver
        .act(
            &Action::Key {
                chord: dexter_core::KeyChord {
                    key: "z".into(),
                    modifiers: Vec::new(),
                },
            },
            &allow_coordinates(&fixture),
        )
        .expect("background key input is rejected");
    assert_verdict(
        &result,
        ActionStatus::ForegroundRequired,
        Mechanism::Coordinates,
    );
}

#[test]
fn unmappable_type_text_is_rejected_before_typing() {
    let Some(fixture) = fixture() else { return };
    let context = allow_coordinates(&fixture);
    let entry_before = element(&fixture.observe(), "text_area", "Name")
        .value
        .clone();
    fixture
        .driver
        .act(
            &Action::Focus {
                target: semantic("button", "Greet"),
            },
            &context,
        )
        .expect("focus non-editable target");
    let result = fixture
        .driver
        .act(
            &Action::TypeText {
                text: "prefix\u{10ffff}".into(),
                target: Some(semantic("button", "Greet")),
            },
            &context,
        )
        .expect("unmappable text is unsupported");
    assert_verdict(&result, ActionStatus::Unsupported, Mechanism::Coordinates);
    assert_eq!(
        result.detail.as_deref(),
        Some("XTest cannot map character '\\u{10ffff}'")
    );
    let after = fixture.observe();
    assert_eq!(
        element(&after, "text_area", "Name").value,
        entry_before,
        "unmappable text must not reach the entry"
    );
    assert!(after.elements.iter().any(|element| {
        element.name.as_deref() == Some("Ready") || element.value.as_deref() == Some("Ready")
    }));
}

#[test]
fn targeted_scroll_updates_value_and_reports_end_of_range() {
    let Some(fixture) = fixture() else { return };
    let target = semantic("scroll_area", "Scroll area");
    let initial = fixture.observe();
    let pane = element(&initial, "scroll_area", "Scroll area");
    let vertical = initial
        .elements
        .iter()
        .find(|element| {
            element.parent == Some(pane.id)
                && element.role.as_deref() == Some("scroll_bar")
                && element.value.is_some()
        })
        .expect("vertical child scrollbar");
    let before = vertical
        .value
        .as_deref()
        .expect("scrollbar value")
        .parse::<f64>()
        .expect("numeric scrollbar value");
    let result = fixture
        .driver
        .act(
            &Action::Scroll {
                delta: ScrollDelta { dx: 0.0, dy: 80.0 },
                target: Some(target.clone()),
            },
            &fixture.context(),
        )
        .expect("scroll down");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Accessibility);
    let after = fixture.observe();
    let pane = element(&after, "scroll_area", "Scroll area");
    let value = after
        .elements
        .iter()
        .find(|element| {
            element.parent == Some(pane.id)
                && element.role.as_deref() == Some("scroll_bar")
                && element.value.is_some()
        })
        .and_then(|element| element.value.as_deref())
        .expect("updated scrollbar value")
        .parse::<f64>()
        .expect("numeric updated scrollbar value");
    assert!(
        value > before,
        "scrollbar did not advance: {before} -> {value}"
    );

    let horizontal = fixture
        .driver
        .act(
            &Action::Scroll {
                delta: ScrollDelta { dx: 80.0, dy: 0.0 },
                target: Some(target.clone()),
            },
            &fixture.context(),
        )
        .expect("horizontal request is unsupported");
    assert_verdict(
        &horizontal,
        ActionStatus::Unsupported,
        Mechanism::Accessibility,
    );
    assert!(horizontal
        .detail
        .as_deref()
        .unwrap_or_default()
        .contains("horizontal"));

    let to_end = Action::Scroll {
        delta: ScrollDelta {
            dx: 0.0,
            dy: 100_000.0,
        },
        target: Some(target.clone()),
    };
    assert_verdict(
        &fixture
            .driver
            .act(&to_end, &fixture.context())
            .expect("scroll to end"),
        ActionStatus::Success,
        Mechanism::Accessibility,
    );
    let at_end = fixture
        .driver
        .act(&to_end, &fixture.context())
        .expect("scrolling at end fails");
    assert_verdict(&at_end, ActionStatus::Failed, Mechanism::Accessibility);
    assert_eq!(at_end.detail.as_deref(), Some("already at scroll limit"));
}

#[test]
fn window_focus_activates_second_window_and_unknown_window_is_not_found() {
    let Some(fixture) = fixture() else { return };
    let context = fixture.context();
    let first = Command::new("xdotool")
        .args(["search", "--onlyvisible", "--pid", &fixture.pid.to_string()])
        .output()
        .expect("find fixture X11 windows");
    assert!(first.status.success(), "xdotool search failed");
    let first = String::from_utf8(first.stdout).expect("xdotool output is UTF-8");
    let first = first
        .lines()
        .find(|id| {
            Command::new("xdotool")
                .args(["getwindowname", id])
                .output()
                .map(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout).trim() == "dexter-act-fixture"
                })
                .unwrap_or(false)
        })
        .expect("fixture main window");
    let activated = Command::new("xdotool")
        .args(["windowactivate", "--sync", first])
        .output()
        .expect("activate fixture main window");
    assert!(
        activated.status.success(),
        "xdotool could not activate main window"
    );
    let active = Command::new("xdotool")
        .arg("getactivewindow")
        .output()
        .expect("read active X11 window");
    assert!(active.status.success(), "xdotool getactivewindow failed");
    assert_eq!(
        String::from_utf8_lossy(&active.stdout).trim(),
        first,
        "main fixture window should be active before EWMH activation"
    );
    let second = window_id(&fixture, "dexter-act-fixture-second");
    let result = fixture
        .driver
        .act(
            &Action::Focus {
                target: Target::Window { window_id: second },
            },
            &context,
        )
        .expect("activate second fixture window");
    assert_verdict(&result, ActionStatus::Success, Mechanism::NativeAutomation);
    let expected = format!("activated window {second} via _NET_ACTIVE_WINDOW");
    assert_eq!(result.detail.as_deref(), Some(expected.as_str()));

    let missing = fixture.driver.act(
        &Action::Focus {
            target: Target::Window { window_id: 0 },
        },
        &context,
    );
    assert!(
        matches!(missing, Err(DriverError::NotFound(_))),
        "{missing:?}"
    );
}

#[test]
fn physical_input_is_gated_when_coordinates_are_disabled() {
    let driver = LinuxDriver::new();
    let point = Action::Click {
        target: Target::Point { x: 10.0, y: 20.0 },
        button: MouseButton::Left,
    };
    let disabled = driver
        .act(&point, &ActContext::default())
        .expect("coordinate click returns an unsupported verdict");
    assert_verdict(&disabled, ActionStatus::Unsupported, Mechanism::Coordinates);
    assert_eq!(
        disabled.detail.as_deref(),
        Some("coordinate input disabled — pass the explicit coords flag")
    );
    let key = Action::Key {
        chord: dexter_core::KeyChord {
            key: "a".into(),
            modifiers: Vec::new(),
        },
    };
    let key_result = driver
        .act(&key, &ActContext::default())
        .expect("key chords are physical input");
    assert_verdict(
        &key_result,
        ActionStatus::Unsupported,
        Mechanism::Coordinates,
    );
    assert_eq!(
        key_result.detail.as_deref(),
        Some("coordinate input disabled — pass the explicit coords flag")
    );

    let scroll = Action::Scroll {
        delta: ScrollDelta { dx: 0.0, dy: 80.0 },
        target: None,
    };
    let scroll_result = driver
        .act(&scroll, &ActContext::default())
        .expect("untargeted scroll is physical input");
    assert_verdict(
        &scroll_result,
        ActionStatus::Unsupported,
        Mechanism::Coordinates,
    );
    assert_eq!(
        scroll_result.detail.as_deref(),
        Some("coordinate input disabled — pass the explicit coords flag")
    );
}

#[test]
fn semantic_fallbacks_and_non_left_clicks_are_honestly_unsupported() {
    let Some(fixture) = fixture() else { return };
    let target = semantic("button", "Greet");
    let context = fixture.context();
    let right_click = Action::Click {
        target: target.clone(),
        button: MouseButton::Right,
    };
    let disabled = fixture
        .driver
        .act(&right_click, &context)
        .expect("right click has no AT-SPI action");
    assert_verdict(
        &disabled,
        ActionStatus::Unsupported,
        Mechanism::Accessibility,
    );
    assert_eq!(
        disabled.detail.as_deref(),
        Some("no AT-SPI action reaches a context menu — enable coords for a pointer click")
    );

    let type_text = Action::TypeText {
        text: "not physical".into(),
        target: Some(target),
    };
    let fallback = fixture
        .driver
        .act(&type_text, &context)
        .expect("typing into a button has no EditableText interface");
    assert_verdict(&fallback, ActionStatus::Unsupported, Mechanism::Coordinates);
    assert_eq!(
        fallback.detail.as_deref(),
        Some("coordinate input disabled — pass the explicit coords flag")
    );
}

#[test]
fn direct_actions_keep_their_declared_mechanisms() {
    let driver = LinuxDriver::new();
    assert!(matches!(
        driver.act(&Action::Observe, &ActContext::default()),
        Err(DriverError::Unsupported(_))
    ));
    let semantic_focus = driver
        .act(
            &Action::Focus {
                target: semantic("button", "Greet"),
            },
            &ActContext::default(),
        )
        .unwrap_err();
    assert!(matches!(semantic_focus, DriverError::NotFound(_)));

    let window_click = driver
        .act(
            &Action::Click {
                target: Target::Window { window_id: 1 },
                button: MouseButton::Left,
            },
            &ActContext::default(),
        )
        .expect("window click is unsupported");
    assert_verdict(
        &window_click,
        ActionStatus::Unsupported,
        Mechanism::Accessibility,
    );

    let point_focus = driver
        .act(
            &Action::Focus {
                target: Target::Point { x: 1.0, y: 1.0 },
            },
            &ActContext::default(),
        )
        .expect("points cannot be focused");
    assert_eq!(point_focus.detail.as_deref(), Some("cannot focus a point"));

    let wait = driver
        .act(&Action::Wait { millis: 0 }, &ActContext::default())
        .expect("wait needs no AT-SPI bus");
    assert_verdict(&wait, ActionStatus::Success, Mechanism::NativeAutomation);
}

#[test]
fn navigate_refuses_non_urls_without_coordinate_or_accessibility_context() {
    let result = LinuxDriver::new()
        .act(
            &Action::Navigate {
                url: "file:///etc/passwd".into(),
            },
            &ActContext::default(),
        )
        .expect("refused navigation returns an unsupported verdict");
    assert_eq!(result.status, ActionStatus::Unsupported);
    assert_eq!(result.mechanism, Mechanism::NativeAutomation);
    assert_eq!(
        result.detail.as_deref(),
        Some("refusing to open non-URL input — only http/https/mailto reach xdg-open")
    );
}

#[test]
fn gdk_scale_two_element_click_uses_device_coordinates() {
    let fixture = fixture_with_scale(Some("2"))
        .expect("GDK_SCALE=2 live GTK fixture requires X11 and a D-Bus session");
    let result = fixture
        .driver
        .act(
            &Action::Click {
                target: semantic("button", "Greet"),
                button: MouseButton::Right,
            },
            &allow_coordinates(&fixture),
        )
        .expect("click Greet with XTest");
    assert_verdict(&result, ActionStatus::Success, Mechanism::Coordinates);

    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let observation = fixture.observe();
        if observation
            .elements
            .iter()
            .any(|item| item.name.as_deref() == Some("Pressed button 3"))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "GDK_SCALE=2 XTest click did not reach Greet:\n{}",
            observation.digest
        );
        thread::sleep(Duration::from_millis(50));
    }
}
