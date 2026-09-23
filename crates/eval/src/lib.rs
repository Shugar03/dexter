//! `dexter-eval` — offline decision replay.
//!
//! An `EvalItem` freezes one decision point: the goal, the full
//! observation, and the gold answer (labeled by a teacher — a human,
//! a scenario author, or a sim world with ground truth). Replaying the
//! item against a `CandidateGenerator` + `DecisionEngine` measures:
//!
//! - **coverage** — was the gold element among the generated candidates?
//!   (a generator property: if the right action was never offered, no
//!   engine can pick it)
//! - **accuracy** — did the engine pick the gold candidate, given it was
//!   offered? (an engine property)
//! - **route correctness** — when gold says "don't act" (wait/abstain/
//!   escalate), did the engine route instead of acting?
//!
//! Separating coverage from accuracy is deliberate: a model that looks
//! dumb may be starved by a weak generator.

use dexter_core::{Action, ElementId, Observation, SemanticTarget, Target};
use dexter_decision::{
    CandidateGenerator, Decision, DecisionContext, DecisionEngine, GenHistory, Route,
};
use serde::{Deserialize, Serialize};

/// The labeled correct answer for one decision point.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Gold {
    /// Act on this element — the declarative target plus the resolved
    /// element id (bound at harvest time, stable for the item).
    Act {
        target: SemanticTarget,
        element: ElementId,
    },
    /// The correct answer is a route, not an action.
    Route { route: Route },
    /// Any of these is acceptable (e.g. either of two equivalent buttons).
    AnyOf { options: Vec<Gold> },
}

/// One frozen decision point.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalItem {
    pub id: String,
    /// The goal as given to the task loop, verbatim.
    pub goal: String,
    /// The complete observation — the eval regenerates candidates with
    /// whatever generator is under test, so the item stays valid across
    /// generator versions.
    pub observation: Observation,
    pub gold: Gold,
    /// Provenance: "scenario", "data-url", "sim", "manual".
    pub source: String,
    #[serde(default)]
    pub meta: serde_json::Value,
}

/// A dataset is JSONL — one `EvalItem` per line.
pub fn load_jsonl(text: &str) -> Result<Vec<EvalItem>, serde_json::Error> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect()
}

/// Provenance key for cross-app evaluation: the app (or page) this item
/// was harvested from. Preference order: `meta.app` (harvest manifest),
/// `meta.url` (web page), `observation.app.value` (bundle id/name),
/// else "unknown". Grouping by this key powers leave-one-app-out
/// generalization evals — training on every app *but* the holdout.
pub fn app_key(item: &EvalItem) -> String {
    item.meta
        .get("app")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| item.meta.get("url").and_then(|v| v.as_str()))
        .or_else(|| {
            item.observation.app.as_ref().map(|a| match a {
                dexter_core::AppSelector::Name(n) => n.as_str(),
                dexter_core::AppSelector::BundleId(b) => b.as_str(),
                dexter_core::AppSelector::Pid(p) => {
                    // Pid has no stable label — fall through to unknown.
                    let _ = p;
                    ""
                }
            })
        })
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown")
        .to_string()
}

/// Group items by `app_key`, preserving first-seen order — one row of a
/// cross-app matrix per group.
pub fn split_by_app(items: &[EvalItem]) -> Vec<(String, Vec<EvalItem>)> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: std::collections::HashMap<String, Vec<EvalItem>> =
        std::collections::HashMap::new();
    for item in items {
        let key = app_key(item);
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(item.clone());
    }
    order
        .into_iter()
        .map(|k| {
            let v = groups.remove(&k).unwrap_or_default();
            (k, v)
        })
        .collect()
}

/// Per-item verdict after replay.
#[derive(Debug)]
pub struct ItemVerdict {
    pub item_id: String,
    /// Gold element was among the generated candidates.
    pub covered: bool,
    /// Engine picked the gold (only meaningful when covered).
    pub correct: Option<bool>,
    /// What the engine decided, for the report.
    pub decision: Decision,
    /// Why coverage failed / what the engine picked instead.
    pub note: String,
}

/// Aggregate metrics for one (generator, engine) pair over a dataset.
#[derive(Debug, Default)]
pub struct EvalReport {
    pub items: usize,
    /// Items whose gold element appeared in the candidate set.
    pub covered: usize,
    /// Covered items the engine got right.
    pub correct: usize,
    /// Gold-route items where the engine routed correctly.
    pub routes_correct: usize,
    /// Gold-route items total.
    pub route_items: usize,
    /// Engine routed/abstained when gold was an action (over-caution).
    pub false_routes: usize,
    /// Engine acted when gold was a route (over-action — the dangerous
    /// direction: acting when it shouldn't).
    pub false_acts: usize,
    pub verdicts: Vec<ItemVerdict>,
}

impl EvalReport {
    pub fn coverage(&self) -> f64 {
        if self.items == 0 {
            return 0.0;
        }
        self.covered as f64 / self.items as f64
    }

    /// Accuracy over covered items — the engine's real decision quality.
    pub fn accuracy(&self) -> f64 {
        if self.covered == 0 {
            return 0.0;
        }
        self.correct as f64 / self.covered as f64
    }
}

/// Resolve an `Action`'s target to an element id within `obs`, when the
/// target is semantic/element-bound.
fn action_element(action: &Action, obs: &Observation) -> Option<ElementId> {
    let target: &Target = match action {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. } => target,
        Action::TypeText { target, .. } => match target {
            Some(t) => t,
            None => &Target::Focused,
        },
        _ => return None,
    };
    let el = dexter_world_model::resolve_element(obs, target).ok()?;
    Some(el.id)
}

