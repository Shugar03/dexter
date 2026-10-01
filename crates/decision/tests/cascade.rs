//! Cascade composition tests: stub engines verify escalation order,
//! which verdicts hand off (Abstain/EscalateLlm/error) and which are
//! final, the journal-visible trail, and aggregate health.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use dexter_core::Action;
use dexter_decision::{
    CandidateAction, Cascade, Decision, DecisionContext, DecisionEngine, DecisionError,
    EngineHealth, Route,
};

struct Stub {
    name: &'static str,
    answer: Result<Decision, String>,
    calls: Arc<AtomicUsize>,
    health: EngineHealth,
}

impl Stub {
    fn decision(name: &'static str, decision: Decision) -> Self {
        Self {
            name,
            answer: Ok(decision),
            calls: Arc::new(AtomicUsize::new(0)),
            health: EngineHealth::Ready,
        }
    }

    fn broken(name: &'static str, message: &str) -> Self {
        Self {
            name,
            answer: Err(message.to_string()),
            calls: Arc::new(AtomicUsize::new(0)),
            health: EngineHealth::Ready,
        }
    }

    fn with_health(mut self, health: EngineHealth) -> Self {
        self.health = health;
        self
    }
}

impl DecisionEngine for Stub {
    fn name(&self) -> &str {
        self.name
    }

    fn decide(&self, _ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        match &self.answer {
            Ok(d) => Ok(d.clone()),
            Err(m) => Err(DecisionError::Engine {
                engine: self.name.to_string(),
                message: m.clone(),
            }),
        }
    }

    fn health(&self) -> EngineHealth {
        self.health.clone()
    }
}

fn act(rationale: &str) -> Decision {
    Decision::Act {
        action: Action::Wait { millis: 1 },
        candidate_index: Some(0),
        rationale: rationale.to_string(),
    }
}

fn route(route: Route, rationale: &str) -> Decision {
    Decision::Route {
        route,
        rationale: rationale.to_string(),
    }
}

fn ctx() -> DecisionContext {
    DecisionContext {
        goal: "pay".into(),
        state_digest: "state".into(),
        candidates: vec![CandidateAction {
            action: Action::Wait { millis: 1 },
            rationale: "candidate".into(),
            prior: 0.9,
        }],
        last_error: None,
        step: 1,
    }
}

#[test]
fn first_tier_act_is_final() {
    let first = Stub::decision("rules", act("rule matched"));
    let second = Stub::decision("llm", act("llm picked"));
    let second_calls = second.calls.clone();
    let cascade = Cascade::new(vec![Box::new(first), Box::new(second)]);

    let Decision::Act { rationale, .. } = cascade.decide(&ctx()).unwrap() else {
        panic!("expected act");
    };
    assert_eq!(rationale, "rule matched");
    assert_eq!(
        second_calls.load(Ordering::SeqCst),
        0,
        "second tier never ran"
    );
}

#[test]
fn abstain_escalates_and_appends_trail() {
    let first = Stub::decision("rules", route(Route::Abstain, "weak prior"));
    let second = Stub::decision("llm", act("llm picked"));
    let cascade = Cascade::new(vec![Box::new(first), Box::new(second)]);

    let Decision::Act { rationale, .. } = cascade.decide(&ctx()).unwrap() else {
        panic!("expected act");
    };
    assert!(
        rationale.contains("rules: Abstain — weak prior"),
        "{rationale}"
    );
    assert!(rationale.ends_with("llm picked"), "{rationale}");
}

#[test]
fn escalate_llm_hands_to_next_tier() {
    let first = Stub::decision("laya", route(Route::EscalateLlm, "needs a model"));
    let second = Stub::decision("llm", act("llm picked"));
    let cascade = Cascade::new(vec![Box::new(first), Box::new(second)]);

    let Decision::Act { rationale, .. } = cascade.decide(&ctx()).unwrap() else {
        panic!("expected act");
    };
    assert!(rationale.contains("laya: EscalateLlm"), "{rationale}");
}

