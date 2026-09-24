//! Verifier: evaluate [`ExpectedState`] against an [`Observation`].
//!
//! Three-valued logic throughout. A verdict that depends on the
//! *completeness* of the element tree degrades to `UNCERTAIN` whenever the
//! tree is partial (`elements_truncated` / `ax_limited`), and a title check
//! is `UNCERTAIN` when no window exposes a title. `UNCERTAIN` is never a
//! success — callers must retry, escalate, or fail.

use dexter_core::{
    ExpectedState, Observation, UnknownReason, ValuePredicate, Verification, VerificationStatus,
};

fn partial_tree(obs: &Observation) -> bool {
    obs.elements_truncated || obs.ax_limited
}

/// The verdict `status` assumes the tree is complete — degrade it to
/// UNCERTAIN whenever the walk was partial, recording why.
fn absence(
    status: VerificationStatus,
    partial: bool,
    reason: &mut Option<UnknownReason>,
) -> VerificationStatus {
    if partial {
        reason.get_or_insert(UnknownReason::TreePartial);
        VerificationStatus::Uncertain
    } else {
        status
    }
}

fn eval(
    obs: &Observation,
    expected: &ExpectedState,
    checks: &mut Vec<String>,
    reason: &mut Option<UnknownReason>,
) -> VerificationStatus {
    let partial = partial_tree(obs);
    match expected {
        ExpectedState::ElementExists { target } => {
            let n = dexter_world_model::find_elements(obs, target).len();
            checks.push(format!("element_exists {target:?}: {n} matches"));
            if n > 0 {
                VerificationStatus::Verified
            } else {
                absence(VerificationStatus::Failed, partial, reason)
            }
        }
        ExpectedState::ElementAbsent { target } => {
            let n = dexter_world_model::find_elements(obs, target).len();
            checks.push(format!("element_absent {target:?}: {n} matches"));
            if n > 0 {
                VerificationStatus::Failed
            } else {
                absence(VerificationStatus::Verified, partial, reason)
            }
        }
        ExpectedState::ElementValue { target, predicate } => {
            let found = dexter_world_model::find_elements(obs, target);
            let hit = found.iter().any(|e| {
                e.value
                    .as_deref()
                    .is_some_and(|v| value_ok(v, predicate, checks))
            });
            checks.push(format!(
                "element_value {target:?}: {} candidates, satisfied={hit}",
                found.len()
            ));
            if hit {
                VerificationStatus::Verified
            } else if found.iter().any(|e| e.is_sensitive()) {
                // A secure-field candidate's value is redacted by
                // design — it could hold the expected value even when
                // no visible candidate does, so "no match" is
                // unknowable, never a failure.
                reason.get_or_insert(UnknownReason::RedactedValue);
                VerificationStatus::Uncertain
            } else if found.is_empty() {
                absence(VerificationStatus::Failed, partial, reason)
            } else {
                VerificationStatus::Failed
            }
        }
        ExpectedState::TextPresent { text } => {
            // Search the data, not the rendered digest: element
            // names/values and window titles. (The digest is a
            // presentation artifact — it filters, truncates and
            // collapses the menu catalog.)
            let needle = text.to_lowercase();
            let hit = obs.elements.iter().any(|e| {
                e.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().contains(&needle))
                    || e.value
                        .as_deref()
                        .is_some_and(|v| v.to_lowercase().contains(&needle))
            }) || obs.windows.iter().any(|w| {
                w.title
                    .as_deref()
                    .is_some_and(|t| t.to_lowercase().contains(&needle))
            });
            checks.push(format!("text_present {text:?}: {hit}"));
            if hit {
                VerificationStatus::Verified
            } else {
                absence(VerificationStatus::Failed, partial, reason)
            }
        }
        ExpectedState::FocusedElement { target } => {
            let focused = obs.elements.iter().find(|e| e.focused);
            match focused {
                None => {
                    checks.push("focused_element: no focused element in tree".into());
                    reason.get_or_insert(UnknownReason::NoFocusedElement);
                    VerificationStatus::Uncertain
                }
                Some(f) => {
                    let obs_one = Observation {
                        elements: vec![f.clone()],
                        ..Default::default()
                    };
                    let hit = !dexter_world_model::find_elements(&obs_one, target).is_empty();
                    checks.push(format!("focused_element {target:?}: match={hit}"));
                    if hit {
                        VerificationStatus::Verified
                    } else {
                        VerificationStatus::Failed
                    }
                }
            }
        }
        ExpectedState::WindowTitleContains { text } => {
            let any_title = obs.windows.iter().any(|w| w.title.is_some());
            let hit = obs
                .windows
                .iter()
                .any(|w| w.title.as_deref().is_some_and(|t| contains_ci(t, text)));
            checks.push(format!(
                "window_title_contains {text:?}: {hit} ({} windows)",
                obs.windows.len()
            ));
            if hit {
                VerificationStatus::Verified
            } else if !any_title {
                reason.get_or_insert(UnknownReason::NoWindowTitle);
                VerificationStatus::Uncertain
            } else {
                VerificationStatus::Failed
            }
        }
        ExpectedState::AppRunning { name } => {
            let hit = obs.windows.iter().any(|w| w.app.eq_ignore_ascii_case(name));
            checks.push(format!("app_running {name:?}: {hit}"));
            if hit {
                VerificationStatus::Verified
            } else {
                VerificationStatus::Failed
            }
        }
        ExpectedState::WorldChanged { from } => {
            let now = dexter_world_model::signature(obs);
            let changed = now != *from;
            checks.push(format!("world_changed from={from} now={now}: {changed}"));
            if changed {
                VerificationStatus::Verified
            } else {
                // Same signature on a partial tree can't claim failure
                // honestly — the missing elements may have moved.
                absence(VerificationStatus::Failed, partial, reason)
            }
        }
        ExpectedState::All { all } => {
            let mut any_fail = false;
            let mut any_uncertain = false;
            for e in all {
                match eval(obs, e, checks, reason) {
                    VerificationStatus::Verified => {}
                    VerificationStatus::Failed => any_fail = true,
                    VerificationStatus::Uncertain => any_uncertain = true,
                }
            }
            if any_fail {
                VerificationStatus::Failed
            } else if any_uncertain {
                VerificationStatus::Uncertain
            } else {
                VerificationStatus::Verified
            }
        }
        ExpectedState::Any { any } => {
            let mut any_verified = false;
            let mut any_uncertain = false;
            for e in any {
                match eval(obs, e, checks, reason) {
                    VerificationStatus::Verified => any_verified = true,
                    VerificationStatus::Failed => {}
                    VerificationStatus::Uncertain => any_uncertain = true,
                }
            }
            if any_verified {
                VerificationStatus::Verified
            } else if any_uncertain {
                VerificationStatus::Uncertain
            } else {
                VerificationStatus::Failed
            }
        }
        ExpectedState::Not { not } => match eval(obs, not, checks, reason) {
            VerificationStatus::Verified => VerificationStatus::Failed,
            VerificationStatus::Failed => VerificationStatus::Verified,
            VerificationStatus::Uncertain => VerificationStatus::Uncertain,
        },
    }
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

fn value_ok(value: &str, predicate: &ValuePredicate, checks: &mut Vec<String>) -> bool {
    match predicate {
        ValuePredicate::Equals(want) => value.eq_ignore_ascii_case(want),
        ValuePredicate::Contains(needle) => contains_ci(value, needle),
        ValuePredicate::Matches(pattern) => match regex::Regex::new(pattern) {
            Ok(re) => re.is_match(value),
            Err(e) => {
                checks.push(format!("invalid regex {pattern:?}: {e}"));
                false
            }
        },
    }
}

/// Evaluate `expected` against `obs`, collecting per-check detail lines.
pub fn verify(obs: &Observation, expected: &ExpectedState) -> Verification {
    let mut checks = Vec::new();
    let mut reason = None;
    let status = eval(obs, expected, &mut checks, &mut reason);
    Verification {
        status,
        checks,
        // A reason stranded on a definite verdict would mislead — only
        // UNCERTAIN carries one.
        unknown_reason: (status == VerificationStatus::Uncertain)
            .then_some(reason)
            .flatten(),
    }
}
