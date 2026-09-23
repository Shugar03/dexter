//! Hermetic end-to-end: sim world + engine loop + policy + verifier.

use dexter_core::*;
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_policy::{ActionContext, Policy};
use dexter_sim::{Effect, SimDriver};
use std::time::Duration;

fn el(id: u64, role: &str, name: &str) -> Element {
    Element {
        id: ElementId(id),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: vec!["press".into()],
        enabled: Some(true),
        ..Default::default()
    }
}

fn allow_all() -> Policy {
    Policy::from_toml(
        r#"
        [[rule]]
        action = "*"
        decision = "allow"
    "#,
    )
    .unwrap()
}

fn cfg() -> RunConfig {
    RunConfig {
        verify_delay: Duration::from_millis(1),
        ..Default::default()
    }
}

#[test]
fn click_verifies_spawned_element() {
    // World: a "Guardar" button; pressing it spawns a "Guardado" label.
    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                role: Some("button".into()),
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                role: Some("static_text".into()),
                name: Some("Guardado".into()),
                ..Default::default()
            },
        }),
        max_attempts: None,
        app: None,
    };
    let status = engine.run_step(&step, &cfg());
    match status {
        StepStatus::Done {
            verification,
            attempts,
            ..
        } => {
            assert_eq!(attempts, 1);
            assert_eq!(verification.unwrap().status, VerificationStatus::Verified);
        }
        other => panic!("expected Done, got {other:?}"),
    }
    // Journal carries the full audit trail.
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::ActionProposed));
    assert!(kinds.contains(&EventKind::ActionExecuted));
    assert!(kinds.contains(&EventKind::VerificationPassed));
}

#[test]
fn unverifiable_expectation_fails_after_bounded_attempts() {
    let sim = SimDriver::new(vec![el(1, "button", "NoOp")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("NoOp".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Nunca".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(2),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Failed { attempts, .. } => assert_eq!(attempts, 2),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn policy_deny_blocks_action() {
    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    let policy = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        decision = "deny"
        reason = "no clicks allowed"
    "#,
    )
    .unwrap();
    let mut engine = Engine::new(sim, policy, Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: None,
        max_attempts: None,
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Denied { reason } => assert!(reason.contains("no clicks")),
        other => panic!("expected Denied, got {other:?}"),
    }
    // The action never reached the driver.
    assert!(engine.driver().pressed().is_empty());
}

#[test]
fn approval_flow_bound_single_use() {
    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    let mut engine = Engine::new(sim, Policy::embedded(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: None,
        max_attempts: None,
        app: None,
    };
    // No grant -> NeedsApproval with the fingerprint to present.
    let fp = match engine.run_step(&step, &cfg()) {
        StepStatus::NeedsApproval { fingerprint, .. } => fingerprint,
        other => panic!("expected NeedsApproval, got {other:?}"),
    };
    // Grant it -> the same step now runs.
    engine.grant_approval(&fp);
    assert!(engine.run_step(&step, &cfg()).done());
    assert_eq!(engine.driver().pressed().len(), 1);
    // Single-use: the identical step asks again.
    match engine.run_step(&step, &cfg()) {
        StepStatus::NeedsApproval { fingerprint, .. } => assert_eq!(fingerprint, fp),
        other => panic!("expected NeedsApproval again, got {other:?}"),
    }
}

#[test]
fn scenario_stops_at_first_failure() {
    let sim = SimDriver::new(vec![el(1, "button", "A"), el(2, "button", "B")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let mk = |name: &str, expect_name: Option<&str>| Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some(name.into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: expect_name.map(|n| ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some(n.into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(1),
        app: None,
    };
    // Step 2 expects an element that never appears -> scenario stops.
    let steps = vec![mk("A", None), mk("B", Some("Fantasma")), mk("A", None)];
    let report = engine.run_scenario(&steps, &cfg());
    assert!(!report.ok());
    assert_eq!(report.steps.len(), 2);
    // Only A and B were pressed — the third step never ran.
    assert_eq!(engine.driver().pressed().len(), 2);
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::TaskFailed));
}

#[test]
fn fingerprint_matches_policy_binding() {
    // The engine's fingerprint must be the same string a caller would
    // pre-compute for grants — this is the binding contract.
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    };
    let ctx = ActionContext {
        app: Some(AppSelector::Name("Sim".into())),
        target_hint: None,
    };
    let fp = dexter_policy::fingerprint(&action, &ctx);
    assert!(fp.contains("Guardar"));
    assert!(fp.contains("click"));
}

#[test]
fn run_task_goal_to_verified_via_rule_based() {
    // The full closed loop: goal → observe → candidates → rule-based
    // decision → act → world mutates → done_when verifies.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let sim = SimDriver::new(vec![
        el(1, "button", "Guardar"),
        el(2, "button", "Cancelar"),
    ]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let outcome = engine.run_task(
        "click guardar",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 5,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Guardado".into()),
                    ..Default::default()
                },
            },
        },
    );
    match outcome {
        TaskOutcome::Completed { steps } => assert_eq!(steps, 1),
        other => panic!("expected Completed, got {other:?}"),
    }
    assert_eq!(engine.driver().pressed().len(), 1);
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::CandidatesGenerated));
    assert!(kinds.contains(&EventKind::DecisionMade));
    assert!(kinds.contains(&EventKind::TaskCompleted));
}

#[test]
fn run_task_abstains_when_nothing_matches() {
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    // World with no actionable elements for the goal.
    let sim = SimDriver::new(vec![el(1, "static_text", "Solo lectura")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let outcome = engine.run_task(
        "press the submit button",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 5,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Imposible".into()),
                    ..Default::default()
                },
            },
        },
    );
    // No candidates and no busy-world signal: the honest route is
    // abstain, not escalation.
    match outcome {
        TaskOutcome::Abstained { .. } => {}
        other => panic!("expected Abstained, got {other:?}"),
    }
    // Nothing was pressed — the engine abstained rather than flailing.
    assert!(engine.driver().pressed().is_empty());
}
