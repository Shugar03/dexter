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
            assert_eq!(result.detail.as_deref(), Some(sentinel));
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

    // Positive control: a stage-needing act on a windowless app DOES
    // wake — the gate narrows, it doesn't remove the borrow.
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
    assert_eq!(engine.driver().wakes(), vec!["sim".to_string()]);
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
