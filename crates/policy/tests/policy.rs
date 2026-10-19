//! Contract tests for dexter-policy: deny-by-default, ordered rules,
//! bound single-use approvals. The driver is never involved — policy is
//! evaluated on the action itself.

use dexter_core::{Action, AppSelector, DexterError, KeyChord, Target};
use dexter_policy::{ActionContext, ApprovalStore, Policy, PolicyDecision, RuleSummary};
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
    }
}

fn coordinate_click() -> Action {
    Action::Click {
        target: Target::Point { x: 1.0, y: 2.0 },
        button: Default::default(),
    }
}

fn key() -> Action {
    Action::Key {
        chord: KeyChord::parse("cmd+s").unwrap(),
    }
}

fn fill_credential() -> Action {
    Action::FillCredential {
        target: Target::Semantic(Default::default()),
        credential: "github".into(),
        field: dexter_core::CredentialField::Password,
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
fn credential_fill_requires_an_explicit_rule() {
    let embedded = Policy::embedded();
    match embedded.evaluate(&fill_credential(), &ctx(Some("Browser"))) {
        PolicyDecision::Deny { reason } => assert!(reason.starts_with(
            "credential fill on app 'Browser' requires an explicit `fill_credential` policy rule"
        )),
        other => panic!("expected explicit credential-rule denial, got {other:?}"),
    }

    let wildcard = Policy::from_toml(
        r#"
        [defaults]
        mutating = "allow"

        [[rule]]
        action = "*"
        decision = "allow"
        "#,
    )
    .unwrap();
    assert!(matches!(
        wildcard.evaluate(&fill_credential(), &ctx(Some("Browser"))),
        PolicyDecision::Deny { .. }
    ));

    let allow = Policy::from_toml(
        r#"
        [[rule]]
        action = "fill_credential"
        decision = "allow"
        "#,
    )
    .unwrap();
    assert_eq!(
        allow.evaluate(&fill_credential(), &ctx(Some("Browser"))),
        PolicyDecision::Allow
    );

    let approval = Policy::from_toml(
        r#"
        [[rule]]
        action = "fill_credential"
        decision = "require_approval"
        "#,
    )
    .unwrap();
    assert!(matches!(
        approval.evaluate(&fill_credential(), &ctx(Some("Browser"))),
        PolicyDecision::RequireApproval { .. }
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
    store.pre_grant(&fp);
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
    store.pre_grant(&fp);
    std::thread::sleep(Duration::from_millis(2));
    assert!(
        !store.check_and_consume(&fp),
        "expired approvals are invalid"
    );
}

#[test]
fn grant_requires_a_live_pending_request() {
    let mut store = ApprovalStore::new(Duration::from_secs(60));
    let fp = dexter_policy::fingerprint(&click(), &ctx(None));

    // An arbitrary fingerprint — the caller computed it locally — is
    // not grantable: no approval request was ever escalated for it.
    assert!(!store.grant(&fp), "no pending request, nothing to grant");
    assert!(!store.check_and_consume(&fp));

    // Once the engine escalates it, exactly one grant answers it.
    store.request(&fp);
    assert!(store.grant(&fp));
    assert!(!store.grant(&fp), "the request is consumed by the grant");
    assert!(store.check_and_consume(&fp), "granted once");
    assert!(!store.check_and_consume(&fp), "consumed — not reusable");
}

#[test]
fn deny_removes_a_live_pending_request() {
    let mut store = ApprovalStore::new(Duration::from_secs(60));
    let fp = dexter_policy::fingerprint(&click(), &ctx(None));

    store.request(&fp);
    assert!(store.deny(&fp));
    assert!(
        !store.grant(&fp),
        "a denied request cannot be granted later"
    );
}

#[test]
fn deny_of_an_unknown_fingerprint_fails() {
    let mut store = ApprovalStore::new(Duration::from_secs(60));

    assert!(!store.deny("unknown"));
}

#[test]
fn pending_request_expires() {
    let mut store = ApprovalStore::new(Duration::from_millis(0));
    let fp = dexter_policy::fingerprint(&click(), &ctx(None));
    store.request(&fp);
    std::thread::sleep(Duration::from_millis(2));
    assert!(!store.grant(&fp), "stale requests cannot be granted");
    assert!(!store.check_and_consume(&fp));
}

#[test]
fn fingerprint_distinguishes_actions() {
    let f1 = dexter_policy::fingerprint(&click(), &ctx(None));
    let f2 = dexter_policy::fingerprint(&key(), &ctx(None));
    assert_ne!(f1, f2);
}

#[test]
fn nonexistent_file_fails_closed() {
    let err = Policy::load(std::path::Path::new("/nonexistent/policy.toml"));
    assert!(matches!(
        err,
        Err(DexterError::Io(_)) | Err(DexterError::Other(_))
    ));
}

#[test]
fn embedded_summary_exposes_effective_defaults_and_implicit_floor() {
    let summary = Policy::embedded().summary();

    assert_eq!(summary.mutating, "require_approval");
    assert_eq!(summary.physical, "deny");
    assert!(!summary.physical_explicit);
    assert!(summary.rules.is_empty());
}

#[test]
fn summary_preserves_rules_and_all_rule_fields_in_order() {
    let policy = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        app = "Safari"
        intrusiveness = "background"
        decision = "allow"
        reason = "Safari is allowed"

        [[rule]]
        action = "*"
        decision = "deny"
        reason = "deny remaining actions"
        "#,
    )
    .unwrap();

    assert_eq!(
        policy.summary().rules,
        vec![
            RuleSummary {
                action: "click".into(),
                app: Some("Safari".into()),
                intrusiveness: Some("background".into()),
                decision: "allow".into(),
                reason: "Safari is allowed".into(),
            },
            RuleSummary {
                action: "*".into(),
                app: None,
                intrusiveness: None,
                decision: "deny".into(),
                reason: "deny remaining actions".into(),
            },
        ]
    );
}

#[test]
fn summary_reports_permit_physical_and_explicit_deny_truthfully() {
    let mut implicit = Policy::embedded();
    implicit.permit_physical();
    let summary = implicit.summary();
    assert_eq!(summary.physical, "allow");
    assert!(summary.physical_explicit);

    let mut explicit = Policy::from_toml(
        r#"
        [defaults]
        physical = "deny"
        "#,
    )
    .unwrap();
    explicit.permit_physical();
    let summary = explicit.summary();
    assert_eq!(summary.physical, "deny");
    assert!(summary.physical_explicit);
}
