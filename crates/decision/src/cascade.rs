//! `Cascade` — cheap tiers first, escalate on abstain (`docs/sdd/cascade.md`).

use crate::{
    Decision, DecisionContext, DecisionEngine, DecisionError, EngineHealth, Route, TracedDecision,
};

/// Composite engine: asks each tier in order, escalating only when a
/// tier abstains (or asks for a larger model). Errors propagate — a
/// broken tier fails the decision, it is never skipped.
pub struct Cascade {
    tiers: Vec<Box<dyn DecisionEngine>>,
}

impl Cascade {
    pub fn new(tiers: Vec<Box<dyn DecisionEngine>>) -> Self {
        Self { tiers }
    }
}

fn escalates(d: &Decision) -> bool {
    matches!(
        d,
        Decision::Route {
            route: Route::Abstain | Route::EscalateLlm,
            ..
        }
    )
}

/// An act must be one of the generated candidates — anything else
/// decays to `Abstain`.
fn vet(engine: &str, d: Decision, ctx: &DecisionContext) -> Decision {
    if let Decision::Act {
        action,
        candidate_index,
        ..
    } = &d
    {
        let offered = candidate_index
            .and_then(|i| ctx.candidates.get(i))
            .is_some_and(|c| c.action == *action);
        if !offered {
            return Decision::Route {
                route: Route::Abstain,
                rationale: format!("{engine} proposed an act outside the candidate set"),
            };
        }
    }
    d
}

impl DecisionEngine for Cascade {
    fn name(&self) -> &str {
        "cascade"
    }

    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        self.decide_traced(ctx).map(|t| t.decision)
    }

    fn decide_traced(&self, ctx: &DecisionContext) -> Result<TracedDecision, DecisionError> {
        let mut hops = Vec::new();
        let mut last = None;
        for tier in &self.tiers {
            let traced = tier.decide_traced(ctx)?;
            hops.extend(traced.hops);
            let decision = vet(tier.name(), traced.decision, ctx);
            let done = !escalates(&decision);
            last = Some(decision);
            if done {
                break;
            }
        }
        match last {
            Some(decision) => Ok(TracedDecision { decision, hops }),
            None => Err(DecisionError::Engine {
                engine: self.name().to_string(),
                message: "cascade has no tiers".into(),
            }),
        }
    }

    fn health(&self) -> EngineHealth {
        if self.tiers.is_empty() {
            return EngineHealth::Down("cascade has no tiers".into());
        }
        let impaired: Vec<String> = self
            .tiers
            .iter()
            .filter_map(|t| match t.health() {
                EngineHealth::Ready => None,
                EngineHealth::Degraded(d) | EngineHealth::Down(d) => {
                    Some(format!("{}: {d}", t.name()))
                }
            })
            .collect();
        let all_down = self
            .tiers
            .iter()
            .all(|t| matches!(t.health(), EngineHealth::Down(_)));
        match (impaired.is_empty(), all_down) {
            (true, _) => EngineHealth::Ready,
            (false, true) => EngineHealth::Down(impaired.join("; ")),
            (false, false) => EngineHealth::Degraded(impaired.join("; ")),
        }
    }
}
