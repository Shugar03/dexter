//! Desktop actions v2 in the sim: invoke, lifecycle, windows,
//! clipboard, click counts and drag — deterministic, no OS.

use dexter_core::{
    Action, ActionStatus, AppSelector, Element, ElementId, ElementSource, MouseButton,
    ObservationScope, SemanticTarget, Sensitivity, Target, WindowOperation,
};
use dexter_driver::{ActContext, ComputerDriver};
use dexter_sim::{Effect, SimDriver};

fn el(id: u64, role: &str, name: &str, actions: &[&str]) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: actions.iter().map(|s| s.to_string()).collect(),
        source: ElementSource::Accessibility,
        ..Default::default()
    }
}

fn semantic(name: &str) -> Target {
    Target::Semantic(SemanticTarget {
        name: Some(name.into()),
        ..Default::default()
    })
}

fn ctx() -> ActContext {
    ActContext::default()
}

#[test]
fn invoke_performs_only_advertised_actions() {
    let d = SimDriver::new(vec![el(1, "row", "file.txt", &["open", "press"])]);
    let r = d
        .act(
            &Action::Invoke {
                target: semantic("file.txt"),
                action: "open".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(r.status, ActionStatus::Success);
    assert_eq!(d.invoked(), vec![(ElementId(1), "open".to_string())]);

    // Unadvertised names are refused — never passed to the platform.
    let r = d
        .act(
            &Action::Invoke {
                target: semantic("file.txt"),
                action: "delete_everything".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(r.status, ActionStatus::Unsupported);
    assert_eq!(d.invoked().len(), 1);
}

#[test]
fn invoke_applies_world_effects_like_a_press() {
    let d = SimDriver::new(vec![el(1, "button", "Go", &["press"])]);
    d.on_press(
        SemanticTarget {
            name: Some("Go".into()),
            ..Default::default()
        },
        Effect::Spawn(el(9, "static_text", "done", &[])),
    );
    let r = d
        .act(
            &Action::Invoke {
                target: semantic("Go"),
                action: "press".into(),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(r.status, ActionStatus::Success);
    assert!(d
        .elements()
        .iter()
        .any(|e| e.name.as_deref() == Some("done")));
}

#[test]
fn launch_and_quit_change_the_window_set() {
    let d = SimDriver::new(vec![]);
    let scope = dexter_core::ObservationScope::default();
    let before = d.observe(&scope).unwrap();
    assert_eq!(before.windows.len(), 1);

    d.act(
        &Action::LaunchApp {
            app: AppSelector::Name("Calc".into()),
            activate: true,
        },
        &ctx(),
    )
    .unwrap();
    let obs = d.observe(&scope).unwrap();
    assert!(obs.windows.iter().any(|w| w.app == "Calc"));

    d.act(
        &Action::QuitApp {
            app: AppSelector::Name("Calc".into()),
        },
        &ctx(),
    )
    .unwrap();
    let obs = d.observe(&scope).unwrap();
    assert!(!obs.windows.iter().any(|w| w.app == "Calc"));

    // Quitting something not running fails honestly.
    assert!(d
        .act(
            &Action::QuitApp {
                app: AppSelector::Name("NeverLaunched".into()),
            },
            &ctx(),
        )
        .is_err());

    // A pid selector is invalid for launch — the process must exist.
    assert!(d
        .act(
            &Action::LaunchApp {
                app: AppSelector::Pid(42),
                activate: true,
            },
            &ctx(),
        )
        .is_err());
}

#[test]
fn window_ops_mutate_the_window_set() {
    let d = SimDriver::new(vec![]);
    let scope = dexter_core::ObservationScope::default();

    // New spawns a window for the frontmost app.
    d.act(
        &Action::Window {
            window_id: None,
            operation: WindowOperation::New,
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(d.observe(&scope).unwrap().windows.len(), 2);

    // Minimize hides; restore shows.
    d.act(
        &Action::Window {
            window_id: Some(1),
            operation: WindowOperation::Minimize,
        },
        &ctx(),
    )
    .unwrap();
    let obs = d.observe(&scope).unwrap();
    assert!(!obs.windows.iter().find(|w| w.id == 1).unwrap().on_screen);

    d.act(
        &Action::Window {
            window_id: Some(1),
            operation: WindowOperation::Restore,
        },
        &ctx(),
    )
    .unwrap();
    let obs = d.observe(&scope).unwrap();
    assert!(obs.windows.iter().find(|w| w.id == 1).unwrap().on_screen);

    // Move/resize write bounds.
    d.act(
        &Action::Window {
            window_id: Some(1),
            operation: WindowOperation::Move { x: 50.0, y: 60.0 },
        },
        &ctx(),
    )
    .unwrap();
    let obs = d.observe(&scope).unwrap();
    let b = &obs.windows.iter().find(|w| w.id == 1).unwrap().bounds;
    assert_eq!((b.x, b.y), (50.0, 60.0));

    // Close removes; a stale id then fails.
    d.act(
        &Action::Window {
            window_id: Some(2),
            operation: WindowOperation::Close,
        },
        &ctx(),
    )
    .unwrap();
    assert!(d
        .act(
            &Action::Window {
                window_id: Some(2),
                operation: WindowOperation::Focus,
            },
            &ctx(),
        )
        .is_err());
}

#[test]
fn clipboard_roundtrips_text() {
    let d = SimDriver::new(vec![]);
    d.act(
        &Action::WriteClipboardText {
            text: "hello clipboard".into(),
        },
        &ctx(),
    )
    .unwrap();
    let r = d.act(&Action::ReadClipboardText, &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Success);
    assert_eq!(r.detail.as_deref(), Some("hello clipboard"));
    assert_eq!(d.clipboard(), "hello clipboard");
}

#[test]
fn drag_requires_both_endpoints_to_resolve() {
    let d = SimDriver::new(vec![
        el(1, "row", "file.txt", &[]),
        el(2, "row", "folder", &[]),
    ]);
    let r = d
        .act(
            &Action::Drag {
                from: semantic("file.txt"),
                to: semantic("folder"),
                duration_ms: 100,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(r.status, ActionStatus::Success);
    assert_eq!(d.dragged(), vec![(ElementId(1), ElementId(2))]);

    // A stale `to` endpoint fails the whole drag — nothing moves.
    let obs = d
        .observe(&dexter_core::ObservationScope::default())
        .unwrap();
    let r = d.act(
        &Action::Drag {
            from: semantic("file.txt"),
            to: Target::Element {
                observation: obs.id,
                element: ElementId(999),
            },
            duration_ms: 0,
        },
        &ctx(),
    );
    assert!(r.is_err());
    assert_eq!(d.dragged().len(), 1);
}

#[test]
fn click_records_count_and_double_click_opens() {
    let d = SimDriver::new(vec![el(1, "row", "doc.pdf", &["open"])]);
    d.act(
        &Action::Click {
            target: semantic("doc.pdf"),
            button: MouseButton::Left,
            count: 2,
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(d.click_counts(), vec![2]);
}

#[test]
fn type_text_appends_set_value_replaces() {
    let d = SimDriver::new(vec![el(1, "text_field", "Notes", &["set_value"])]);
    d.act(
        &Action::SetValue {
            target: semantic("Notes"),
            value: "first".into(),
        },
        &ctx(),
    )
    .unwrap();
    d.act(
        &Action::TypeText {
            text: " second".into(),
            target: Some(semantic("Notes")),
        },
        &ctx(),
    )
    .unwrap();
    assert_eq!(
        d.elements()[0].value.as_deref(),
        Some("first second"),
        "TypeText appends; SetValue replaces"
    );
}

#[test]
fn wake_launches_a_stopped_app() {
    // Wake means "the app is on stage": a scoped task against a closed
    // app surfaces it like the real driver's launch-on-wake.
    let d = SimDriver::new(vec![]);
    let sel = AppSelector::Name("Editor".into());
    d.wake(&sel).unwrap();
    let obs = d.observe(&ObservationScope::default()).unwrap();
    assert!(obs.windows.iter().any(|w| w.app == "Editor"));
    // Waking an already-running app is a no-op — one window, not two.
    d.wake(&sel).unwrap();
    let obs = d.observe(&ObservationScope::default()).unwrap();
    assert_eq!(obs.windows.iter().filter(|w| w.app == "Editor").count(), 1);
}

#[test]
fn window_op_on_empty_window_list_is_notfound_not_panic() {
    // Closing the last window leaves `windows` empty — a follow-up op
    // is an honest NotFound, not an index-0 panic.
    let d = SimDriver::new(vec![]);
    d.act(
        &Action::Window {
            window_id: None,
            operation: WindowOperation::Close,
        },
        &ctx(),
    )
    .unwrap();
    let err = d
        .act(
            &Action::Window {
                window_id: None,
                operation: WindowOperation::Focus,
            },
            &ctx(),
        )
        .expect_err("empty window list must be a miss");
    assert!(matches!(err, dexter_driver::DriverError::NotFound(_)));
}

#[test]
fn pointer_scroll_requires_coordinates_optin() {
    // `Scroll` without a target is physical input — `act` enforces the
    // same `allow_coordinates` gate `plan` declares, like `Key` does.
    let d = SimDriver::new(vec![]);
    let scroll = Action::Scroll {
        delta: dexter_core::ScrollDelta { dx: 0.0, dy: 120.0 },
        target: None,
    };
    let r = d.act(&scroll, &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Unsupported);
    let r = d
        .act(
            &scroll,
            &ActContext {
                allow_coordinates: true,
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(r.status, ActionStatus::Success);
}

#[test]
fn click_enforces_the_shared_count_contract() {
    // The same refusals macOS and browser make: count is 1..=3 and
    // multi-click is a left-button gesture. The double diverging is
    // how scenarios pass here and fail in production.
    let d = SimDriver::new(vec![el(1, "button", "Save", &["press"])]);
    let click = |button, count| Action::Click {
        target: semantic("Save"),
        button,
        count,
    };

    let r = d.act(&click(MouseButton::Left, 0), &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Failed);
    let r = d.act(&click(MouseButton::Left, 4), &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Failed);
    let r = d.act(&click(MouseButton::Right, 2), &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Unsupported);

    // The legal shapes still land: a single right-click context menu
    // and a left multi-click.
    let r = d.act(&click(MouseButton::Right, 1), &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Success);
    let r = d.act(&click(MouseButton::Left, 2), &ctx()).unwrap();
    assert_eq!(r.status, ActionStatus::Success);
}

#[test]
fn element_route_descriptor_carries_semantic_identity() {
    // A grant or audit line should read "button Save", not "element 1"
    // — the element handle stays for stale-token validation while
    // role/name give the record its semantic identity.
    let d = SimDriver::new(vec![el(1, "button", "Save", &["press"])]);
    let obs = d.observe(&ObservationScope::default()).unwrap();
    let plan = d
        .plan(
            &Action::Click {
                target: Target::Element {
                    observation: obs.id,
                    element: ElementId(1),
                },
                button: MouseButton::Left,
                count: 1,
            },
            &ctx(),
        )
        .unwrap();
    let t = &plan.routes[0].target;
    assert_eq!(t.element, Some(ElementId(1)));
    assert_eq!(t.observation, Some(obs.id));
    assert_eq!(t.role.as_deref(), Some("button"));
    assert_eq!(t.name.as_deref(), Some("Save"));

    // A password-subrole field: role "text_field" is not sensitive on
    // its own — the engine's secrets floor reads `desc.subrole`, so the
    // descriptor must carry it.
    let mut pwd = el(2, "text_field", "Password", &["set_value", "focus"]);
    pwd.subrole = Some("password".into());
    let d = SimDriver::new(vec![pwd]);
    let obs = d.observe(&ObservationScope::default()).unwrap();
    let plan = d
        .plan(
            &Action::SetValue {
                target: Target::Element {
                    observation: obs.id,
                    element: ElementId(2),
                },
                value: "x".into(),
            },
            &ctx(),
        )
        .unwrap();
    let t = &plan.routes[0].target;
    assert_eq!(t.role.as_deref(), Some("text_field"));
    assert_eq!(t.subrole.as_deref(), Some("password"));
}

#[test]
fn quit_and_close_carry_destructive_sensitivity() {
    // The destructive floor is route metadata: quit_app and window
    // close declare it, so policy gates them before the mutating
    // default — the other window ops stay standard.
    let d = SimDriver::new(vec![]);
    let plan = d
        .plan(
            &Action::QuitApp {
                app: AppSelector::Name("x".into()),
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(plan.routes[0].sensitivity, Sensitivity::Destructive);
    let plan = d
        .plan(
            &Action::Window {
                window_id: Some(1),
                operation: WindowOperation::Close,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(plan.routes[0].sensitivity, Sensitivity::Destructive);
    let plan = d
        .plan(
            &Action::Window {
                window_id: Some(1),
                operation: WindowOperation::Minimize,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(plan.routes[0].sensitivity, Sensitivity::Standard);
    let plan = d
        .plan(
            &Action::LaunchApp {
                app: AppSelector::Name("x".into()),
                activate: false,
            },
            &ctx(),
        )
        .unwrap();
    assert_eq!(plan.routes[0].sensitivity, Sensitivity::Standard);
}
