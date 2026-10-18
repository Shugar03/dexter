use dexter_core::{Element, ElementId, ElementSource, Observation, SemanticTarget, Target};
use dexter_decision::{
    CandidateGenerator, Decision, DecisionContext, DecisionEngine, DecisionError,
    HeuristicGenerator, LastAct, Route, WorldDelta,
};
use dexter_eval::{load_jsonl, questions_dataset, run_eval, EvalItem, Gold, QuestionStats};

fn el(id: u64, role: &str, name: &str, parent: Option<u64>) -> Element {
    Element {
        id: ElementId(id),
        parent: parent.map(ElementId),
        role: Some(role.into()),
        name: Some(name.into()),
        actions: (role == "button")
            .then(|| "press".into())
            .into_iter()
            .collect(),
        enabled: Some(true),
        source: ElementSource::Dom,
        ..Default::default()
    }
}

fn observation(elements: Vec<Element>) -> Observation {
    let mut observation = Observation {
        elements,
        ..Default::default()
    };
    observation.digest = dexter_world_model::digest(&observation, 250);
    observation
}

fn gold_act(element: u64, role: &str, name: &str) -> Gold {
    Gold::Act {
        target: SemanticTarget {
            role: Some(role.into()),
            name: Some(name.into()),
            ..Default::default()
        },
        element: ElementId(element),
    }
}

fn item(
    id: &str,
    goal: &str,
    observation: Observation,
    gold: Gold,
    last_act: Option<LastAct>,
    expect_questions: Option<Vec<&str>>,
) -> EvalItem {
    EvalItem {
        id: id.into(),
        goal: goal.into(),
        observation,
        gold,
        source: "test".into(),
        meta: serde_json::json!({"note": "question stats fixture"}),
        last_act,
        expect_questions: expect_questions.map(|ids| ids.into_iter().map(str::to_string).collect()),
    }
}

fn modal_item() -> EvalItem {
    let obs = observation(vec![
        el(1, "window", "Editor", None),
        el(2, "button", "Guardar", Some(1)),
        el(3, "dialog", "Actualización disponible", Some(1)),
        el(4, "button", "Más tarde", Some(3)),
    ]);
    item(
        "modal",
        "guardar el documento",
        obs,
        Gold::Route {
            route: Route::Abstain,
        },
        None,
        Some(vec!["blocking_modal"]),
    )
}

fn twin_item() -> EvalItem {
    let obs = observation(vec![
        el(1, "window", "Tabla", None),
        el(2, "static_text", "Juan", Some(1)),
        el(3, "button", "Eliminar", Some(1)),
        el(4, "static_text", "Marta", Some(1)),
        el(5, "button", "Eliminar", Some(1)),
    ]);
    item(
        "twins",
        "eliminar la fila de Marta",
        obs,
        gold_act(5, "button", "Eliminar"),
        None,
        Some(vec!["disambiguate"]),
    )
}

fn submit_item(
    id: &str,
    expect_questions: Option<Vec<&str>>,
    delta: WorldDelta,
    gold: Gold,
) -> EvalItem {
    let obs = observation(vec![
        el(1, "window", "Editor", None),
        el(2, "button", "Enviar", Some(1)),
    ]);
    let candidate =
        HeuristicGenerator::default().generate(&obs, "enviar el mensaje", &Default::default());
    let action = candidate
        .first()
        .expect("Enviar is a generated candidate")
        .action
        .clone();
    item(
        id,
        "enviar el mensaje",
        obs,
        gold,
        Some(LastAct { action, delta }),
        expect_questions,
    )
}

fn complete_item() -> EvalItem {
    let obs = observation(vec![
        el(1, "window", "Editor", None),
        el(2, "button", "Enviar otro mensaje", Some(1)),
    ]);
    item(
        "goal-complete",
        "enviar el mensaje",
        obs,
        Gold::AnyOf {
            options: vec![gold_act(2, "button", "Enviar otro mensaje")],
        },
        Some(LastAct {
            action: dexter_core::Action::Click {
                target: Target::Semantic(SemanticTarget {
                    role: Some("button".into()),
                    name: Some("Enviar".into()),
                    ..Default::default()
                }),
                button: dexter_core::MouseButton::Left,
            },
            delta: WorldDelta::Changed,
        }),
        Some(vec!["goal_complete"]),
    )
}

fn question_items() -> Vec<EvalItem> {
    vec![
        modal_item(),
        twin_item(),
        submit_item(
            "effect-expected",
            Some(vec!["action_effect"]),
            WorldDelta::Unchanged,
            Gold::Route {
                route: Route::Abstain,
            },
        ),
        complete_item(),
        submit_item(
            "effect-spurious",
            Some(vec![]),
            WorldDelta::Unknown,
            Gold::Route {
                route: Route::Abstain,
            },
        ),
        submit_item(
            "effect-unlabeled",
            None,
            WorldDelta::Unchanged,
            gold_act(2, "button", "Enviar"),
        ),
    ]
}

struct AlwaysAbstain;

impl DecisionEngine for AlwaysAbstain {
    fn name(&self) -> &str {
        "always-abstain"
    }

