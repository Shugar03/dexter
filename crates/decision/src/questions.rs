//! Typed micro-decisions beyond "pick a candidate" — blocking-modal
//! detection and ambiguous-target resolution (docs/sdd/laya-questions.md).
//! Structure is detected here, deterministically; Q&A engines ask the
//! model only for the judgment.

use crate::{CandidateAction, DecisionContext, Question, WorldDelta};
use dexter_core::{Action, Target};

/// `Question::Bool` id: does the open modal block the goal?
pub const BLOCKING_MODAL_ID: &str = "blocking_modal";
/// `Question::Choice` id: which of the identical targets is meant?
pub const DISAMBIGUATE_ID: &str = "disambiguate";
/// `Question::Bool` id: did an unchanged successful action take effect?
pub const ACTION_EFFECT_ID: &str = "action_effect";
/// `Question::Bool` id: does a changed world already satisfy the goal?
pub const GOAL_COMPLETE_ID: &str = "goal_complete";

/// The action with its semantic target's `index` cleared — twins
/// compare equal under it.
fn without_index(action: &Action) -> Option<Action> {
    let mut a = action.clone();
    let target = match &mut a {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. }
        | Action::FillCredential { target, .. } => Some(target),
        Action::TypeText { target, .. } | Action::Scroll { target, .. } => target.as_mut(),
        _ => None,
    };
    match target {
        Some(Target::Semantic(st)) if st.index.is_some() => {
            st.index = None;
            Some(a)
        }
        _ => None,
    }
}

/// Indices of the candidates tied with the top one (same prior) whose
/// actions differ only by the target's `index` — identical labels the
/// generator cannot tell apart. Empty when there is no such tie.
pub fn ambiguous_group(candidates: &[CandidateAction]) -> Vec<usize> {
    let Some(top) = candidates.first() else {
        return Vec::new();
    };
    let Some(key) = without_index(&top.action) else {
        return Vec::new();
    };
    let group: Vec<usize> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| {
            (c.prior - top.prior).abs() < f32::EPSILON
                && without_index(&c.action).as_ref() == Some(&key)
        })
        .map(|(i, _)| i)
        .collect();
    if group.len() < 2 {
        Vec::new()
    } else {
        group
    }
}

/// The extra typed questions a Q&A engine asks alongside its pick.
pub fn typed_questions(ctx: &DecisionContext) -> Vec<Question> {
    let mut out = Vec::new();
    if let Some(modal) = ctx
        .candidates
        .iter()
        .find_map(|c| c.behind_modal.as_deref())
    {
        out.push(Question::Bool {
            id: BLOCKING_MODAL_ID.into(),
            prompt: format!(
                "A modal dialog \"{modal}\" is open over the window. Does it block \
                 the goal in [GOAL] — must it be handled before acting on the \
                 elements behind it?"
            ),
        });
    }
    let group = ambiguous_group(&ctx.candidates);
    if !group.is_empty() {
        let mut options: Vec<String> = group
            .iter()
            .enumerate()
            .map(|(k, &i)| format!("option {k}: {}", ctx.candidates[i].rationale))
            .collect();
        options.push("none — the goal names none of these".into());
        out.push(Question::Choice {
            id: DISAMBIGUATE_ID.into(),
            prompt: "Several elements share the same label. Using their context, \
                     which one does the goal in [GOAL] mean? Pick 'none' if the \
                     goal does not single one out."
                .into(),
            options,
        });
    }
    if let Some(last_act) = &ctx.last_act {
        match last_act.delta {
            WorldDelta::Unchanged | WorldDelta::Unknown => {
                let action = format!("{:?}", last_act.action);
                let incomplete = if last_act.delta == WorldDelta::Unknown {
                    " Perception was incomplete, so no change could be confirmed."
                } else {
                    ""
                };
                out.push(Question::Bool {
                    id: ACTION_EFFECT_ID.into(),
                    prompt: format!(
                        "The previous action {action} reported success but no change was observed.{incomplete} \
                         Did it take effect (e.g. invisibly), so repeating it would do it twice?"
                    ),
                });
            }
            WorldDelta::Changed => out.push(Question::Bool {
                id: GOAL_COMPLETE_ID.into(),
                prompt: "After the previous action the world changed. Is the goal in [GOAL] already complete?"
                    .into(),
            }),
        }
    }
    out
}

/// Whether the candidate at `candidate_index` would blindly repeat the
/// last successful action after an unchanged or uncertain observation.
pub fn repeats_last_act(ctx: &DecisionContext, candidate_index: usize) -> bool {
    let Some(last_act) = &ctx.last_act else {
        return false;
    };
    matches!(last_act.delta, WorldDelta::Unchanged | WorldDelta::Unknown)
        && ctx
            .candidates
            .get(candidate_index)
            .is_some_and(|candidate| candidate.action == last_act.action)
}
