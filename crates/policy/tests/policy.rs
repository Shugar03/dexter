//! Contract tests for dexter-policy: deny-by-default, ordered rules,
//! bound single-use approvals. The driver is never involved — policy is
//! evaluated on the action itself.

use dexter_core::{Action, AppSelector, DexterError, ExecutionRoute, KeyChord, Target};
use dexter_policy::{ActionContext, ApprovalStore, Policy, PolicyDecision};
use std::time::Duration;

fn ctx(app: Option<&str>) -> ActionContext {
    ActionContext {
        app: app.map(AppSelector::parse),
        target_hint: None,
    }
}

fn click() -> Action {
    Action::Click {
        target: Target::Semantic(Default::default()),
        button: Default::default(),
        count: 1,
    }
}

fn coordinate_click() -> Action {
    Action::Click {
        target: Target::Point { x: 1.0, y: 2.0 },
        button: Default::default(),
        count: 1,
    }
}

fn key() -> Action {
    Action::Key {
        chord: KeyChord::parse("cmd+s").unwrap(),
    }
}

#[test]
fn embedded_policy_allows_reads_and_gates_mutations() {
    let p = Policy::embedded();
    assert_eq!(
        p.evaluate(&Action::Observe, &ctx(None)),
        PolicyDecision::Allow
    );
    assert_eq!(
        p.evaluate(&Action::Wait { millis: 10 }, &ctx(None)),
        PolicyDecision::Allow
    );
    assert!(matches!(
        p.evaluate(&click(), &ctx(Some("Safari"))),
        PolicyDecision::RequireApproval { .. }
    ));
    assert!(matches!(
        p.evaluate(&key(), &ctx(Some("Safari"))),
        // Keys are physical input — denied, never merely approval-gated.
        PolicyDecision::Deny { .. }
    ));
}

#[test]
fn physical_actions_are_denied_by_default() {
    // Coordinate clicks move the real cursor — a batch approval for
    // semantic mutations must never cover them silently.
    let p = Policy::embedded();
    assert!(matches!(
        p.evaluate(&coordinate_click(), &ctx(Some("Safari"))),
        PolicyDecision::Deny { .. }
    ));
    // Keys are physical too: no semantic equivalent exists.
    assert!(matches!(
        p.evaluate(&key(), &ctx(Some("Safari"))),
        PolicyDecision::Deny { .. }
    ));
    // Semantic clicks still reach the normal mutating gate.
    assert!(matches!(
        p.evaluate(&click(), &ctx(Some("Safari"))),
        PolicyDecision::RequireApproval { .. }
    ));
}

#[test]
fn physical_default_is_configurable() {
    let p = Policy::from_toml(
        r#"
        [defaults]
        physical = "require_approval"
        "#,
    )
    .unwrap();
    assert!(matches!(
        p.evaluate(&coordinate_click(), &ctx(None)),
        PolicyDecision::RequireApproval { .. }
    ));
}

#[test]
fn intrusiveness_matcher_scopes_a_rule() {
    let p = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        intrusiveness = "physical"
        decision = "allow"
        reason = "coordinate clicks approved for this environment"
        "#,
    )
    .unwrap();
    assert_eq!(
        p.evaluate(&coordinate_click(), &ctx(None)),
        PolicyDecision::Allow
    );
    // A semantic click does not match the physical-scoped rule and
    // falls back to the mutating default.
    assert!(matches!(
        p.evaluate(&click(), &ctx(None)),
        PolicyDecision::RequireApproval { .. }
    ));
}

