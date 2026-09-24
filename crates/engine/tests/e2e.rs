//! Hermetic end-to-end: sim world + engine loop + policy + verifier.

use dexter_core::*;
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError, WakeHandle};
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_policy::{ActionContext, Policy};
use dexter_sim::{Effect, SimDriver};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
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
        post_act_settle: Duration::ZERO,
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
            count: 1,
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
            count: 1,
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
            count: 1,
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
    // The click must change the world — a no-op is no longer credited.
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "ok")),
    );
    let mut engine = Engine::new(sim, Policy::embedded(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
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
    // Both clicks land real effects — no-ops are no longer credited.
    for name in ["A", "B"] {
        sim.on_press(
            SemanticTarget {
                name: Some(name.into()),
                ..Default::default()
            },
            Effect::Spawn(el(0, "static_text", name)),
        );
    }
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let mk = |name: &str, expect_name: Option<&str>| Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some(name.into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
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
    // pre-compute for grants — this is the binding contract. v2: opaque
    // (sha256) — the grant binds the canonical route tuple, not a
    // readable payload.
    let action = Action::Click {
        target: Target::Semantic(SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
        count: 1,
    };
    let ctx = ActionContext {
        app: Some(AppSelector::Name("Sim".into())),
        target_hint: None,
    };
    let fp = dexter_policy::fingerprint(&action, &ctx);
    assert!(fp.starts_with("sha256:"), "{fp}");
    assert_eq!(fp, dexter_policy::fingerprint(&action, &ctx));
    assert!(!fp.contains("Guardar"), "{fp}");
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
            max_duration: None,
            cancel: None,
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
fn action_proposed_journals_intrusiveness_and_target_bounds() {
    // Presence contract: an overlay tails the journal and needs the
    // action's intrusiveness tier plus the on-screen rect of its target.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let mut btn = el(1, "button", "Guardar");
    btn.bounds = Some(Rect {
        x: 10.0,
        y: 20.0,
        w: 80.0,
        h: 24.0,
    });
    let sim = SimDriver::new(vec![btn]);
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
            max_steps: 3,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Guardado".into()),
                    ..Default::default()
                },
            },
        },
    );
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));

    let events = engine.events();
    let proposed = events
        .iter()
        .find(|e| e.kind == EventKind::ActionProposed)
        .expect("ActionProposed");
    assert_eq!(proposed.data["intrusiveness"], "background");
    let b = &proposed.data["target_bounds"];
    assert_eq!(b["x"], 10.0, "bounds resolved from the live observation");
    assert_eq!(b["w"], 80.0);
}

#[test]
fn physical_action_denied_before_touching_driver() {
    // A coordinate click with the embedded policy: physical input is
    // denied by default — the driver never sees it.
    let sim = SimDriver::new(vec![el(1, "button", "A")]);
    let mut engine = Engine::new(sim, Policy::embedded(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Point { x: 5.0, y: 5.0 },
            button: MouseButton::Left,
            count: 1,
        },
        expect: None,
        max_attempts: None,
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Denied { reason } => assert!(reason.contains("physical")),
        other => panic!("expected Denied, got {other:?}"),
    }
    let events = engine.events();
    let checked = events
        .iter()
        .find(|e| e.kind == EventKind::PolicyChecked)
        .expect("PolicyChecked");
    assert_eq!(checked.data["intrusiveness"], "physical");
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
            max_duration: None,
            cancel: None,
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

#[test]
fn run_task_cancels_cooperatively() {
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let sim = SimDriver::new(vec![el(1, "button", "Save")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let cancel = Arc::new(AtomicBool::new(true)); // pre-set

    let outcome = engine.run_task(
        "click save",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 10,
            max_duration: None,
            cancel: Some(cancel),
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Nope".into()),
                    ..Default::default()
                },
            },
        },
    );
    assert!(matches!(outcome, TaskOutcome::Cancelled));
    let events = engine.events();
    assert!(events.iter().any(|e| e.kind == EventKind::TaskCancelled));
    // Nothing was acted on — the flag was checked before step 1.
    assert!(engine.driver().pressed().is_empty());
    let _ = Ordering::Relaxed;
}

#[test]
fn run_task_times_out_on_wall_clock() {
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let sim = SimDriver::new(vec![el(1, "button", "Save")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let outcome = engine.run_task(
        "click save",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 10,
            max_duration: Some(Duration::ZERO), // already over budget
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Nope".into()),
                    ..Default::default()
                },
            },
        },
    );
    assert!(matches!(outcome, TaskOutcome::TimedOut { .. }));
    assert!(engine
        .events()
        .iter()
        .any(|e| e.kind == EventKind::TaskTimedOut));
}

#[test]
fn run_plan_executes_subgoals_in_order() {
    // "escribir 'hola' en texto" then "guardar" — two intents, one plan.
    // Auto-completion: each finishes when its act verifiably moved the
    // world (value set / element spawned), not on the engine's say-so.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{PlanOutcome, Subgoal, TaskConfig};

    let mut field = el(1, "text_field", "Texto");
    field.actions = vec!["press".into(), "set_value".into(), "focus".into()];
    let sim = SimDriver::new(vec![field, el(2, "button", "Guardar")]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let outcome = engine.run_plan(
        &[
            Subgoal {
                goal: "escribir 'hola' en texto".into(),
                done_when: None,
            },
            Subgoal {
                goal: "guardar".into(),
                done_when: Some(ExpectedState::ElementExists {
                    target: SemanticTarget {
                        name: Some("Guardado".into()),
                        ..Default::default()
                    },
                }),
            },
        ],
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 6,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget::default(),
            },
        },
    );
    match outcome {
        PlanOutcome::Completed { subgoals, steps } => {
            assert_eq!(subgoals, 2);
            assert_eq!(steps, 2);
        }
        other => panic!("expected Completed, got {other:?}"),
    }
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EventKind::SubgoalStarted)
            .count(),
        2
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EventKind::SubgoalCompleted)
            .count(),
        2
    );
    assert!(!kinds.contains(&EventKind::SubgoalFailed));
    // The write actually landed — subgoal one wasn't vacuous.
    assert!(engine
        .driver()
        .elements()
        .iter()
        .any(|e| e.value.as_deref() == Some("hola")));
}

