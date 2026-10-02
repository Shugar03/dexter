//! Decision cascade — cheap tiers first, escalate on abstain, never an
//! invented act. Hermetic fakes only.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dexter_core::{Action, MouseButton, SemanticTarget, Target};
use dexter_decision::{
    CandidateAction, Cascade, Decision, DecisionContext, DecisionEngine, DecisionError,
    EngineHealth, Route,
};

fn click(name: &str) -> Action {
    Action::Click {
        target: Target::Semantic(SemanticTarget {
            role: Some("button".into()),
            name: Some(name.into()),
            ..Default::default()
        }),
        button: MouseButton::Left,
    }
}

fn ctx() -> DecisionContext {
    DecisionContext {
        goal: "guardar".into(),
        state_digest: String::new(),
        candidates: vec![
            CandidateAction {
                action: click("Guardar"),
                rationale: "label match".into(),
                prior: 0.4,
                behind_modal: None,
            },
            CandidateAction {
                action: click("Cancelar"),
                rationale: "weak".into(),
                prior: 0.1,
                behind_modal: None,
            },
        ],
        last_error: None,
        step: 1,
    }
}

/// Fake tier: returns a canned decision (or error) and counts calls.
struct Fake {
    name: &'static str,
    reply: Result<Decision, &'static str>,
    health: EngineHealth,
    calls: Arc<AtomicUsize>,
}

