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
    // A forged fingerprint is rejected: only a live escalation can be
    // granted.
    assert!(!engine.approve_pending("forged-fingerprint"));
    // Grant it -> the same step now runs.
    assert!(engine.approve_pending(&fp));
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
fn route_retry_replays_last_action() {
    // `Route::Retry`'s contract is "repeat the last action". A decider
    // that acts once, then routes Retry, then abstains must leave TWO
    // presses on the driver — a bare continue would silently drop the
    // intent (old behavior: pressed().len() == 1).
    use dexter_decision::{
        Decision, DecisionContext, DecisionEngine, DecisionError, HeuristicGenerator, Route,
    };
    use dexter_engine::{TaskConfig, TaskOutcome};

    struct RetryOnce;
    impl DecisionEngine for RetryOnce {
        fn name(&self) -> &str {
            "retry-once"
        }
        fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
            Ok(match ctx.step {
                1 => Decision::Act {
                    action: Action::Click {
                        target: Target::Semantic(SemanticTarget {
                            role: Some("button".into()),
                            name: Some("Guardar".into()),
                            ..Default::default()
                        }),
                        button: MouseButton::Left,
                    },
                    candidate_index: None,
                    rationale: "first press".into(),
                },
                2 => Decision::Route {
                    route: Route::Retry,
                    rationale: "again".into(),
                },
                _ => Decision::Route {
                    route: Route::Abstain,
                    rationale: "done".into(),
                },
            })
        }
    }

    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "click guardar twice",
        &HeuristicGenerator::default(),
        &RetryOnce,
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
    assert!(matches!(outcome, TaskOutcome::Abstained { .. }));
    assert_eq!(
        engine.driver().pressed().len(),
        2,
        "Retry must replay the last action"
    );
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

/// Wraps the sim world and counts `observe` calls — observes are not
/// free (a full AX walk live, a tick advance in sim).
struct CountingDriver {
    inner: SimDriver,
    observes: std::sync::atomic::AtomicUsize,
}

impl dexter_driver::ComputerDriver for CountingDriver {
    fn capabilities(&self) -> dexter_driver::DriverCapabilities {
        self.inner.capabilities()
    }
    fn windows(&self) -> Result<Vec<Window>, dexter_driver::DriverError> {
        self.inner.windows()
    }
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, dexter_driver::DriverError> {
        self.observes
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.observe(scope)
    }
    fn act(
        &self,
        action: &Action,
        ctx: &dexter_driver::ActContext,
    ) -> Result<ActionResult, dexter_driver::DriverError> {
        self.inner.act(action, ctx)
    }
}

fn counting_engine() -> Engine<CountingDriver> {
    let mut btn = el(1, "button", "Guardar");
    btn.bounds = Some(Rect {
        x: 10.0,
        y: 20.0,
        w: 80.0,
        h: 24.0,
    });
    let driver = CountingDriver {
        inner: SimDriver::new(vec![btn]),
        observes: Default::default(),
    };
    Engine::new(driver, allow_all(), Duration::from_secs(60))
}

fn click_guardar() -> Step {
    Step {
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
    }
}

fn proposed_bounds<D: dexter_driver::ComputerDriver>(engine: &Engine<D>) -> serde_json::Value {
    engine
        .events()
        .iter()
        .find(|e| e.kind == EventKind::ActionProposed)
        .expect("ActionProposed")
        .data["target_bounds"]
        .clone()
}

#[test]
fn run_step_without_live_sink_skips_cosmetic_observe() {
    // No presence consumer → no bounds observe: the act is the only
    // driver call for an unverified step.
    let mut engine = counting_engine();
    assert!(engine.run_step(&click_guardar(), &cfg()).done());
    let observes = engine
        .driver()
        .observes
        .load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(observes, 0, "no consumer for target_bounds");
    assert!(proposed_bounds(&engine).is_null());
}