#[test]
fn permit_physical_fills_absent_but_never_overrides_explicit() {
    // --coords consent: lifts the implicit deny floor…
    let mut p = Policy::embedded();
    p.permit_physical();
    assert!(
        matches!(
            p.evaluate(&coordinate_click(), &ctx(None)),
            PolicyDecision::RequireApproval { .. }
        ),
        "floor passes, then the mutating default still applies"
    );

    // …and an explicit allow in the file keeps physical outright.
    let open = Policy::from_toml(
        r#"
        [defaults]
        physical = "allow"
        mutating = "allow"
        "#,
    )
    .unwrap();
    assert_eq!(
        open.evaluate(&coordinate_click(), &ctx(None)),
        PolicyDecision::Allow
    );

    // …but an explicit deny in the file survives the flag.
    let mut strict = Policy::from_toml(
        r#"
        [defaults]
        physical = "deny"
        "#,
    )
    .unwrap();
    strict.permit_physical();
    assert!(matches!(
        strict.evaluate(&coordinate_click(), &ctx(None)),
        PolicyDecision::Deny { .. }
    ));
}

#[test]
fn invalid_intrusiveness_fails_closed() {
    assert!(Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        intrusiveness = "loud"
        decision = "allow"
        "#
    )
    .is_err());
}

#[test]
fn first_matching_rule_wins() {
    let p = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        decision = "deny"
        reason = "no clicks at all"

        [[rule]]
        action = "click"
        decision = "allow"
        "#,
    )
    .unwrap();
    assert!(matches!(
        p.evaluate(&click(), &ctx(None)),
        PolicyDecision::Deny { .. }
    ));
}

#[test]
fn app_scoped_rule_only_matches_that_app() {
    let p = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        app = "Safari"
        decision = "allow"

        [defaults]
        mutating = "deny"
        "#,
    )
    .unwrap();
    assert_eq!(
        p.evaluate(&click(), &ctx(Some("Safari"))),
        PolicyDecision::Allow
    );
    assert_eq!(
        p.evaluate(&click(), &ctx(Some("safari"))),
        PolicyDecision::Allow,
        "app match is case-insensitive"
    );
    assert!(matches!(
        p.evaluate(&click(), &ctx(Some("Finder"))),
        PolicyDecision::Deny { .. }
    ));
    assert!(matches!(
        p.evaluate(&click(), &ctx(None)),
        PolicyDecision::Deny { .. }
    ));
}

#[test]
fn bundle_scoped_rule_matches_bundle_selector() {
    let p = Policy::from_toml(
        r#"
        [[rule]]
        action = "*"
        app = "bundle:com.apple.Safari"
        decision = "allow"
        "#,
    )
    .unwrap();
    let safari = ActionContext {
        app: Some(AppSelector::BundleId("com.apple.Safari".into())),
        target_hint: None,
    };
    assert_eq!(p.evaluate(&click(), &safari), PolicyDecision::Allow);
    assert!(
        matches!(
            p.evaluate(&click(), &ctx(Some("Safari"))),
            PolicyDecision::RequireApproval { .. }
        ),
        "name selector must not match a bundle: rule"
    );
}

#[test]
fn malformed_policy_is_an_error_not_a_default() {
    assert!(Policy::from_toml("this is not [toml").is_err());
    // A rule with an unknown decision string must also fail closed.
    assert!(Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        decision = "maybe"
        "#
    )
    .is_err());
}

#[test]
fn approvals_are_bound_single_use_and_expiring() {
    let mut store = ApprovalStore::new(Duration::from_secs(60));
    let a = click();
    let c = ctx(Some("Safari"));
    let fp = dexter_policy::fingerprint(&a, &c);

    assert!(!store.check_and_consume(&fp), "nothing granted yet");
    store.grant(&fp);
    assert!(store.check_and_consume(&fp), "granted once");
    assert!(!store.check_and_consume(&fp), "consumed — not reusable");

    // A different context means a different approval.
    let other_fp = dexter_policy::fingerprint(&a, &ctx(Some("Finder")));
    assert!(!store.check_and_consume(&other_fp));
}

#[test]
fn expired_approval_is_denied() {
    let mut store = ApprovalStore::new(Duration::from_millis(0));
    let fp = dexter_policy::fingerprint(&click(), &ctx(None));
    store.grant(&fp);
    std::thread::sleep(Duration::from_millis(2));
    assert!(
        !store.check_and_consume(&fp),
        "expired approvals are invalid"
    );
}

