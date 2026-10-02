//! Threshold calibration — sweep one decision knob (the rule-based act
//! threshold, Laya's `min_confidence`) over frozen items and pick the
//! operating point fail-closed. See `docs/sdd/calibration.md`.

use crate::{run_eval, EvalItem, EvalReport};
use dexter_decision::{CandidateGenerator, DecisionEngine, RuleBased};

/// One swept value and the eval it produced.
#[derive(Debug)]
pub struct SweepPoint {
    pub value: f32,
    pub report: EvalReport,
}

impl SweepPoint {
    /// Correct decisions: gold acts picked plus gold routes taken.
    pub fn score(&self) -> usize {
        self.report.correct + self.report.routes_correct
    }
}

/// Replay `items` once per value, with the engine `make` builds for it.
/// A build error aborts the sweep — never a silently missing row.
pub fn sweep<E, F>(
    items: &[EvalItem],
    generator: &dyn CandidateGenerator,
    values: &[f32],
    mut make: F,
) -> Result<Vec<SweepPoint>, E>
where
    F: FnMut(f32) -> Result<Box<dyn DecisionEngine>, E>,
{
    values
        .iter()
        .map(|&value| {
            let engine = make(value)?;
            Ok(SweepPoint {
                value,
                report: run_eval(items, generator, engine.as_ref()),
            })
        })
        .collect()
}

/// `sweep` over `RuleBased { act_threshold }`.
pub fn sweep_act_threshold(
    items: &[EvalItem],
    generator: &dyn CandidateGenerator,
    thresholds: &[f32],
) -> Vec<SweepPoint> {
    let built: Result<_, std::convert::Infallible> = sweep(items, generator, thresholds, |t| {
        Ok(Box::new(RuleBased { act_threshold: t }))
    });
    match built {
        Ok(points) => points,
        Err(never) => match never {},
    }
}

/// Cost of one false act in `utility` — it loses the correct route it
/// displaced *and* acted on the world when gold said don't.
pub const FALSE_ACT_COST: usize = 2;

impl SweepPoint {
    /// `score − FALSE_ACT_COST · false_acts`, saturating at 0.
    pub fn utility(&self) -> usize {
        self.score()
            .saturating_sub(FALSE_ACT_COST * self.report.false_acts)
    }
}

/// Fail-closed operating point: highest `utility` (a false act costs
/// more than a correct answer earns), then fewest false acts, then the
/// highest value (the more conservative gate for both knobs). Pure
/// false-acts-first would pick "abstain on everything" — that is not
/// calibration, it is switching the engine off.
pub fn pick(points: &[SweepPoint]) -> Option<f32> {
    points
        .iter()
        .min_by(|a, b| {
            b.utility()
                .cmp(&a.utility())
                .then(a.report.false_acts.cmp(&b.report.false_acts))
                .then(b.value.total_cmp(&a.value))
        })
        .map(|p| p.value)
}