impl Fake {
    fn new(name: &'static str, reply: Result<Decision, &'static str>) -> Self {
        Self {
            name,
            reply,
            health: EngineHealth::Ready,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }
    fn abstain(name: &'static str) -> Self {
        Self::new(name, Ok(route(Route::Abstain)))
    }
    fn acts(name: &'static str, index: usize) -> Self {
        Self::new(
            name,
            Ok(Decision::Act {
                action: ctx().candidates[index].action.clone(),
                candidate_index: Some(index),
                rationale: format!("{name} picked {index}"),
            }),
        )
    }
    fn with_health(mut self, h: EngineHealth) -> Self {
        self.health = h;
        self
    }
}

fn route(route: Route) -> Decision {
    Decision::Route {
        route,
        rationale: "fake".into(),
    }
}

impl DecisionEngine for Fake {
    fn name(&self) -> &str {
        self.name
    }
    fn decide(&self, _ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.reply.clone().map_err(|m| DecisionError::Engine {
            engine: self.name.into(),
            message: m.into(),
        })
    }
    fn health(&self) -> EngineHealth {
        self.health.clone()
    }
}

fn hop_engines(t: &dexter_decision::TracedDecision) -> Vec<&str> {
    t.hops.iter().map(|h| h.engine.as_str()).collect()
}

#[test]
fn first_tier_answer_is_final() {
    let llm = Fake::acts("llm", 1);
    let llm_calls = llm.calls.clone();
    let c = Cascade::new(vec![Box::new(Fake::acts("rules", 0)), Box::new(llm)]);
    assert_eq!(c.name(), "cascade");
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(hop_engines(&t), ["rules"]);
    assert_eq!(t.answered_by(), Some("rules"));
    assert!(matches!(
        t.decision,
        Decision::Act {
            candidate_index: Some(0),
            ..
        }
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn abstain_escalates_until_a_tier_answers() {
    let c = Cascade::new(vec![
        Box::new(Fake::abstain("rules")),
        Box::new(Fake::new("laya", Ok(route(Route::EscalateLlm)))),
        Box::new(Fake::acts("llm", 1)),
    ]);
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(hop_engines(&t), ["rules", "laya", "llm"]);
    assert_eq!(t.answered_by(), Some("llm"));
    match &t.decision {
        Decision::Act {
            action,
            candidate_index,
            ..
        } => {
            assert_eq!(*candidate_index, Some(1));
            assert_eq!(*action, click("Cancelar"));
        }
        other => panic!("expected Act, got {other:?}"),
    }
    // `decide` is the untraced view of the same verdict.
    assert_eq!(c.decide(&ctx()).unwrap(), t.decision);
}

#[test]
fn abstain_at_every_tier_abstains() {
    let c = Cascade::new(vec![
        Box::new(Fake::abstain("rules")),
        Box::new(Fake::abstain("laya")),
        Box::new(Fake::abstain("llm")),
    ]);
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(hop_engines(&t), ["rules", "laya", "llm"]);
    assert!(matches!(
        t.decision,
        Decision::Route {
            route: Route::Abstain,
            ..
        }
    ));
}

#[test]
fn invented_or_mismatched_acts_never_pass() {
    let invented = Fake::new(
        "laya",
        Ok(Decision::Act {
            action: click("Borrar todo"),
            candidate_index: None,
            rationale: "made it up".into(),
        }),
    );
    let out_of_range = Fake::new(
        "llm",
        Ok(Decision::Act {
            action: click("Guardar"),
            candidate_index: Some(7),
            rationale: "bad index".into(),
        }),
    );
    let mismatched = Fake::new(
        "llm2",
        Ok(Decision::Act {
            action: click("Borrar todo"),
            candidate_index: Some(0),
            rationale: "index 0 but another action".into(),
        }),
    );
    let c = Cascade::new(vec![
        Box::new(Fake::abstain("rules")),
        Box::new(invented),
        Box::new(out_of_range),
        Box::new(mismatched),
    ]);
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(hop_engines(&t), ["rules", "laya", "llm", "llm2"]);
    assert!(
        matches!(
            t.decision,
            Decision::Route {
                route: Route::Abstain,
                ..
            }
        ),
        "invented acts must decay to Abstain, got {:?}",
        t.decision
    );
}

#[test]
fn invented_act_escalates_to_a_valid_answer() {
    let c = Cascade::new(vec![
        Box::new(Fake::new(
            "laya",
            Ok(Decision::Act {
                action: click("Borrar todo"),
                candidate_index: None,
                rationale: "made it up".into(),
            }),
        )),
        Box::new(Fake::acts("llm", 0)),
    ]);
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(t.answered_by(), Some("llm"));
    assert!(matches!(
        t.decision,
        Decision::Act {
            candidate_index: Some(0),
            ..
        }
    ));
}

#[test]
fn non_abstain_route_is_final() {
    let llm = Fake::acts("llm", 0);
    let llm_calls = llm.calls.clone();
    let c = Cascade::new(vec![
        Box::new(Fake::new("rules", Ok(route(Route::Wait { millis: 500 })))),
        Box::new(llm),
    ]);
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(hop_engines(&t), ["rules"]);
    assert!(matches!(
        t.decision,
        Decision::Route {
            route: Route::Wait { millis: 500 },
            ..
        }
    ));
    assert_eq!(llm_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn tier_error_propagates_without_falling_through() {
    let llm = Fake::acts("llm", 0);
    let llm_calls = llm.calls.clone();
    let c = Cascade::new(vec![
        Box::new(Fake::abstain("rules")),
        Box::new(Fake::new("laya", Err("worker died"))),
        Box::new(llm),
    ]);
    match c.decide_traced(&ctx()) {
        Err(DecisionError::Engine { engine, message }) => {
            assert_eq!(engine, "laya");
            assert_eq!(message, "worker died");
        }
        other => panic!("expected laya Engine error, got {other:?}"),
    }
    assert_eq!(llm_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn empty_cascade_is_an_engine_error() {
    let c = Cascade::new(vec![]);
    assert!(matches!(
        c.decide(&ctx()),
        Err(DecisionError::Engine { .. })
    ));
    assert!(matches!(c.health(), EngineHealth::Down(_)));
}

#[test]
fn nested_cascades_flatten_hops() {
    let inner = Cascade::new(vec![
        Box::new(Fake::abstain("rules")),
        Box::new(Fake::abstain("laya")),
    ]);
    let c = Cascade::new(vec![Box::new(inner), Box::new(Fake::acts("llm", 0))]);
    let t = c.decide_traced(&ctx()).unwrap();
    assert_eq!(hop_engines(&t), ["rules", "laya", "llm"]);
}

#[test]
fn health_aggregates_tiers() {
    let ready = Cascade::new(vec![
        Box::new(Fake::abstain("a")),
        Box::new(Fake::abstain("b")),
    ]);
    assert_eq!(ready.health(), EngineHealth::Ready);

    let partial = Cascade::new(vec![
        Box::new(Fake::abstain("rules")),
        Box::new(Fake::abstain("llm").with_health(EngineHealth::Down("no key".into()))),
    ]);
    match partial.health() {
        EngineHealth::Degraded(d) => assert!(d.contains("llm"), "{d}"),
        other => panic!("expected Degraded, got {other:?}"),
    }

    let down = Cascade::new(vec![
        Box::new(Fake::abstain("a").with_health(EngineHealth::Down("x".into()))),
        Box::new(Fake::abstain("b").with_health(EngineHealth::Down("y".into()))),
    ]);
    assert!(matches!(down.health(), EngineHealth::Down(_)));
}