#[test]
fn run_plan_reports_which_subgoal_failed() {
    // Second subgoal names an element that doesn't exist — the plan must
    // attribute the failure to index 1, with subgoal 0 completed.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{PlanOutcome, Subgoal, TaskConfig};

    let mut field = el(1, "text_field", "Texto");
    field.actions = vec!["press".into(), "set_value".into(), "focus".into()];
    let sim = SimDriver::new(vec![field]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let outcome = engine.run_plan(
        &[
            Subgoal {
                goal: "escribir 'hola' en texto".into(),
                done_when: None,
            },
            Subgoal {
                goal: "pulsar el botón inexistente".into(),
                done_when: None,
            },
        ],
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 4,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget::default(),
            },
        },
    );
    match outcome {
        PlanOutcome::Failed {
            index, completed, ..
        } => {
            assert_eq!(index, 1);
            assert_eq!(completed, 1);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(engine
        .events()
        .iter()
        .any(|e| e.kind == EventKind::SubgoalFailed));
}

#[test]
fn run_plan_auto_complete_requires_world_change() {
    // A successful act on a no-op control must NOT complete the subgoal:
    // auto-completion trusts observed change, never act success alone.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{PlanOutcome, Subgoal, TaskConfig};

    let sim = SimDriver::new(vec![el(1, "button", "NoOp")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));

    let outcome = engine.run_plan(
        &[Subgoal {
            goal: "pulsar noop".into(),
            done_when: None,
        }],
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 4,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget::default(),
            },
        },
    );
    match outcome {
        PlanOutcome::Failed { .. } => {}
        other => panic!("expected Failed (world never changed), got {other:?}"),
    }
}

struct DelayedVerifyDriver {
    execute_count: Arc<AtomicU32>,
    observe_count: Arc<AtomicU32>,
    /// Observation index at which the expected element appears.
    appear_at: u32,
}

impl ComputerDriver for DelayedVerifyDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "test",
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Ok(vec![])
    }

    fn observe(&self, _scope: &ObservationScope) -> Result<Observation, DriverError> {
        let n = self.observe_count.fetch_add(1, Ordering::SeqCst) + 1;
        let mut obs = Observation::default();
        if self.execute_count.load(Ordering::SeqCst) > 0 && n >= self.appear_at {
            obs.elements.push(el(1, "static_text", "Done"));
        }
        Ok(obs)
    }

    fn act(&self, _action: &Action, _ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.execute_count.fetch_add(1, Ordering::SeqCst);
        Ok(ActionResult::success(
            Mechanism::Api,
            Some("executed".into()),
        ))
    }
}

#[test]
fn task_returns_needs_approval_without_spinning() {
    // Embedded policy requires approval for every mutation — the task
    // must surface that as an outcome immediately, not burn max_steps
    // re-deciding around a wall it cannot cross.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    let mut engine = Engine::new(sim, Policy::embedded(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "click guardar",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 5,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Guardado".into()),
                    ..Default::default()
                },
            },
        },
    );
    match outcome {
        TaskOutcome::NeedsApproval { fingerprint, .. } => {
            assert!(fingerprint.starts_with("sha256:"), "{fingerprint}");
        }
        other => panic!("expected NeedsApproval, got {other:?}"),
    }
    // The button was never touched — approval precedes execution.
    assert!(engine.driver().pressed().is_empty());
}

#[test]
fn verification_poll_executes_mutation_once() {
    let execute_count = Arc::new(AtomicU32::new(0));
    let observe_count = Arc::new(AtomicU32::new(0));
    let driver = DelayedVerifyDriver {
        execute_count: execute_count.clone(),
        observe_count,
        appear_at: 2,
    };
    let mut engine = Engine::new(driver, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Navigate {
            url: "https://example.test".into(),
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Done".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(3),
        app: None,
    };
    assert!(engine.run_step(&step, &cfg()).done());
    assert_eq!(execute_count.load(Ordering::SeqCst), 1);
}

// -- recovery accounting: a Started must pair with a Completed --

#[test]
fn verify_poll_recovery_closes_the_pair() {
    // The act lands on the first execute but the world only shows it on
    // the second poll — attempt > 1 means a recovery was entered and,
    // when it verifies, the journal must record the completion.
    let driver = DelayedVerifyDriver {
        execute_count: Arc::new(AtomicU32::new(0)),
        observe_count: Arc::new(AtomicU32::new(0)),
        appear_at: 2,
    };
    let mut engine = Engine::new(driver, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Navigate {
            url: "https://example.test".into(),
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Done".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(4),
        app: None,
    };
    assert!(engine.run_step(&step, &cfg()).done());
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EventKind::RecoveryStarted)
            .count(),
        1,
        "one verify-poll retry entered"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EventKind::RecoveryCompleted)
            .count(),
        1,
        "the recovery verified — it must close"
    );
}

#[test]
fn unrecovered_poll_never_claims_completion() {
    // The effect never lands: every retry starts a recovery attempt,
    // none completes — `started − completed` is the failure count.
    let driver = DelayedVerifyDriver {
        execute_count: Arc::new(AtomicU32::new(0)),
        observe_count: Arc::new(AtomicU32::new(0)),
        appear_at: u32::MAX,
    };
    let mut engine = Engine::new(driver, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Navigate {
            url: "https://example.test".into(),
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Nunca".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(3),
        app: None,
    };
    assert!(!engine.run_step(&step, &cfg()).done());
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::RecoveryStarted));
    assert!(!kinds.contains(&EventKind::RecoveryCompleted));
}

/// First planned route is always `Unsupported`; the fallback actually
/// works — exercises the `next_route` recovery path.
struct FallbackDriver(SimDriver);

impl ComputerDriver for FallbackDriver {
    fn capabilities(&self) -> DriverCapabilities {
        self.0.capabilities()
    }
    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        self.0.windows()
    }
    fn observe(&self, s: &ObservationScope) -> Result<Observation, DriverError> {
        self.0.observe(s)
    }
    fn act(&self, a: &Action, c: &ActContext) -> Result<ActionResult, DriverError> {
        self.0.act(a, c)
    }
    fn plan(&self, action: &Action, _ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        let route = |mechanism| ExecutionRoute {
            action: action.clone(),
            target: TargetDescriptor::from_action(action),
            mechanism: Some(mechanism),
            intrusiveness: Intrusiveness::Background,
            sensitivity: Sensitivity::Standard,
            requires_foreground: false,
        };
        Ok(ExecutionPlan {
            requested: action.clone(),
            routes: vec![route(Mechanism::Vision), route(Mechanism::Api)],
        })
    }
    fn execute(
        &self,
        route: &ExecutionRoute,
        ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        if route.mechanism == Some(Mechanism::Vision) {
            return Ok(ActionResult::failure(
                ActionStatus::Unsupported,
                Mechanism::Vision,
                "no vision backend",
            ));
        }
        self.0.act(&route.action, ctx)
    }
}

#[test]
fn fallback_route_completion_is_journaled() {
    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(2, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(FallbackDriver(sim), allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Guardado".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(1),
        app: None,
    };
    assert!(engine.run_step(&step, &cfg()).done());
    let events = engine.events();
    let started = events
        .iter()
        .filter(|e| e.kind == EventKind::RecoveryStarted)
        .count();
    let completed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::RecoveryCompleted)
        .collect();
    assert_eq!(started, 1, "the Unsupported route starts a recovery");
    assert_eq!(completed.len(), 1, "the fallback landing closes it");
    assert_eq!(completed[0].data["strategy"], "next_route");
}

// -- loop-integrity slice 1: verify-every-act inside run_goal --