#[test]
fn fingerprint_distinguishes_actions() {
    let f1 = dexter_policy::fingerprint(&click(), &ctx(None));
    let f2 = dexter_policy::fingerprint(&key(), &ctx(None));
    assert_ne!(f1, f2);
}

#[test]
fn structured_target_rule_distinguishes_save_from_delete() {
    // A `target` matcher sees the resolved element label — "Delete" is
    // denied while an otherwise identical "Save" click falls through to
    // the default. This is the rule shape that makes target filtering
    // real (v1's target_hint was never populated).
    let policy = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        target = "delete"
        decision = "deny"
        reason = "destructive control"
    "#,
    )
    .unwrap();
    let click_on = |name: &str| Action::Click {
        target: Target::Semantic(dexter_core::SemanticTarget {
            name: Some(name.into()),
            ..Default::default()
        }),
        button: dexter_core::MouseButton::Left,
        count: 1,
    };
    match policy.evaluate(&click_on("Delete"), &ctx(None)) {
        PolicyDecision::Deny { reason } => assert!(reason.contains("destructive")),
        other => panic!("expected Deny, got {other:?}"),
    }
    match policy.evaluate(&click_on("Save"), &ctx(None)) {
        PolicyDecision::RequireApproval { .. } => {}
        other => panic!("expected default RequireApproval, got {other:?}"),
    }
}

#[test]
fn fingerprint_is_opaque_and_contains_no_payload() {
    let secret = "DEXTER_SECRET_SENTINEL";
    let action = Action::TypeText {
        text: secret.into(),
        target: Some(Target::Focused),
    };
    let fp = dexter_policy::fingerprint(&action, &ctx(Some("TextEdit")));
    assert!(fp.starts_with("sha256:"), "{fp}");
    assert_eq!(fp.len(), "sha256:".len() + 64, "{fp}");
    assert!(!fp.contains(secret), "{fp}");
    assert!(!fp.contains("type_text"), "{fp}");
}

#[test]
fn nonexistent_file_fails_closed() {
    let err = Policy::load(std::path::Path::new("/nonexistent/policy.toml"));
    assert!(matches!(
        err,
        Err(DexterError::Io(_)) | Err(DexterError::Other(_))
    ));
}

// ---------- desktop actions v2 ----------