#[test]
fn run_step_with_live_sink_journals_target_bounds() {
    let path = std::env::temp_dir().join(format!(
        "dexter-e2e-sink-{}-{:?}.jsonl",
        std::process::id(),
        std::thread::current().id()
    ));
    let mut engine = counting_engine();
    engine.set_journal_sink(&path).unwrap();
    assert!(engine.run_step(&click_guardar(), &cfg()).done());
    let observes = engine
        .driver()
        .observes
        .load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(observes, 1, "one cosmetic observe for the overlay");
    let b = proposed_bounds(&engine);
    assert_eq!(b["x"], 10.0);
    assert_eq!(b["w"], 80.0);
    let streamed = std::fs::read_to_string(&path).unwrap();
    assert!(streamed.contains("\"target_bounds\":{"), "{streamed}");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn into_driver_hands_back_the_same_driver() {
    // `dexter mcp` reuses the CLI engine's driver instead of building a
    // second one (a second `safaridriver` session, a second AX setup).
    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
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
        expect: None,
        max_attempts: None,
        app: None,
    };
    let _ = engine.run_step(&step, &cfg());
    let sim = engine.into_driver();
    assert_eq!(sim.pressed(), vec![ElementId(1)]);
}

#[test]
fn cascade_hops_are_journaled_on_decision_made() {
    // `--engine cascade`: an abstaining first tier escalates to the
    // next; DecisionMade must record every tier consulted.
    use dexter_decision::{
        Cascade, Decision, DecisionContext, DecisionEngine, DecisionError, HeuristicGenerator,
        Route, RuleBased,
    };
    use dexter_engine::{TaskConfig, TaskOutcome};

    struct Abstains;
    impl DecisionEngine for Abstains {
        fn name(&self) -> &str {
            "abstains"
        }
        fn decide(&self, _: &DecisionContext) -> Result<Decision, DecisionError> {
            Ok(Decision::Route {
                route: Route::Abstain,
                rationale: "not my call".into(),
            })
        }
    }

    let sim = SimDriver::new(vec![el(1, "button", "Guardar")]);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Guardado")),
    );
    let mut engine = Engine::new(sim, allow_all(), Duration::from_secs(60));
    let cascade = Cascade::new(vec![Box::new(Abstains), Box::new(RuleBased::default())]);
    let outcome = engine.run_task(
        "click guardar",
        &HeuristicGenerator::default(),
        &cascade,
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
    assert!(
        matches!(outcome, TaskOutcome::Completed { steps: 1 }),
        "{outcome:?}"
    );
    let events = engine.events();
    let made = events
        .iter()
        .find(|e| e.kind == EventKind::DecisionMade)
        .expect("DecisionMade journaled");
    assert_eq!(made.data["engine"], "cascade");
    let hops: Vec<&str> = made.data["hops"]
        .as_array()
        .expect("hops array")
        .iter()
        .map(|h| h["engine"].as_str().unwrap())
        .collect();
    assert_eq!(hops, ["abstains", "rule-based"]);
    assert_eq!(made.data["hops"][1]["decision"]["type"], "act");
}

/// Dead-end world for the recovery ladder: "Guardar documento" ranks
/// first (covers every goal term) but the app refuses the press;
/// "Guardar" is the next-best candidate and actually saves.
fn dead_end_world(with_alternative: bool) -> SimDriver {
    let mut elements = vec![el(1, "button", "Guardar documento")];
    if with_alternative {
        elements.push(el(2, "button", "Guardar"));
    }
    let sim = SimDriver::new(elements);
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar documento".into()),
            ..Default::default()
        },
        Effect::Fail("la aplicación rechazó la acción".into()),
    );
    sim.on_press(
        SemanticTarget {
            name: Some("Guardar".into()),
            ..Default::default()
        },
        Effect::Spawn(el(0, "static_text", "Documento guardado")),
    );
    sim
}

fn saved_cfg(max_steps: u32) -> dexter_engine::TaskConfig {
    dexter_engine::TaskConfig {
        run: cfg(),
        max_steps,
        max_duration: None,
        cancel: None,
        done_when: ExpectedState::ElementExists {
            target: SemanticTarget {
                role: Some("static_text".into()),
                name: Some("Documento guardado".into()),
                ..Default::default()
            },
        },
    }
}