    fn decide(&self, _ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        Ok(Decision::Route {
            route: Route::Abstain,
            rationale: "test".into(),
        })
    }
}

struct AlwaysAct;

impl DecisionEngine for AlwaysAct {
    fn name(&self) -> &str {
        "always-act"
    }

    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        let Some(candidate) = ctx.candidates.first() else {
            return Ok(Decision::Route {
                route: Route::Abstain,
                rationale: "no candidate".into(),
            });
        };
        Ok(Decision::Act {
            action: candidate.action.clone(),
            candidate_index: Some(0),
            rationale: "test".into(),
        })
    }
}

fn stats(
    labeled: usize,
    detected: usize,
    spurious: usize,
    asked: usize,
    correct: usize,
    false_acts: usize,
    false_routes: usize,
) -> QuestionStats {
    QuestionStats {
        labeled,
        detected,
        spurious,
        asked,
        correct,
        false_acts,
        false_routes,
    }
}

#[test]
fn question_stat_rates_use_zero_for_empty_denominators() {
    let empty = QuestionStats::default();
    assert_eq!(empty.detector_recall(), 0.0);
    assert_eq!(empty.accuracy(), 0.0);

    let measured = QuestionStats {
        labeled: 4,
        detected: 1,
        asked: 4,
        correct: 3,
        ..Default::default()
    };
    assert!((measured.detector_recall() - 0.25).abs() < f64::EPSILON);
    assert!((measured.accuracy() - 0.75).abs() < f64::EPSILON);
}

#[test]
fn per_question_stats_match_expected_arithmetic_for_both_engines() {
    let items = question_items();
    let generator = HeuristicGenerator::default();
    let abstain = run_eval(&items, &generator, &AlwaysAbstain);
    assert_eq!(abstain.questions.len(), 4);
    assert_eq!(
        abstain.questions["blocking_modal"],
        stats(1, 1, 0, 1, 1, 0, 0)
    );
    assert_eq!(
        abstain.questions["disambiguate"],
        stats(1, 1, 0, 1, 0, 0, 1)
    );
    assert_eq!(
        abstain.questions["action_effect"],
        stats(1, 1, 1, 3, 2, 0, 1)
    );
    assert_eq!(
        abstain.questions["goal_complete"],
        stats(1, 1, 0, 1, 0, 0, 1)
    );

    let act = run_eval(&items, &generator, &AlwaysAct);
    assert_eq!(act.questions["blocking_modal"], stats(1, 1, 0, 1, 0, 1, 0));
    assert_eq!(act.questions["disambiguate"], stats(1, 1, 0, 1, 0, 0, 0));
    assert_eq!(act.questions["action_effect"], stats(1, 1, 1, 3, 1, 2, 0));
    assert_eq!(act.questions["goal_complete"], stats(1, 1, 0, 1, 1, 0, 0));
}

#[test]
fn unlabeled_items_do_not_change_detector_counts() {
    let item = submit_item(
        "unlabeled",
        None,
        WorldDelta::Unknown,
        Gold::Route {
            route: Route::Abstain,
        },
    );
    let report = run_eval(&[item], &HeuristicGenerator::default(), &AlwaysAbstain);
    let action_effect = &report.questions["action_effect"];
    assert_eq!(action_effect.labeled, 0);
    assert_eq!(action_effect.detected, 0);
    assert_eq!(action_effect.spurious, 0);
    assert_eq!(action_effect.asked, 1);
    for id in ["blocking_modal", "disambiguate", "goal_complete"] {
        assert_eq!(report.questions[id].labeled, 0);
        assert_eq!(report.questions[id].detected, 0);
        assert_eq!(report.questions[id].spurious, 0);
    }
}

#[test]
fn old_items_without_question_fields_still_deserialize() {
    let old_line = include_str!("../../../datasets/sim/items.jsonl")
        .lines()
        .next()
        .expect("sim dataset has a row");
    let item: EvalItem = serde_json::from_str(old_line).unwrap();
    assert!(item.last_act.is_none());
    assert!(item.expect_questions.is_none());
}

#[test]
fn question_items_load_and_unchanged_or_unknown_last_actions_are_candidates() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../datasets/questions/items.jsonl");
    let text = std::fs::read_to_string(path).expect("question dataset exists");
    let items = load_jsonl(&text).unwrap();
    assert_eq!(items.len(), 11);
    let generator = HeuristicGenerator::default();
    for item in &items {
        let Some(last_act) = &item.last_act else {
            continue;
        };
        if matches!(last_act.delta, WorldDelta::Unchanged | WorldDelta::Unknown) {
            let candidates = generator.generate(&item.observation, &item.goal, &Default::default());
            assert!(
                candidates
                    .iter()
                    .any(|candidate| candidate.action == last_act.action),
                "{} last action does not match a generated candidate",
                item.id
            );
        }
    }
}

#[test]
fn question_dataset_regeneration_is_byte_identical() {
    let generated = dexter_eval::to_jsonl(&questions_dataset::generate_questions()).unwrap();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../datasets/questions/items.jsonl");
    assert_eq!(generated, std::fs::read_to_string(path).unwrap());
}