#[test]
fn final_routes_do_not_escalate() {
    for r in [Route::Wait { millis: 5 }, Route::EscalateHuman] {
        let first = Stub::decision("rules", route(r, "hold"));
        let second = Stub::decision("llm", act("llm picked"));
        let second_calls = second.calls.clone();
        let cascade = Cascade::new(vec![Box::new(first), Box::new(second)]);

        let Decision::Route { route, .. } = cascade.decide(&ctx()).unwrap() else {
            panic!("expected route");
        };
        assert_eq!(route, r);
        assert_eq!(second_calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn fully_abstaining_cascade_returns_last_abstain_with_chain() {
    let names = ["rules", "laya", "llm"];
    let cascade = Cascade::new(
        names
            .iter()
            .map(|n| {
                Box::new(Stub::decision(n, route(Route::Abstain, "unsure")))
                    as Box<dyn DecisionEngine>
            })
            .collect(),
    );

    let Decision::Route { route, rationale } = cascade.decide(&ctx()).unwrap() else {
        panic!("expected route");
    };
    assert_eq!(route, Route::Abstain);
    assert!(rationale.contains("rules: Abstain"), "{rationale}");
    assert!(rationale.contains("laya: Abstain"), "{rationale}");
    assert!(rationale.ends_with("unsure"), "{rationale}");
}

#[test]
fn mid_tier_error_escalates_with_trail() {
    let first = Stub::broken("laya", "worker died");
    let second = Stub::decision("llm", act("llm picked"));
    let cascade = Cascade::new(vec![Box::new(first), Box::new(second)]);

    let Decision::Act { rationale, .. } = cascade.decide(&ctx()).unwrap() else {
        panic!("expected act");
    };
    assert!(rationale.contains("laya: error"), "{rationale}");
    assert!(rationale.contains("worker died"), "{rationale}");
}

#[test]
fn last_tier_error_surfaces_with_trail() {
    let first = Stub::decision("rules", route(Route::Abstain, "weak prior"));
    let second = Stub::broken("llm", "http 500");
    let cascade = Cascade::new(vec![Box::new(first), Box::new(second)]);

    let err = cascade.decide(&ctx()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("http 500"), "{msg}");
    assert!(msg.contains("rules: Abstain"), "{msg}");
}

#[test]
fn health_aggregates() {
    let all_ready = Cascade::new(vec![
        Box::new(Stub::decision("a", act("x"))),
        Box::new(Stub::decision("b", act("x"))),
    ]);
    assert_eq!(all_ready.health(), EngineHealth::Ready);

    let one_down = Cascade::new(vec![
        Box::new(Stub::decision("a", act("x"))),
        Box::new(Stub::decision("b", act("x")).with_health(EngineHealth::Down("dead".into()))),
    ]);
    let EngineHealth::Degraded(detail) = one_down.health() else {
        panic!("expected degraded");
    };
    assert!(detail.contains("b down: dead"), "{detail}");

    let all_down = Cascade::new(vec![
        Box::new(Stub::decision("a", act("x")).with_health(EngineHealth::Down("dead".into()))),
        Box::new(Stub::decision("b", act("x")).with_health(EngineHealth::Down("dead".into()))),
    ]);
    let EngineHealth::Down(detail) = all_down.health() else {
        panic!("expected down");
    };
    assert!(detail.contains("every tier down"), "{detail}");
}

#[test]
fn cascade_name_lists_tiers() {
    let cascade = Cascade::new(vec![
        Box::new(Stub::decision("rules", act("x"))),
        Box::new(Stub::decision("llm", act("x"))),
    ]);
    assert_eq!(cascade.name(), "cascade(rules,llm)");
}

#[test]
fn empty_cascade_is_an_engine_error() {
    let cascade = Cascade::new(vec![]);
    let err = cascade.decide(&ctx()).unwrap_err();
    assert!(err.to_string().contains("no tiers"), "{err}");
}