#[test]
fn goal_act_that_changes_nothing_is_not_credited() {
    // Silent no-op: a dead button, no on_press rule. The click executes,
    // the world provably does not change, and the loop must NOT treat
    // the act as progress — the step fails verification and the task
    // ends without a Completed claim.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let sim = SimDriver::new(vec![el(1, "button", "Dead")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "click dead",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 3,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Nunca".into()),
                    ..Default::default()
                },
            },
        },
    );
    assert!(
        !matches!(outcome, TaskOutcome::Completed { .. }),
        "a no-op click must never complete the task: {outcome:?}"
    );
    // The journal carries *why*: verification ran and could not confirm.
    let failed_checks: Vec<String> = engine
        .events()
        .iter()
        .filter(|e| e.kind == EventKind::VerificationFailed)
        .flat_map(|e| {
            e.data
                .get("checks")
                .and_then(|c| c.as_array().cloned())
                .unwrap_or_default()
                .into_iter()
                .filter_map(|c| c.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        failed_checks.iter().any(|c| c.contains("world_changed")),
        "expected a world_changed verdict in the journal: {failed_checks:?}"
    );
}

#[test]
fn goal_act_verified_carries_evidence() {
    // The positive half: a click whose effect lands earns a
    // VerificationPassed — every mutating act in goal flow is verified,
    // not just trusted.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
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
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("Guardado".into()),
                    ..Default::default()
                },
            },
        },
    );
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert!(
        kinds.contains(&EventKind::VerificationPassed),
        "goal-flow acts must be verified, not trusted: {kinds:?}"
    );
}

// ---------- desktop actions v2 ----------

#[test]
fn launch_app_verified_by_window_observation() {
    // LaunchApp derives AppRunning — the world must show the app's
    // window before the step is credited.
    let sim = SimDriver::new(vec![]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::LaunchApp {
            app: AppSelector::Name("Calc".into()),
            activate: true,
        },
        expect: None, // derived: AppRunning("Calc")
        max_attempts: None,
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { verification, .. } => {
            let v = verification.expect("launch must carry a verdict");
            assert_eq!(v.status, VerificationStatus::Verified);
        }
        other => panic!("launch should complete verified, got {other:?}"),
    }
}

