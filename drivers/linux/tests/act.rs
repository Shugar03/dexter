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
    let mut child = ChildGuard(
        Command::new("python3")
            .arg(script)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("NO_AT_BRIDGE", "0")
            .env("GTK_MODULES", "gail:atk-bridge")
            .spawn()
            .expect("spawn GTK fixture"),
    );
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
fn physical_input_is_gated_and_never_claims_success() {
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
    let enabled = driver
        .act(
            &point,
            &ActContext {
                allow_coordinates: true,
                ..ActContext::default()
            },
        )
        .expect("XTest is deferred");
    assert_verdict(&enabled, ActionStatus::Unsupported, Mechanism::Coordinates);
    assert_eq!(
        enabled.detail.as_deref(),
        Some("XTest input not implemented on linux")
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

    let physical = ActContext {
        allow_coordinates: true,
        ..context.clone()
    };
    let right_click = fixture
        .driver
        .act(&right_click, &physical)
        .expect("no physical pointer backend is implemented");
    assert_verdict(
        &right_click,
        ActionStatus::Unsupported,
        Mechanism::Coordinates,
    );
    assert_eq!(
        right_click.detail.as_deref(),
        Some("XTest input not implemented on linux")
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
    let fallback = fixture
        .driver
        .act(&type_text, &physical)
        .expect("typing fallback remains unimplemented");
    assert_eq!(
        fallback.detail.as_deref(),
        Some("XTest input not implemented on linux")
    );
}

#[test]
fn direct_actions_keep_their_declared_mechanisms() {
    let driver = LinuxDriver::new();
    assert!(matches!(
        driver.act(&Action::Observe, &ActContext::default()),
        Err(DriverError::Unsupported(_))
    ));
    let navigate = driver
        .act(
            &Action::Navigate {
                url: "https://example.com".into(),
            },
            &ActContext::default(),
        )
        .expect("navigation returns an unsupported verdict");
    assert_verdict(&navigate, ActionStatus::Unsupported, Mechanism::Api);
    assert_eq!(
        navigate.detail.as_deref(),
        Some("navigate not implemented on linux")
    );
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

    let window_focus = driver
        .act(
            &Action::Focus {
                target: Target::Window { window_id: 1 },
            },
            &ActContext::default(),
        )
        .expect("window focus is unsupported");
    assert_eq!(
        window_focus.detail.as_deref(),
        Some("window focus not implemented on linux")
    );

    let targeted_scroll = driver
        .act(
            &Action::Scroll {
                delta: ScrollDelta { dx: 1.0, dy: 1.0 },
                target: Some(semantic("button", "Greet")),
            },
            &ActContext::default(),
        )
        .expect("targeted scrolling is unsupported");
    assert_eq!(
        targeted_scroll.detail.as_deref(),
        Some(
            "AT-SPI scroll not implemented — Component.ScrollTo only reveals, it doesn't scroll by a delta"
        )
    );

    let wait = driver
        .act(&Action::Wait { millis: 0 }, &ActContext::default())
        .expect("wait needs no AT-SPI bus");
    assert_verdict(&wait, ActionStatus::Success, Mechanism::NativeAutomation);
}