/// Does `action` satisfy this gold on this observation?
fn gold_satisfied(gold: &Gold, action: &Action, obs: &Observation) -> bool {
    match gold {
        Gold::Act { element, .. } => action_element(action, obs) == Some(*element),
        Gold::Route { .. } => false,
        Gold::AnyOf { options } => options.iter().any(|g| gold_satisfied(g, action, obs)),
    }
}

/// Route discriminant — compare decisions by route kind, not parameters.
/// Public: export tooling maps gold routes onto route-option slots.
pub fn route_variant(r: &Route) -> &'static str {
    match r {
        Route::Reobserve => "reobserve",
        Route::Wait { .. } => "wait",
        Route::Retry => "retry",
        Route::Abstain => "abstain",
        Route::EscalateLlm => "escalate_llm",
        Route::EscalateHuman => "escalate_human",
    }
}

/// Is the gold's element among the candidates' targets?
fn gold_covered(gold: &Gold, ctx: &DecisionContext, obs: &Observation) -> bool {
    match gold {
        Gold::Act { element, .. } => ctx
            .candidates
            .iter()
            .any(|c| action_element(&c.action, obs) == Some(*element)),
        Gold::Route { .. } => true, // routes are always "available" to engines
        Gold::AnyOf { options } => options.iter().any(|g| gold_covered(g, ctx, obs)),
    }
}

/// Which candidate index the gold resolves to, if it's an act-gold
/// offered among the candidates. `None` = uncovered (gold isn't in the
/// menu — the honest label for that row is ambiguous, skip it for
/// training).
pub fn gold_candidate_index(
    gold: &Gold,
    ctx: &DecisionContext,
    obs: &Observation,
) -> Option<usize> {
    match gold {
        Gold::Act { element, .. } => ctx
            .candidates
            .iter()
            .position(|c| action_element(&c.action, obs) == Some(*element)),
        Gold::Route { .. } => None,
        Gold::AnyOf { options } => options
            .iter()
            .find_map(|g| gold_candidate_index(g, ctx, obs)),
    }
}

/// Replay one item: regenerate candidates, ask the engine, score.
pub fn replay_item(
    item: &EvalItem,
    generator: &dyn CandidateGenerator,
    engine: &dyn DecisionEngine,
    step: u32,
) -> Result<ItemVerdict, dexter_decision::DecisionError> {
    let obs = &item.observation;
    let candidates = generator.generate(obs, &item.goal, &GenHistory::default());
    let ctx = DecisionContext {
        goal: item.goal.clone(),
        // Same budget the engine applies in production — eval must feed
        // engines the digest they'd actually see.
        state_digest: dexter_world_model::digest_budget(obs, 14_000),
        candidates,
        last_error: None,
        step,
    };

    let covered = gold_covered(&item.gold, &ctx, obs);
    let decision = engine.decide(&ctx)?;

    let (correct, note) = match (&item.gold, &decision) {
        (_, Decision::Act { action, .. }) => {
            let hit = gold_satisfied(&item.gold, action, obs);
            if hit {
                (Some(true), "engine picked the gold element".into())
            } else {
                let desc = action_element(action, obs)
                    .map(|id| format!("element {id}"))
                    .unwrap_or_else(|| "unresolvable action".into());
                (
                    Some(false),
                    format!("engine acted on {desc}, gold was elsewhere"),
                )
            }
        }
        (Gold::Route { route }, Decision::Route { route: chosen, .. }) => {
            // Variant-level match: Wait{500} vs gold Wait{1000} is the
            // *same decision* ("wait") — the duration is a parameter the
            // engine tunes, not a different answer.
            if route_variant(chosen) == route_variant(route) {
                (Some(true), "engine routed correctly".into())
            } else {
                (
                    Some(false),
                    format!("engine routed {chosen:?}, gold was {route:?}"),
                )
            }
        }
        (Gold::Act { .. } | Gold::AnyOf { .. }, Decision::Route { route, .. }) => (
            Some(false),
            format!("engine routed {route:?} over the gold action"),
        ),
    };

    Ok(ItemVerdict {
        item_id: item.id.clone(),
        covered,
        correct,
        decision,
        note,
    })
}

/// Replay a whole dataset.
pub fn run_eval(
    items: &[EvalItem],
    generator: &dyn CandidateGenerator,
    engine: &dyn DecisionEngine,
) -> EvalReport {
    let mut report = EvalReport::default();
    for (i, item) in items.iter().enumerate() {
        report.items += 1;
        let verdict = match replay_item(item, generator, engine, i as u32 + 1) {
            Ok(v) => v,
            Err(e) => ItemVerdict {
                item_id: item.id.clone(),
                covered: false,
                correct: None,
                decision: Decision::Route {
                    route: Route::Abstain,
                    rationale: format!("engine error: {e}"),
                },
                note: format!("engine error: {e}"),
            },
        };
        let is_route_gold = matches!(item.gold, Gold::Route { .. });
        if is_route_gold {
            report.route_items += 1;
        }
        if verdict.covered {
            report.covered += 1;
            if verdict.correct == Some(true) {
                if is_route_gold {
                    report.routes_correct += 1;
                } else {
                    report.correct += 1;
                }
            } else if is_route_gold {
                if matches!(verdict.decision, Decision::Act { .. }) {
                    report.false_acts += 1;
                }
            } else if matches!(verdict.decision, Decision::Route { .. }) {
                report.false_routes += 1;
            }
        }
        report.verdicts.push(verdict);
    }
    report
}
