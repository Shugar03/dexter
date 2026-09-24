//! Hermetic end-to-end: sim world + engine loop + policy + verifier.

use dexter_core::*;
use dexter_driver::{ActContext, ComputerDriver, DriverCapabilities, DriverError};
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
    // pre-compute for grants — this is the binding contract. v2: opaque
    // (sha256) — the grant binds the canonical route tuple, not a
    // readable payload.
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
        if self.execute_count.load(Ordering::SeqCst) > 0 && n >= 2 {
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
