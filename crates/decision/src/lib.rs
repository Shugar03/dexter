//! Decision seam — who decides *what* to do next, never *how*.
//!
//! Architecture (from the v3 reflex design):
//!
//! ```text
//! Observation ──► CandidateGenerator ──► Vec<CandidateAction>
//!        │                                    │
//!        └──────────► DecisionContext ◄───────┘
//!                            │
//!                     DecisionEngine::decide()
//!                            │
//!               Decision::Act / Decision::Route(...)
//! ```
//!
//! - `CandidateGenerator` reduces thousands of observed elements to a
//!   handful of plausible actions before any model sees them
//!   (rules-based today; learned ranking later).
//! - `DecisionEngine` is the plug point: `RuleBased` ships with the
//!   runtime, `LayaEngine` (sidecar) and LLM agents implement the same
//!   trait. Engines only *propose* — policy still gates every action.
//! - `Route` is richer than act/don't-act: a decision can wait,
//!   re-observe, retry, abstain, or escalate.
//! - `Question`/`Answer` are the typed micro-decision DTOs Q&A-style
//!   engines (Laya) express themselves through; the engine-internal
//!   `decide()` call stays opaque.

use dexter_core::{Action, Element, MouseButton, Observation, SemanticTarget, Target};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What the runtime should do next, beyond executing an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Route {
    /// Nothing sensible to do yet — observe again.
    Reobserve,
    /// Wait for the world to change (animations, loads).
    Wait { millis: u64 },
    /// Repeat the last action (e.g. transient failure).
    Retry,
    /// No candidate fits the goal — stop without acting.
    Abstain,
    /// Hand the decision to a larger model.
    EscalateLlm,
    /// Hand the decision to a human.
    EscalateHuman,
}

/// A proposed action with provenance for audit and ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateAction {
    pub action: Action,
    /// Why the generator proposed it (journal-visible).
    pub rationale: String,
    /// Generator prior in [0,1] — *not* a confidence the policy trusts.
    pub prior: f32,
}

/// Everything a decision engine needs for one step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionContext {
    /// The user's goal, verbatim.
    pub goal: String,
    /// World-model text digest of the latest observation.
    pub state_digest: String,
    /// Generated candidates, highest prior first.
    pub candidates: Vec<CandidateAction>,
    /// Last step's error/failure summary, if this is a retry.
    pub last_error: Option<String>,
    /// 1-based step counter within the task.
    pub step: u32,
}

/// The engine's verdict for one step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Decision {
    /// Execute this action — index into `ctx.candidates` when it came
    /// from the generator (`None` = engine invented it).
    Act {
        action: Action,
        candidate_index: Option<usize>,
        /// Journal-visible explanation (never trusted by policy).
        rationale: String,
    },
    /// Don't act this step — take a route instead.
    Route { route: Route, rationale: String },
}

#[derive(Debug, Error)]
pub enum DecisionError {
    #[error("decision engine '{engine}' failed: {message}")]
    Engine { engine: String, message: String },
    #[error("decision engine '{engine}' timed out after {millis}ms")]
    Timeout { engine: String, millis: u64 },
}

/// The plug point for Laya, LLMs and rule engines.
pub trait DecisionEngine: Send + Sync {
    fn name(&self) -> &str;
    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError>;
}

/// Typed micro-decision DTO for Q&A-style engines (Laya's actual API
/// shape). A `DecisionEngine` may internally render a `DecisionContext`
/// into `Question`s, get `Answer`s from a model, and interpret them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Pick exactly one option by index.
    Choice {
        id: String,
        prompt: String,
        options: Vec<String>,
    },
    /// Numeric score in [0,1].
    Score { id: String, prompt: String },
    /// Yes/no gate.
    Bool { id: String, prompt: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice { id: String, index: usize },
    Score { id: String, value: f32 },
    Bool { id: String, value: bool },
}

/// Reduces an observation to plausible next actions for a goal.
pub trait CandidateGenerator: Send + Sync {
    fn generate(&self, obs: &Observation, goal: &str) -> Vec<CandidateAction>;
}