/// A decider that acts on the top candidate once, then insists on
/// `Route::Retry` forever — the shape that used to spin the engine on a
/// failing action until `max_steps`.
struct ActThenRetry;
impl dexter_decision::DecisionEngine for ActThenRetry {
    fn name(&self) -> &str {
        "act-then-retry"
    }
    fn decide(
        &self,
        ctx: &dexter_decision::DecisionContext,
    ) -> Result<dexter_decision::Decision, dexter_decision::DecisionError> {
        use dexter_decision::{Decision, Route};
        Ok(if ctx.step == 1 {
            let first = ctx.candidates.first().expect("candidates");
            Decision::Act {
                action: first.action.clone(),
                candidate_index: Some(0),
                rationale: "top candidate".into(),
            }
        } else {
            Decision::Route {
                route: Route::Retry,
                rationale: "insist".into(),
            }
        })
    }
}

fn failed_presses(events: &[Event]) -> usize {
    events
        .iter()
        .filter(|e| e.kind == EventKind::ActionExecuted && e.data["status"] == "Failed")
        .count()
}

#[test]
fn recovery_rung3_tries_next_best_candidate() {
    use dexter_decision::HeuristicGenerator;
    use dexter_engine::TaskOutcome;

    let mut engine = Engine::new(dead_end_world(true), allow_all(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "guardar el documento",
        &HeuristicGenerator::default(),
        &ActThenRetry,
        &saved_cfg(6),
    );
    assert!(
        matches!(outcome, TaskOutcome::Completed { .. }),
        "alternative must complete the task, got {outcome:?}"
    );
    let events = engine.events();
    // Rung 1 (one replay) happened, then rung 3 took over — the dead
    // end was tried exactly twice, never a third time.
    assert_eq!(failed_presses(&events), 2, "rung 1 retry, then rung 3");
    let rung3 = events
        .iter()
        .find(|e| e.kind == EventKind::RecoveryStarted && e.data["rung"] == 3)
        .expect("RecoveryStarted rung 3 journaled");
    assert_eq!(rung3.data["failures"], 2);
    assert_eq!(rung3.data["alternative"]["target"]["name"], "Guardar");
    assert_eq!(
        engine.driver().pressed(),
        vec![ElementId(2)],
        "only the alternative was actually pressed"
    );
}

#[test]
fn recovery_ladder_exhausted_escalates() {
    use dexter_decision::{HeuristicGenerator, Route};
    use dexter_engine::TaskOutcome;

    let mut engine = Engine::new(dead_end_world(false), allow_all(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "guardar el documento",
        &HeuristicGenerator::default(),
        &ActThenRetry,
        &saved_cfg(6),
    );
    match outcome {
        TaskOutcome::Escalated { route, reason } => {
            assert_eq!(route, Route::EscalateHuman);
            assert!(reason.contains("Guardar documento"), "{reason}");
        }
        other => panic!("no alternative must escalate, not spin: {other:?}"),
    }
    assert_eq!(failed_presses(&engine.events()), 2);
}

#[test]
fn insisting_act_on_dead_candidate_is_substituted() {
    // A decider that keeps *acting* on the same failing action (not
    // routing Retry) hits the same ladder: twice is enough.
    use dexter_decision::{Decision, DecisionContext, DecisionError, HeuristicGenerator};
    use dexter_engine::TaskOutcome;

    struct InsistDead;
    impl dexter_decision::DecisionEngine for InsistDead {
        fn name(&self) -> &str {
            "insist-dead"
        }
        fn decide(&self, _ctx: &DecisionContext) -> Result<Decision, DecisionError> {
            Ok(Decision::Act {
                action: Action::Click {
                    target: Target::Semantic(SemanticTarget {
                        role: Some("button".into()),
                        name: Some("Guardar documento".into()),
                        ..Default::default()
                    }),
                    button: MouseButton::Left,
                },
                candidate_index: None,
                rationale: "the one I like".into(),
            })
        }
    }

    let mut engine = Engine::new(dead_end_world(true), allow_all(), Duration::from_secs(60));
    let outcome = engine.run_task(
        "guardar el documento",
        &HeuristicGenerator::default(),
        &InsistDead,
        &saved_cfg(6),
    );
    assert!(
        matches!(outcome, TaskOutcome::Completed { .. }),
        "{outcome:?}"
    );
    assert_eq!(failed_presses(&engine.events()), 2);
    assert_eq!(engine.driver().pressed(), vec![ElementId(2)]);
}