#[test]
fn v2_action_kinds_match_rules() {
    // A rule per new kind — first-match order is preserved.
    let toml = r#"
[defaults]
mutating = "deny"

[[rule]]
action = "invoke"
decision = "allow"

[[rule]]
action = "launch_app"
decision = "allow"

[[rule]]
action = "quit_app"
decision = "allow"

[[rule]]
action = "window"
decision = "allow"

[[rule]]
action = "clipboard_read"
decision = "allow"

[[rule]]
action = "clipboard_write"
decision = "allow"

[[rule]]
action = "drag"
decision = "allow"
"#;
    let dir = std::env::temp_dir().join(format!("dexter-pol-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("policy.toml");
    std::fs::write(&path, toml).unwrap();
    let policy = Policy::load(&path).unwrap();
    use dexter_core::{AppSelector, WindowOperation};
    let cases: &[Action] = &[
        Action::Invoke {
            target: Target::Focused,
            action: "press".into(),
        },
        Action::LaunchApp {
            app: AppSelector::Name("x".into()),
            activate: true,
        },
        Action::QuitApp {
            app: AppSelector::Name("x".into()),
        },
        Action::Window {
            window_id: None,
            operation: WindowOperation::Focus,
        },
        Action::ReadClipboardText,
        Action::WriteClipboardText { text: "x".into() },
        Action::Drag {
            from: Target::Focused,
            to: Target::Focused,
            duration_ms: 0,
        },
    ];
    for action in cases {
        match policy.evaluate(action, &ctx(None)) {
            PolicyDecision::Allow => {}
            other => panic!("{action:?} should match its rule, got {other:?}"),
        }
    }
}

#[test]
fn secrets_floor_requires_approval_under_mutating_allow() {
    // mutating = "allow" must NOT silently authorize secret-bearing
    // routes — clipboard has its own sensitivity floor.
    let toml = r#"
[defaults]
mutating = "allow"
"#;
    let dir = std::env::temp_dir().join(format!("dexter-pol-sec-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("policy.toml");
    std::fs::write(&path, toml).unwrap();
    let policy = Policy::load(&path).unwrap();

    // A secrets-bearing route (what drivers declare for clipboard).
    let route = ExecutionRoute {
        action: Action::ReadClipboardText,
        target: Default::default(),
        mechanism: Some(dexter_core::Mechanism::Api),
        intrusiveness: dexter_core::Intrusiveness::Background,
        sensitivity: dexter_core::Sensitivity::Secrets,
        requires_foreground: false,
    };
    match policy.evaluate_route(&route, &ctx(None)) {
        PolicyDecision::RequireApproval { .. } => {}
        other => panic!("secrets floor should require approval, got {other:?}"),
    }

    // An explicit clipboard rule still wins — the floor is a default,
    // not a hard veto.
    let toml2 = r#"
[defaults]
mutating = "deny"
[[rule]]
action = "clipboard_read"
decision = "allow"
"#;
    std::fs::write(&path, toml2).unwrap();
    let policy = Policy::load(&path).unwrap();
    match policy.evaluate_route(&route, &ctx(None)) {
        PolicyDecision::Allow => {}
        other => panic!("explicit rule should win over the floor, got {other:?}"),
    }
}

#[test]
fn clipboard_write_payload_binds_the_fingerprint() {
    // A secrets-tier clipboard write digests its text like TypeText /
    // SetValue do — an approval for one payload must never cover
    // another.
    let a = Action::WriteClipboardText {
        text: "first".into(),
    };
    let b = Action::WriteClipboardText {
        text: "second".into(),
    };
    assert_ne!(
        dexter_policy::fingerprint(&a, &ctx(None)),
        dexter_policy::fingerprint(&b, &ctx(None)),
    );
}

#[test]
fn element_target_ids_bind_the_fingerprint() {
    // An element route binds its minted element handle — a grant for
    // element 4 never covers a same-shaped click on element 7. The
    // observation nonce is deliberately *not* bound: grant+retry
    // re-observes, so the same element under a fresh observation id
    // must reproduce the fingerprint or approvals would be
    // unreachable.
    let route = |obs, el| ExecutionRoute {
        action: click(),
        target: dexter_core::TargetDescriptor {
            observation: Some(dexter_core::ObservationId(obs)),
            element: Some(dexter_core::ElementId(el)),
            ..Default::default()
        },
        mechanism: Some(dexter_core::Mechanism::Accessibility),
        intrusiveness: dexter_core::Intrusiveness::Physical,
        sensitivity: dexter_core::Sensitivity::Standard,
        requires_foreground: false,
    };
    let c = ctx(None);
    assert_ne!(
        dexter_policy::fingerprint_route(&route(12, 4), &c),
        dexter_policy::fingerprint_route(&route(12, 7), &c),
    );
    assert_eq!(
        dexter_policy::fingerprint_route(&route(12, 4), &c),
        dexter_policy::fingerprint_route(&route(13, 4), &c),
    );
}

#[test]
fn non_payload_params_bind_the_fingerprint() {
    // A grant binds the action the operator approved, not the action
    // class: chord, url, app, window op, button/count, invoke name,
    // scroll delta, drag destination and wait duration all
    // discriminate.
    let c = ctx(None);
    let fp = |a: &Action| dexter_policy::fingerprint(a, &c);

    let key = |chord: &str| Action::Key {
        chord: KeyChord::parse(chord).unwrap(),
    };
    assert_ne!(fp(&key("return")), fp(&key("cmd+shift+q")));
    assert_eq!(fp(&key("cmd+s")), fp(&key("cmd+s")));

    let nav = |url: &str| Action::Navigate { url: url.into() };
    assert_ne!(fp(&nav("https://a.example")), fp(&nav("https://b.example")));

    let win = |operation| Action::Window {
        window_id: Some(3),
        operation,
    };
    assert_ne!(
        fp(&win(dexter_core::WindowOperation::Minimize)),
        fp(&win(dexter_core::WindowOperation::Close))
    );
    assert_eq!(
        fp(&win(dexter_core::WindowOperation::Minimize)),
        fp(&win(dexter_core::WindowOperation::Minimize))
    );

    let click = |button, count| Action::Click {
        target: Target::Focused,
        button,
        count,
    };
    assert_ne!(
        fp(&click(dexter_core::MouseButton::Left, 1)),
        fp(&click(dexter_core::MouseButton::Right, 1))
    );
    assert_ne!(
        fp(&click(dexter_core::MouseButton::Left, 1)),
        fp(&click(dexter_core::MouseButton::Left, 2))
    );

    let invoke = |action: &str| Action::Invoke {
        target: Target::Focused,
        action: action.into(),
    };
    assert_ne!(fp(&invoke("press")), fp(&invoke("show_menu")));

    let quit = |name: &str| Action::QuitApp {
        app: AppSelector::Name(name.into()),
    };
    assert_ne!(fp(&quit("Safari")), fp(&quit("Notes")));

    let launch = |name: &str, activate: bool| Action::LaunchApp {
        app: AppSelector::Name(name.into()),
        activate,
    };
    assert_ne!(fp(&launch("Safari", true)), fp(&launch("Safari", false)));

    let scroll = |dy: f64| Action::Scroll {
        delta: dexter_core::ScrollDelta { dx: 0.0, dy },
        target: None,
    };
    assert_ne!(fp(&scroll(120.0)), fp(&scroll(-120.0)));

    let drag = |to: Target, duration_ms: u64| Action::Drag {
        from: Target::Focused,
        to,
        duration_ms,
    };
    assert_ne!(
        fp(&drag(Target::Focused, 300)),
        fp(&drag(Target::Point { x: 1.0, y: 1.0 }, 300))
    );
    assert_ne!(
        fp(&drag(Target::Focused, 300)),
        fp(&drag(Target::Focused, 0))
    );

    assert_ne!(
        fp(&Action::Wait { millis: 100 }),
        fp(&Action::Wait { millis: 200 })
    );
}

#[test]
fn destructive_floor_requires_approval_under_mutating_allow() {
    // quit_app / window close can discard unsaved state — a batch
    // `mutating = "allow"` must not silently cover them. Same floor
    // shape as secrets.
    let policy = Policy::from_toml("[defaults]\nmutating = \"allow\"\n").unwrap();
    let route = ExecutionRoute {
        action: Action::QuitApp {
            app: dexter_core::AppSelector::Name("Finder".into()),
        },
        target: Default::default(),
        mechanism: Some(dexter_core::Mechanism::Api),
        intrusiveness: dexter_core::Intrusiveness::Visual,
        sensitivity: dexter_core::Sensitivity::Destructive,
        requires_foreground: false,
    };
    match policy.evaluate_route(&route, &ctx(None)) {
        PolicyDecision::RequireApproval { .. } => {}
        other => panic!("destructive floor should require approval, got {other:?}"),
    }

    // An explicit rule still wins — the floor is a default, not a veto.
    let policy = Policy::from_toml(
        "[defaults]\nmutating = \"deny\"\n[[rule]]\naction = \"quit_app\"\ndecision = \"allow\"\n",
    )
    .unwrap();
    match policy.evaluate_route(&route, &ctx(None)) {
        PolicyDecision::Allow => {}
        other => panic!("explicit quit_app rule should win, got {other:?}"),
    }
}