/// Default generator: elements advertising press/focus actions whose
/// labels relate to goal keywords, ranked by name-match strength.
/// Deterministic, no model — that's the point of the seam.
pub struct HeuristicGenerator {
    /// Max candidates emitted per observation.
    pub max_candidates: usize,
}

impl Default for HeuristicGenerator {
    fn default() -> Self {
        Self { max_candidates: 8 }
    }
}

fn goal_keywords(goal: &str) -> Vec<String> {
    goal.split(|c: char| !c.is_alphanumeric())
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() >= 3)
        .collect()
}

/// Score one element against the goal. Higher is better; 0 = unrelated.
/// Uses the element's *advertised actions* (press/set_value/focus), not
/// role guessing.
fn score(el: &Element, keywords: &[String]) -> f32 {
    if el.enabled == Some(false) {
        return 0.0;
    }
    let can_press = el.actions.iter().any(|a| a == "press" || a == "show_menu");
    let can_edit = el.actions.iter().any(|a| a == "set_value" || a == "focus");
    if !can_press && !can_edit {
        return 0.0;
    }
    let label = el.label().unwrap_or("").to_lowercase();
    let mut s: f32 = if can_press { 0.2 } else { 0.15 };
    for kw in keywords {
        if label == *kw {
            s += 1.0;
        } else if !label.is_empty() && label.contains(kw) {
            s += 0.5;
        }
    }
    s.min(1.0)
}

impl CandidateGenerator for HeuristicGenerator {
    fn generate(&self, obs: &Observation, goal: &str) -> Vec<CandidateAction> {
        let keywords = goal_keywords(goal);
        let mut scored: Vec<(f32, &Element)> = obs
            .elements
            .iter()
            .map(|el| (score(el, &keywords), el))
            // Base affordance alone (0.2) is not a candidate — a goal
            // keyword must relate the element to the task, else we'd
            // propose every button on screen.
            .filter(|(s, _)| *s > 0.25)
            .collect();
        scored.sort_by(|a, b| b.0.total_cmp(&a.0));
        scored
            .into_iter()
            .take(self.max_candidates)
            .map(|(prior, el)| {
                let st = SemanticTarget {
                    role: el.role.clone(),
                    name: el.name.clone(),
                    ..Default::default()
                };
                let target = Target::Semantic(st);
                let action = if el.actions.iter().any(|a| a == "press") {
                    Action::Click {
                        target,
                        button: MouseButton::Left,
                    }
                } else {
                    Action::Focus { target }
                };
                CandidateAction {
                    action,
                    rationale: format!(
                        "{} \"{}\" advertises [{}], matches goal (prior {prior:.2})",
                        el.role.as_deref().unwrap_or("?"),
                        el.label().unwrap_or("?"),
                        el.actions.join(","),
                    ),
                    prior,
                }
            })
            .collect()
    }
}

/// Deterministic baseline: execute the top candidate, escalate when
/// nothing plausible exists, retry once after an error. Ships with the
/// runtime so `run_task` works with zero model dependencies.
pub struct RuleBased {
    /// Escalate after this many consecutive steps with no candidates.
    pub max_empty_steps: u32,
}

impl Default for RuleBased {
    fn default() -> Self {
        Self { max_empty_steps: 3 }
    }
}

impl DecisionEngine for RuleBased {
    fn name(&self) -> &str {
        "rule-based"
    }

    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        if let Some(first) = ctx.candidates.first() {
            return Ok(Decision::Act {
                action: first.action.clone(),
                candidate_index: Some(0),
                rationale: format!("top candidate: {}", first.rationale),
            });
        }
        if ctx.last_error.is_some() && ctx.step > 1 {
            return Ok(Decision::Route {
                route: Route::Retry,
                rationale: "no candidates; retrying after error".into(),
            });
        }
        Ok(Decision::Route {
            route: Route::EscalateLlm,
            rationale: "no candidate matches the goal — beyond rule-based scope".into(),
        })
    }
}