#[test]
fn quit_app_verified_by_window_removal() {
    let sim = SimDriver::new(vec![]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    // The sim itself runs as "sim" — quitting it removes its window.
    let step = Step {
        note: None,
        action: Action::QuitApp {
            app: AppSelector::Name("sim".into()),
        },
        expect: None, // derived: Not(AppRunning("sim"))
        max_attempts: None,
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { verification, .. } => {
            assert_eq!(
                verification.expect("quit must carry a verdict").status,
                VerificationStatus::Verified
            );
        }
        other => panic!("quit should complete verified, got {other:?}"),
    }
}

#[test]
fn launch_of_pid_selector_is_invalid() {
    // A pid selector names a running process — it can't be launched.
    // The failure is honest, not a simulated success.
    let sim = SimDriver::new(vec![]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::LaunchApp {
            app: AppSelector::Pid(9999),
            activate: true,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { .. } | StepStatus::Errored { .. } => {}
        other => panic!("pid launch should error or fail, got {other:?}"),
    }
}

#[test]
fn invoke_verified_via_world_changed() {
    // Invoke derives WorldChanged — the press must actually do
    // something observable, not just return success.
    let sim = SimDriver::new(vec![el(1, "button", "Go")]);
    sim.on_press(
        SemanticTarget {
            name: Some("Go".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "started")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Invoke {
            target: Target::Semantic(SemanticTarget {
                name: Some("Go".into()),
                ..Default::default()
            }),
            action: "press".into(),
        },
        expect: None, // derived: WorldChanged
        max_attempts: None,
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { verification, .. } => {
            assert_eq!(
                verification.expect("invoke must carry a verdict").status,
                VerificationStatus::Verified
            );
        }
        other => panic!("invoke should complete verified, got {other:?}"),
    }
}

#[test]
fn invoke_unadvertised_action_is_not_credited() {
    // Element doesn't advertise the requested action — honest
    // Unsupported, never a fake press.
    let sim = SimDriver::new(vec![el(1, "button", "Go")]); // advertises "press"
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Invoke {
            target: Target::Semantic(SemanticTarget {
                name: Some("Go".into()),
                ..Default::default()
            }),
            action: "explode".into(),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { .. } => panic!("unadvertised invoke must not succeed"),
        StepStatus::Failed { .. } | StepStatus::Errored { .. } | StepStatus::Denied { .. } => {}
        other => panic!("got {other:?}"),
    }
}

#[test]
fn clipboard_write_then_read_via_engine() {
    // Clipboard routes carry the secrets floor — under allow-all rules
    // they still execute; the content returns in the result, and the
    // journal must never show it.
    let sentinel = "CLIPBOARD_SENTINEL_9x2";
    let toml = r#"
        [[rule]]
        action = "clipboard_write"
        decision = "allow"
        [[rule]]
        action = "clipboard_read"
        decision = "allow"
    "#;
    let policy = Policy::from_toml(toml).unwrap();
    let sim = SimDriver::new(vec![]);
    let mut engine = Engine::new(sim, policy, Duration::from_secs(60));
    let write = Step {
        note: None,
        action: Action::WriteClipboardText {
            text: sentinel.into(),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    assert!(engine.run_step(&write, &cfg()).done());

    // Read returns content in the result — and the journal redacts it.
    let read = Step {
        note: None,
        action: Action::ReadClipboardText,
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&read, &cfg()) {
        StepStatus::Done { result, .. } => {
            assert_eq!(result.and_then(|r| r.detail).as_deref(), Some(sentinel));
        }
        other => panic!("clipboard read should succeed, got {other:?}"),
    }
    let journal: Vec<String> = engine
        .events()
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(
        !journal.iter().any(|l| l.contains(sentinel)),
        "clipboard content must never reach the journal"
    );
}

#[test]
fn navigate_url_query_never_reaches_the_journal() {
    // Query strings carry signed tokens — the policy fingerprint binds
    // the full URL, but the journal must keep only the redacted form
    // in the action summary and the result detail alike.
    let sim = SimDriver::new(vec![el(1, "button", "Save")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Navigate {
            url: "https://app.test/callback?session=hunter2tok".into(),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    assert!(engine.run_step(&step, &cfg()).done());
    let journal: Vec<String> = engine
        .events()
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(
        !journal.iter().any(|l| l.contains("hunter2tok")),
        "the signed query must never reach the journal"
    );
    assert!(
        journal
            .iter()
            .any(|l| l.contains("https://app.test/callback?[redacted]")),
        "the redacted origin+path stays operator-legible"
    );
}

#[test]
fn clipboard_write_needs_approval_under_default_policy() {
    // The secrets floor: no matching rule → approval required, even
    // though clipboard is a semantic (background) action.
    let policy = Policy::embedded();
    let sim = SimDriver::new(vec![]);
    let mut engine = Engine::new(sim, policy, Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::WriteClipboardText { text: "x".into() },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::NeedsApproval { .. } => {}
        other => panic!("clipboard write should need approval, got {other:?}"),
    }
}

#[test]
fn drag_stale_endpoint_never_reaches_the_world() {
    // A stale `to` fails the drag before anything moves — no partial
    // gesture, no pressed state.
    let sim = SimDriver::new(vec![el(1, "row", "file"), el(2, "row", "folder")]);
    let obs = sim
        .observe(&dexter_core::ObservationScope::default())
        .unwrap();
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Drag {
            from: Target::Semantic(SemanticTarget {
                name: Some("file".into()),
                ..Default::default()
            }),
            to: Target::Element {
                observation: obs.id,
                element: ElementId(999), // vanished — never existed
            },
            duration_ms: 10,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { .. } => panic!("stale drag must not succeed"),
        StepStatus::Failed { .. } | StepStatus::Errored { .. } => {}
        other => panic!("got {other:?}"),
    }
    assert!(engine.driver().dragged().is_empty());
}

#[test]
fn window_ops_verified_or_honestly_unsupported() {
    use dexter_core::WindowOperation as Op;
    let sim = SimDriver::new(vec![]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    // New spawns a window — the world signature shifts.
    let step = Step {
        note: None,
        action: Action::Window {
            window_id: None,
            operation: Op::New,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { .. } => {}
        other => panic!("window new should succeed in sim, got {other:?}"),
    }
    // Focus on a window target normalizes to the window op.
    let wins = engine
        .driver()
        .observe(&ObservationScope::default())
        .unwrap();
    let step = Step {
        note: None,
        action: Action::Focus {
            target: Target::Window {
                window_id: wins.windows[0].id,
            },
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { .. } => {}
        other => panic!("window focus should succeed, got {other:?}"),
    }
}

#[test]
fn goal_flow_open_affordance_yields_invoke() {
    // The acceptance case: an element advertising `open` under an
    // open-goal generates the semantic invoke, and the engine picks it
    // over the generic click (higher prior for the proven affordance).
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TaskOutcome};

    let file = Element {
        id: ElementId(1),
        role: Some("row".into()),
        name: Some("report.pdf".into()),
        actions: vec!["open".into(), "press".into()],
        enabled: Some(true),
        ..Default::default()
    };
    let sim = SimDriver::new(vec![file]);
    sim.on_press(
        SemanticTarget {
            name: Some("report.pdf".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "opened")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "open report.pdf",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 5,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementExists {
                target: SemanticTarget {
                    name: Some("opened".into()),
                    ..Default::default()
                },
            },
        },
    );
    assert!(matches!(outcome, TaskOutcome::Completed { .. }));
    // The journal shows the semantic invoke ran — not a raw click.
    let invoked = engine.events().iter().any(|e| {
        serde_json::to_string(e)
            .map(|s| s.contains("\"invoke\""))
            .unwrap_or(false)
    });
    assert!(invoked, "open-affordance should route through Invoke");
}

// -- runtime-reliability-v2 review fixes --

/// A driver whose `execute` reports a mechanism other than the route
/// declared — the plan→execute contract violation the engine must
/// refuse rather than launder.
struct MismatchDriver;

impl ComputerDriver for MismatchDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "mismatch",
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Ok(vec![])
    }

    fn observe(&self, _scope: &ObservationScope) -> Result<Observation, DriverError> {
        Ok(Observation::default())
    }

    fn plan(&self, action: &Action, _ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        Ok(ExecutionPlan {
            requested: action.clone(),
            routes: vec![ExecutionRoute {
                action: action.clone(),
                target: TargetDescriptor::from_action(action),
                mechanism: Some(Mechanism::Api),
                intrusiveness: Intrusiveness::Background,
                sensitivity: Sensitivity::Standard,
                requires_foreground: false,
            }],
        })
    }

    fn execute(
        &self,
        _route: &ExecutionRoute,
        _ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        // Claims Api at plan time, ran Coordinates at execute time —
        // physical input under a background-authorized route.
        Ok(ActionResult::success(
            Mechanism::Coordinates,
            Some("sneaky".into()),
        ))
    }

    fn act(&self, _action: &Action, _ctx: &ActContext) -> Result<ActionResult, DriverError> {
        Ok(ActionResult::success(Mechanism::Api, None))
    }
}

#[test]
fn mechanism_mismatch_fails_instead_of_laundering() {
    let mut engine = Engine::new(MismatchDriver, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Wait { millis: 1 },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Failed { reason, .. } => {
            assert!(reason.contains("mechanism mismatch"), "{reason}")
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    let kinds: Vec<_> = engine.events().iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EventKind::ActionFailed));
}

/// A driver whose observation path is down — the pre-act observe
/// fails, the act proceeds unverified, and the journal must say why.
struct FailObserveDriver {
    executes: Arc<AtomicU32>,
    wakes: Arc<AtomicU32>,
}

impl ComputerDriver for FailObserveDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "fail-observe",
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Ok(vec![])
    }

    fn observe(&self, _scope: &ObservationScope) -> Result<Observation, DriverError> {
        Err(DriverError::Platform("AX API unavailable".into()))
    }

    fn act(&self, _action: &Action, _ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        Ok(ActionResult::success(Mechanism::Api, Some("ran".into())))
    }

    fn wake(&self, _app: &AppSelector) -> Result<WakeHandle, DriverError> {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        Ok(WakeHandle::activated(None))
    }
}

#[test]
fn failed_pre_observation_journals_and_never_wakes() {
    let executes = Arc::new(AtomicU32::new(0));
    let wakes = Arc::new(AtomicU32::new(0));
    let driver = FailObserveDriver {
        executes: executes.clone(),
        wakes: wakes.clone(),
    };
    let mut engine = Engine::new(driver, allow_all(), Duration::from_secs(60));
    // A click is a stage-needing action — with no observation at all,
    // the engine must not foreground the app to find out.
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("Anything".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: None,
        max_attempts: Some(1),
        app: Some(AppSelector::Name("ghost".into())),
    };
    let status = engine.run_step(&step, &cfg());
    match status {
        StepStatus::Done { verification, .. } => {
            assert!(verification.is_none(), "unverified, not faked");
        }
        other => panic!("expected Done (unverified), got {other:?}"),
    }
    assert_eq!(executes.load(Ordering::SeqCst), 1, "the act still ran");
    assert_eq!(
        wakes.load(Ordering::SeqCst),
        0,
        "absent observation must never trigger a wake"
    );
    let events = engine.events();
    let fail_events: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::ObservationFailed)
        .collect();
    assert_eq!(
        fail_events
            .iter()
            .filter(|e| e.data["context"] == "pre_act")
            .count(),
        1,
        "the pre-act observe failure is journaled: {:?}",
        engine.events()
    );
}

#[test]
fn stage_free_actions_never_wake_the_app() {
    // `dexter launch Foo --background` and `wait` must not steal focus:
    // before the stage gate, maybe_wake fired on any absent-window
    // observation regardless of what the action needed.
    let sim = SimDriver::new(vec![el(1, "button", "A")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let c = RunConfig {
        app: Some(AppSelector::Name("sim".into())),
        ..cfg()
    };
    let launch = Step {
        note: None,
        action: Action::LaunchApp {
            app: AppSelector::Name("ghost".into()),
            activate: false,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    assert!(engine.run_step(&launch, &c).done());
    let wait = Step {
        note: None,
        action: Action::Wait { millis: 1 },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    assert!(engine.run_step(&wait, &c).done());
    assert!(
        engine.driver().wakes().is_empty(),
        "stage-free acts woke: {:?}",
        engine.driver().wakes()
    );

    // A stage-needing act whose route is background-capable (sim's
    // semantic click resolves without the stage) still does not wake —
    // the borrow belongs to the route that executes, not the action.
    let click = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("A".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    engine.run_step(&click, &c);
    assert!(
        engine.driver().wakes().is_empty(),
        "background route stole focus: {:?}",
        engine.driver().wakes()
    );

    // Positive control: a route that declares `requires_foreground`
    // (physical key input) DOES borrow the stage — the gate narrows
    // the borrow to the route that needs it, it doesn't remove it.
    let coords_cfg = RunConfig {
        allow_coordinates: true,
        ..c.clone()
    };
    let key = Step {
        note: None,
        action: Action::Key {
            chord: KeyChord::parse("a").unwrap(),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    engine.run_step(&key, &coords_cfg);
    assert_eq!(engine.driver().wakes(), vec!["sim".to_string()]);
}

/// A driver whose app only exposes window content once `wake` runs —
/// the Activation path's test double (macOS hides the AX window tree
/// of a non-frontmost app the same way). The default plan yields the
/// legacy route, which the engine marks `requires_foreground` for
/// stage-needing actions.
struct StageDriver {
    wakes: Arc<AtomicU32>,
    restores: Arc<AtomicU32>,
    executes: Arc<AtomicU32>,
}

impl StageDriver {
    fn new() -> (Self, Arc<AtomicU32>, Arc<AtomicU32>, Arc<AtomicU32>) {
        let wakes = Arc::new(AtomicU32::new(0));
        let restores = Arc::new(AtomicU32::new(0));
        let executes = Arc::new(AtomicU32::new(0));
        (
            Self {
                wakes: wakes.clone(),
                restores: restores.clone(),
                executes: executes.clone(),
            },
            wakes,
            restores,
            executes,
        )
    }
}

impl ComputerDriver for StageDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "stage",
            element_tree: true,
            screenshots: false,
            background_input: true,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        Ok(vec![])
    }

    fn observe(&self, _scope: &ObservationScope) -> Result<Observation, DriverError> {
        // Pre-wake the app shows no window tree; post-wake one appears.
        let mut elements = vec![];
        if self.wakes.load(Ordering::SeqCst) > 0 {
            elements.push(el(1, "window", "Main"));
            elements.push(el(2, "button", "A"));
        }
        // The act has a real effect — the derived WorldChanged
        // expectation has something to see.
        if self.executes.load(Ordering::SeqCst) > 0 {
            elements.push(el(3, "static_text", "done"));
        }
        Ok(Observation {
            id: ObservationId(1),
            timestamp: std::time::SystemTime::now(),
            elements,
            ..Default::default()
        })
    }

    fn act(&self, _action: &Action, _ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.executes.fetch_add(1, Ordering::SeqCst);
        Ok(ActionResult::success(Mechanism::Api, Some("ran".into())))
    }

    fn wake(&self, _app: &AppSelector) -> Result<WakeHandle, DriverError> {
        self.wakes.fetch_add(1, Ordering::SeqCst);
        Ok(WakeHandle::activated(None))
    }

    fn restore(&self, _handle: &WakeHandle) {
        self.restores.fetch_add(1, Ordering::SeqCst);
    }
}

fn stage_click() -> Step {
    Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("A".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: None,
        max_attempts: Some(1),
        app: Some(AppSelector::Name("stage-app".into())),
    }
}

#[test]
fn denied_stage_borrow_never_activates() {
    // The reviewer's critical: a step policy will deny must not
    // activate the app first. Deny the activation while allowing the
    // click — the step fails with the stage refusal and zero wakes.
    let (driver, wakes, _, executes) = StageDriver::new();
    let policy = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        decision = "allow"
        [[rule]]
        action = "launch_app"
        decision = "deny"
        "#,
    )
    .unwrap();
    let mut engine = Engine::new(driver, policy, Duration::from_secs(60));
    match engine.run_step(&stage_click(), &cfg()) {
        StepStatus::Failed { reason, .. } => {
            assert!(reason.contains("stage borrow refused"), "{reason}")
        }
        other => panic!("expected Failed (stage refused), got {other:?}"),
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "denied stage still woke");
    assert_eq!(executes.load(Ordering::SeqCst), 0, "refused route ran");
}

#[test]
fn stage_borrow_approval_then_grant_activates_and_restores() {
    let (driver, wakes, restores, executes) = StageDriver::new();
    let policy = Policy::from_toml(
        r#"
        [[rule]]
        action = "click"
        decision = "allow"
        [[rule]]
        action = "launch_app"
        decision = "require_approval"
        "#,
    )
    .unwrap();
    let mut engine = Engine::new(driver, policy, Duration::from_secs(60));
    let fp = match engine.run_step(&stage_click(), &cfg()) {
        StepStatus::NeedsApproval { fingerprint, .. } => fingerprint,
        other => panic!("expected NeedsApproval for the stage, got {other:?}"),
    };
    assert_eq!(wakes.load(Ordering::SeqCst), 0, "unapproved stage woke");
    engine.grant_approval(&fp);
    match engine.run_step(&stage_click(), &cfg()) {
        StepStatus::Done { .. } => {}
        other => panic!("granted stage should run, got {other:?}"),
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1);
    assert_eq!(executes.load(Ordering::SeqCst), 1);
    assert_eq!(
        restores.load(Ordering::SeqCst),
        1,
        "focus must be handed back after the step"
    );
}

#[test]
fn post_wake_route_is_reauthorized() {
    // The first verdict was computed on a windowless world — after
    // activation the route is re-evaluated rather than executed on the
    // stale authorization. The consumed single-use grant doesn't cover
    // the second ask, so the route re-surfaces NeedsApproval instead
    // of running. (A semantic descriptor's bound tuple is unchanged by
    // the wake — element-token routes can see the fingerprint move.)
    let (driver, wakes, _, executes) = StageDriver::new();
    let policy = Policy::from_toml(
        r#"
        [defaults]
        mutating = "allow"
        [[rule]]
        action = "click"
        decision = "require_approval"
        [[rule]]
        action = "launch_app"
        decision = "allow"
        "#,
    )
    .unwrap();
    let mut engine = Engine::new(driver, policy, Duration::from_secs(60));
    let fp1 = match engine.run_step(&stage_click(), &cfg()) {
        StepStatus::NeedsApproval { fingerprint, .. } => fingerprint,
        other => panic!("expected NeedsApproval, got {other:?}"),
    };
    engine.grant_approval(&fp1);
    match engine.run_step(&stage_click(), &cfg()) {
        StepStatus::NeedsApproval { fingerprint, .. } => {
            assert_eq!(fingerprint, fp1, "same bound tuple re-asked")
        }
        other => panic!("post-wake route must re-ask, got {other:?}"),
    }
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "stage borrow ran once");
    assert_eq!(
        executes.load(Ordering::SeqCst),
        0,
        "route ran on a stale authorization"
    );
    // Grant again — now the world is already windowed, no borrow is
    // needed, and the act executes.
    engine.grant_approval(&fp1);
    match engine.run_step(&stage_click(), &cfg()) {
        StepStatus::Done { .. } => {}
        other => panic!("granted post-wake route should run, got {other:?}"),
    }
    assert_eq!(executes.load(Ordering::SeqCst), 1);
    assert_eq!(wakes.load(Ordering::SeqCst), 1, "no second borrow needed");
}

#[test]
fn typing_into_sensitive_field_is_unverified_and_types_once() {
    // Secure fields redact `value` at collection — no value expectation
    // can ever verify there. The old code derived one anyway: a
    // physically-successful password entry failed every poll, and a
    // retry would append the secret a second time.
    let mut pw = el(1, "secure_text_field", "Password");
    pw.value = None; // redacted, as a real driver reports it
    let sim = SimDriver::new(vec![pw]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::TypeText {
            text: "s3cret".into(),
            target: Some(Target::Semantic(SemanticTarget {
                name: Some("Password".into()),
                ..Default::default()
            })),
        },
        expect: None,
        max_attempts: Some(3),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done {
            verification,
            attempts,
            ..
        } => {
            assert!(
                verification.is_none(),
                "sensitive target derives no value expectation"
            );
            assert_eq!(attempts, 1, "no failed-verify retry may re-type");
        }
        other => panic!("expected Done (unverified), got {other:?}"),
    }
    let els = engine.driver().elements();
    assert_eq!(
        els[0].value.as_deref(),
        Some("s3cret"),
        "the secret lands exactly once — never re-appended by a retry"
    );
    assert!(
        !engine
            .events()
            .iter()
            .any(|e| e.kind == EventKind::VerificationFailed),
        "no verification may run on a redacted value"
    );
}

// ---------- review round 2 ----------

/// `execute` reports failure *after* the side effect landed — the
/// classic timed-out reply to a successful click. With an expectation
/// in hand the engine must verify before declaring the act dead:
/// VERIFIED means the step did its job despite the broken report.
struct EffectThenErrorDriver(SimDriver);

impl ComputerDriver for EffectThenErrorDriver {
    fn capabilities(&self) -> DriverCapabilities {
        self.0.capabilities()
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        self.0.windows()
    }

    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        self.0.observe(scope)
    }

    fn plan(&self, action: &Action, ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        self.0.plan(action, ctx)
    }

    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.0.act(action, ctx)
    }

    fn execute(
        &self,
        route: &ExecutionRoute,
        ctx: &ActContext,
    ) -> Result<ActionResult, DriverError> {
        // The act runs (and mutates the world) — only the report is lost.
        self.0.act(&route.action, ctx)?;
        Err(DriverError::Timeout("delivery report lost".into()))
    }
}

#[test]
fn execute_error_verifies_landed_effect_before_failing() {
    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(
        EffectThenErrorDriver(sim),
        allow_all(),
        Duration::from_secs(60),
    );
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                role: Some("button".into()),
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Guardado".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done {
            result,
            verification,
            ..
        } => {
            assert!(
                result.is_none(),
                "no delivery report exists to relay — only the verdict"
            );
            assert_eq!(verification.unwrap().status, VerificationStatus::Verified);
        }
        other => panic!("expected Done (verified post-error), got {other:?}"),
    }
}

#[test]
fn menu_target_under_window_scope_derives_no_expectation() {
    // Under a pinned window the menu window can't enter the window
    // list and menu elements are signature-excluded — a WorldChanged
    // expectation would poll for a change it cannot see. The honest
    // derive is none. The menu item is *boundless* — the state the
    // real macOS driver always produces (`walk_menu` mints no bounds),
    // so scoping drops it and the target can't resolve in-scope.
    let menu = el(1, "menu_item", "Archivo");
    let sim = SimDriver::new(vec![menu]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                role: Some("menu_item".into()),
                name: Some("Archivo".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    let mut scoped = cfg();
    scoped.window_scope = Some(1);
    match engine.run_step(&step, &scoped) {
        StepStatus::Done { verification, .. } => {
            assert!(
                verification.is_none(),
                "menu act under window scope must not derive an unsatisfiable expectation"
            );
        }
        other => panic!("expected Done (unverified), got {other:?}"),
    }
}

#[test]
fn menu_act_without_scope_derives_no_expectation() {
    // Unscoped the defect is the same: a menu press that mutates only
    // signature-excluded menu state (checkmark toggle, silent command)
    // leaves the signature identical — WorldChanged would false-fail
    // the success and invite a re-mutation in a goal loop.
    let menu = el(1, "menu_item", "Archivo"); // boundless, like the real driver
    let sim = SimDriver::new(vec![menu]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Invoke {
            target: Target::Semantic(SemanticTarget {
                role: Some("menu_item".into()),
                name: Some("Archivo".into()),
                ..Default::default()
            }),
            action: "press".into(),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { verification, .. } => {
            assert!(
                verification.is_none(),
                "menu act must not derive a signature it cannot move"
            );
        }
        other => panic!("expected Done (unverified), got {other:?}"),
    }
}

#[test]
fn out_of_scope_target_derives_no_expectation() {
    // A semantic target resolving *outside* the pinned window acts on
    // a world verification can't see — the same unverifiable class as
    // menu items. The driver resolves via its own (unscoped) state,
    // the act lands, and the honest verdict is unverified — never a
    // false failure that invites re-mutation.
    let mut far = el(1, "button", "Otra ventana");
    far.bounds = Some(Rect {
        x: 2000.0,
        y: 0.0,
        w: 80.0,
        h: 24.0,
    });
    let sim = SimDriver::new(vec![far]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                role: Some("button".into()),
                name: Some("Otra ventana".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    let mut scoped = cfg();
    scoped.window_scope = Some(1);
    match engine.run_step(&step, &scoped) {
        StepStatus::Done { verification, .. } => {
            assert!(
                verification.is_none(),
                "an out-of-scope act can't be verified in the pinned world"
            );
        }
        other => panic!("expected Done (unverified), got {other:?}"),
    }
}

#[test]
fn launch_app_under_window_scope_derives_no_expectation() {
    // A launch changes the window *set* — under a pinned window the
    // new window can't enter the pinned list, so neither AppRunning
    // nor WorldChanged can observe it.
    let sim = SimDriver::new(vec![el(1, "button", "Save")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::LaunchApp {
            app: AppSelector::Name("Notes".into()),
            activate: true,
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    let mut scoped = cfg();
    scoped.window_scope = Some(1);
    match engine.run_step(&step, &scoped) {
        StepStatus::Done { verification, .. } => {
            assert!(
                verification.is_none(),
                "a launch under window scope can't be verified in the pinned world"
            );
        }
        other => panic!("expected Done (unverified), got {other:?}"),
    }
}

#[test]
fn sensitive_target_route_gets_secrets_floor() {
    // A route into a secure field upgrades to Secrets at the engine
    // seam — under `mutating = "allow"` the floor still gates it, on
    // every driver, whatever the driver declared.
    let policy = Policy::from_toml(
        r#"
[defaults]
mutating = "allow"
"#,
    )
    .unwrap();
    let sim = SimDriver::new(vec![el(1, "secure_text_field", "Password")]);
    let mut engine = Engine::new(sim, policy, Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::TypeText {
            text: "s3cret".into(),
            target: Some(Target::Semantic(SemanticTarget {
                role: Some("secure_text_field".into()),
                name: Some("Password".into()),
                ..Default::default()
            })),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::NeedsApproval { .. } => {}
        other => panic!("secrets floor must gate the route, got {other:?}"),
    }
}

#[test]
fn invoke_and_drag_journal_target_bounds() {
    // The overlay's presence contract covers every element-bound act —
    // Invoke and Drag carry the same target_bounds click does.
    let mut btn = el(1, "button", "Guardar");
    btn.bounds = Some(Rect {
        x: 10.0,
        y: 20.0,
        w: 80.0,
        h: 24.0,
    });
    let mut drop_zone = el(2, "group", "Destino");
    drop_zone.bounds = Some(Rect {
        x: 200.0,
        y: 200.0,
        w: 100.0,
        h: 100.0,
    });
    let sim = SimDriver::new(vec![btn, drop_zone]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let target = |name: &str| {
        Target::Semantic(SemanticTarget {
            name: Some(name.into()),
            ..Default::default()
        })
    };
    for action in [
        Action::Invoke {
            target: target("Guardar"),
            action: "press".into(),
        },
        Action::Drag {
            from: target("Guardar"),
            to: target("Destino"),
            duration_ms: 0,
        },
    ] {
        let step = Step {
            note: None,
            action,
            expect: None,
            max_attempts: Some(1),
            app: None,
        };
        engine.run_step(&step, &cfg());
    }
    let events = engine.events();
    let proposed: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::ActionProposed)
        .collect();
    assert_eq!(proposed.len(), 2, "both acts must be proposed");
    for ev in proposed {
        assert!(
            ev.data["target_bounds"].is_object(),
            "ActionProposed must carry target_bounds for overlay presence: {}",
            ev.data
        );
    }
}

#[test]
fn training_journal_scrubs_action_payloads() {
    // The Training journal keeps replayable structure — but typed/set
    // values appear only as digest tokens, never plaintext.
    use dexter_decision::{HeuristicGenerator, RuleBased};
    use dexter_engine::{TaskConfig, TraceMode};

    let sim = SimDriver::new(vec![el(1, "text_field", "Cuenta")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    engine.set_trace_mode(TraceMode::Training);
    let outcome = engine.run_task(
        "type \"hunter2\" into cuenta",
        &HeuristicGenerator::default(),
        &RuleBased::default(),
        &TaskConfig {
            run: cfg(),
            max_steps: 2,
            max_duration: None,
            cancel: None,
            done_when: ExpectedState::ElementValue {
                target: SemanticTarget {
                    name: Some("Cuenta".into()),
                    ..Default::default()
                },
                predicate: ValuePredicate::Contains("hunter2".into()),
            },
        },
    );
    assert!(matches!(
        outcome,
        dexter_engine::TaskOutcome::Completed { .. }
    ));
    // The replay contract holds: the context still deserializes and
    // every action payload rides as a digest token — the goal text
    // itself is user input and stays verbatim, but no `Action` in the
    // journal may carry its payload in plaintext.
    let events = engine.events();
    let ctx_event = events
        .iter()
        .find(|e| e.kind == EventKind::CandidatesGenerated)
        .expect("candidates event");
    let ctx = &ctx_event.data["context"];
    let candidates = ctx["candidates"]
        .as_array()
        .expect("context keeps its shape");
    let typed = candidates
        .iter()
        .map(|c| &c["action"])
        .find(|a| a["type"] == "type_text" || a["text"].is_string())
        .expect("a TypeText candidate exists");
    let payload = typed["text"].as_str().unwrap_or_default();
    assert!(
        payload.starts_with("[redacted len=7 sha256="),
        "candidate payload must be a digest token, got {payload:?}"
    );
    let decision_event = events
        .iter()
        .find(|e| e.kind == EventKind::DecisionMade)
        .expect("decision event");
    let decision = serde_json::to_string(&decision_event.data["decision"]).unwrap();
    assert!(
        !decision.contains("\"hunter2\""),
        "decision action must not carry the plaintext payload"
    );
}

/// A driver that ignores `scope.window` — models the macOS degraded
/// fallback where a scoped walk silently returns the whole app.
struct UnscopedSim(SimDriver);

impl ComputerDriver for UnscopedSim {
    fn capabilities(&self) -> DriverCapabilities {
        self.0.capabilities()
    }
    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        self.0.windows()
    }
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        self.0.observe(&ObservationScope {
            window: None,
            ..scope.clone()
        })
    }
    fn act(&self, action: &Action, ctx: &ActContext) -> Result<ActionResult, DriverError> {
        self.0.act(action, ctx)
    }
    fn plan(&self, action: &Action, ctx: &ActContext) -> Result<ExecutionPlan, DriverError> {
        self.0.plan(action, ctx)
    }
}

#[test]
fn window_scope_filters_degraded_unscoped_observations() {
    // The engine applies the pinned window even when the driver's
    // scoped walk degrades to app-wide — an element outside the pin
    // must never verify as existing inside it.
    let mut a = el(1, "button", "En ventana");
    a.bounds = Some(Rect {
        x: 10.0,
        y: 10.0,
        w: 60.0,
        h: 24.0,
    });
    let mut b = el(2, "static_text", "Otra ventana");
    b.bounds = Some(Rect {
        x: 1010.0,
        y: 10.0,
        w: 60.0,
        h: 24.0,
    });
    let sim = SimDriver::new(vec![a, b]);
    sim.add_window(Window {
        id: 2,
        pid: 1,
        app: "sim".into(),
        title: Some("Otra".into()),
        bounds: Rect {
            x: 1000.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        },
        on_screen: true,
        layer: 0,
    });
    let mut engine = Engine::new(UnscopedSim(sim), allow_all(), Duration::from_secs(60));
    let mut c = cfg();
    c.window_scope = Some(1);
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                name: Some("En ventana".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
            count: 1,
        },
        // "Otra ventana" exists in the world but not in the pinned
        // window — an honest verifier can't see it.
        expect: Some(ExpectedState::ElementExists {
            target: SemanticTarget {
                name: Some("Otra ventana".into()),
                ..Default::default()
            },
        }),
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &c) {
        StepStatus::Failed { .. } => {}
        other => panic!("out-of-scope element must not verify, got {other:?}"),
    }
}

#[test]
fn focused_secure_field_gets_secrets_floor() {
    // Focus-bound routes (`type_text` with no target, `key`) resolve
    // the focused element — a focused password field gets the secrets
    // floor on every driver, under `mutating = "allow"`.
    let toml = "[defaults]\nmutating = \"allow\"";
    let mut pwd = el(1, "secure_text_field", "Password");
    pwd.focused = true;
    let sim = SimDriver::new(vec![pwd]);
    let mut engine = Engine::new(
        sim,
        Policy::from_toml(toml).unwrap(),
        Duration::from_secs(60),
    );
    let step = |action| Step {
        note: None,
        action,
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(
        &step(Action::TypeText {
            text: "x".into(),
            target: None,
        }),
        &cfg(),
    ) {
        StepStatus::NeedsApproval { .. } => {}
        other => panic!("type_text into a focused secure field must gate, got {other:?}"),
    }
    match engine.run_step(
        &step(Action::Key {
            chord: KeyChord::parse("a").unwrap(),
        }),
        &cfg(),
    ) {
        StepStatus::NeedsApproval { .. } => {}
        other => panic!("key into a focused secure field must gate, got {other:?}"),
    }

    // Contrast: the same acts pass while a normal field has focus.
    let mut plain = el(1, "text_field", "Search");
    plain.focused = true;
    let sim2 = SimDriver::new(vec![plain]);
    let mut engine2 = Engine::new(
        sim2,
        Policy::from_toml(toml).unwrap(),
        Duration::from_secs(60),
    );
    if let StepStatus::NeedsApproval { .. } = engine2.run_step(
        &step(Action::TypeText {
            text: "x".into(),
            target: None,
        }),
        &cfg(),
    ) {
        panic!("a non-sensitive focused field must not trip the floor");
    }
}

#[test]
fn focused_target_enrichment_binds_identity() {
    // `rule.target` matchers and the grant fingerprint see the focused
    // element's identity — an approval for a chord while "Search" has
    // focus never covers the same chord on "Password".
    let toml = r#"
[defaults]
mutating = "allow"
[[rule]]
action = "key"
decision = "require_approval"
"#;
    let fingerprint_for = |name: &str| {
        let mut f = el(1, "text_field", name);
        f.focused = true;
        let sim = SimDriver::new(vec![f]);
        let mut engine = Engine::new(
            sim,
            Policy::from_toml(toml).unwrap(),
            Duration::from_secs(60),
        );
        let step = Step {
            note: None,
            action: Action::Key {
                chord: KeyChord::parse("return").unwrap(),
            },
            expect: None,
            max_attempts: Some(1),
            app: None,
        };
        match engine.run_step(&step, &cfg()) {
            StepStatus::NeedsApproval {
                fingerprint,
                action,
                ..
            } => {
                assert_eq!(action["type"], "key");
                fingerprint
            }
            other => panic!("key rule must gate, got {other:?}"),
        }
    };
    assert_ne!(fingerprint_for("Search"), fingerprint_for("Password"));
}

#[test]
fn foreign_observation_element_token_derives_no_expectation() {
    // The dexter_candidates → dexter_act flow: the element token is
    // bound to the observation that minted it, and `run_step` observes
    // fresh — the token's qualifier is always foreign there. Deriving
    // an expectation against the fresh tree would bind whichever
    // element now sits at that id — or, for a secure field, a value
    // check that can only fail on the redacted value, and a retry
    // would re-type the secret. Honest answer: run unverified.
    let mut pwd = el(1, "secure_text_field", "Password");
    pwd.focused = true;
    let sim = SimDriver::new(vec![pwd]);
    // obs1: the token's minting observation — what `dexter_observe`
    // or `dexter_candidates` would have produced before the act.
    let obs1 = sim.observe(&ObservationScope::default()).unwrap();
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::TypeText {
            text: "hunter2".into(),
            target: Some(Target::Element {
                observation: obs1.id,
                element: ElementId(1),
            }),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { verification, .. } => {
            assert!(
                verification.is_none(),
                "a foreign element token must run unverified, got {verification:?}"
            );
        }
        other => panic!("the act must complete unverified, not fail: {other:?}"),
    }
    let journal: Vec<String> = engine
        .events()
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    assert!(
        !journal
            .iter()
            .any(|l| l.contains("\"verification_failed\"") || l.contains("VerificationFailed")),
        "no verification verdict may fire for a foreign-observation token"
    );
}

#[test]
fn foreign_element_token_on_present_element_runs_unverified() {
    // Same qualifier, no security angle: obs1 and the pre-act obs2
    // both contain element 1 — a bare id lookup would bind it and
    // verify. The qualifier is still foreign (tokens bind to their
    // minting observation), so the honest path is unverified.
    let sim = SimDriver::new(vec![el(1, "text_field", "Nombre")]);
    let obs1 = sim.observe(&ObservationScope::default()).unwrap();
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let step = Step {
        note: None,
        action: Action::SetValue {
            target: Target::Element {
                observation: obs1.id,
                element: ElementId(1),
            },
            value: "x".into(),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    match engine.run_step(&step, &cfg()) {
        StepStatus::Done { verification, .. } => {
            assert!(
                verification.is_none(),
                "a foreign token must not bind by bare id: {verification:?}"
            );
        }
        other => panic!("the act must complete unverified: {other:?}"),
    }
}

#[test]
fn secrets_floor_catches_subrole_and_semantic_targets() {
    // Browser DOM walks mark password inputs as role text_field +
    // subrole password — the role string alone is not sensitive, and a
    // name-only semantic target carries no descriptor at all. The
    // floor must still gate both, on every driver.
    let policy = Policy::from_toml("[defaults]\nmutating = \"allow\"").unwrap();
    let mut pwd = el(1, "text_field", "Password");
    pwd.subrole = Some("password".into());
    let sim = SimDriver::new(vec![pwd]);
    // obs1 mints the element token — the candidates flow.
    let obs1 = sim.observe(&ObservationScope::default()).unwrap();
    let mut engine = Engine::new(sim, policy, Duration::from_secs(60));
    let mk = |target: Target| Step {
        note: None,
        action: Action::TypeText {
            text: "x".into(),
            target: Some(target),
        },
        expect: None,
        max_attempts: Some(1),
        app: None,
    };
    // Element token: the driver's plan-time enrichment carries the
    // subrole into the descriptor, which the floor reads even though
    // the token is foreign to the pre-act observation.
    match engine.run_step(
        &mk(Target::Element {
            observation: obs1.id,
            element: ElementId(1),
        }),
        &cfg(),
    ) {
        StepStatus::NeedsApproval { .. } => {}
        other => panic!("element token on a password-subrole field must gate: {other:?}"),
    }
    // Name-only semantic target: nothing in the descriptor — the
    // floor resolves it against the pre-act observation.
    match engine.run_step(
        &mk(Target::Semantic(SemanticTarget {
            name: Some("Password".into()),
            ..Default::default()
        })),
        &cfg(),
    ) {
        StepStatus::NeedsApproval { .. } => {}
        other => panic!("semantic target on a secure field must gate: {other:?}"),
    }
}
