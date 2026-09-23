//! Contract tests for dexter-policy: deny-by-default, ordered rules,
//! bound single-use approvals. The driver is never involved — policy is
//! evaluated on the action itself.

use dexter_core::{Action, AppSelector, DexterError, KeyChord, Target};
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
        target: Target::Point { x: 1.0, y: 2.0 },
        button: Default::default(),
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
        PolicyDecision::RequireApproval { .. }
    ));
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
fn nonexistent_file_fails_closed() {
    let err = Policy::load(std::path::Path::new("/nonexistent/policy.toml"));
    assert!(matches!(
        err,
        Err(DexterError::Io(_)) | Err(DexterError::Other(_))
    ));
}
