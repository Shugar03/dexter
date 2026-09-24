//! Shared element-resolution semantics behind the driver seam — one
//! stale/ambiguity contract for every adapter.
//!
//! An `Element` target is a *token* over a held observation: valid only
//! while that snapshot is held and the referenced node still matches a
//! fresh walk. Every miss fails closed as [`DriverError::StaleReference`]
//! — a stale token is never a `NotFound` (the world isn't missing; the
//! reference is old). Adapters keep their mechanics (AX walks, DOM
//! evals, in-memory state) and delegate identity to this module.

use crate::DriverError;
use dexter_core::{
    DexterError, Element, ElementId, ElementSource, Observation, ObservationId, Rect,
};

/// Whether `fresh` is plausibly the same UI node `stored` referenced:
/// role, name, parent, depth and bounds within 2px. The strictest rule
/// the adapters had (macOS) is now the only rule — a moved or
/// reparented element is a different element, and the id must be
/// re-earned with a fresh observation.
pub fn same_identity(stored: &Element, fresh: &Element) -> bool {
    stored.role == fresh.role
        && stored.name == fresh.name
        && stored.parent == fresh.parent
        && stored.depth == fresh.depth
        && bounds_close(stored.bounds, fresh.bounds)
}

fn bounds_close(a: Option<Rect>, b: Option<Rect>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            (a.x - b.x).abs() <= 2.0
                && (a.y - b.y).abs() <= 2.0
                && (a.w - b.w).abs() <= 2.0
                && (a.h - b.h).abs() <= 2.0
        }
        _ => false,
    }
}

/// First half of the stale-token contract: `stored` is the element list
/// the referenced observation held (`None` = the snapshot is no longer
/// held). The observation must be held, the element must be part of it,
/// and evidence-only elements (OCR text has no live handle) refuse with
/// an actionable message.
pub fn stored_element(
    stored: Option<&[Element]>,
    observation: ObservationId,
    element: ElementId,
) -> Result<&Element, DriverError> {
    let elements = stored.ok_or_else(|| {
        DriverError::StaleReference(format!(
            "observation {} is no longer held — re-observe",
            observation.0
        ))
    })?;
    let el = elements.iter().find(|e| e.id == element).ok_or_else(|| {
        DriverError::StaleReference(format!(
            "element {} is not part of observation {} — re-observe",
            element.0, observation.0
        ))
    })?;
    if el.source == ElementSource::Ocr {
        return Err(DriverError::StaleReference(format!(
            "element {} is OCR-derived — target its bounds center as a Point instead",
            element.0
        )));
    }
    Ok(el)
}

/// Second half: the element must still exist in a walk taken *now* and
/// match the stored identity. `None` = the node vanished.
pub fn verify_identity(
    stored: &Element,
    fresh: Option<&Element>,
    observation: ObservationId,
    element: ElementId,
) -> Result<(), DriverError> {
    match fresh {
        None => Err(DriverError::StaleReference(format!(
            "element {} vanished — the tree shrank since observation {}",
            element.0, observation.0
        ))),
        Some(f) if !same_identity(stored, f) => Err(DriverError::StaleReference(format!(
            "element {} changed since observation {} — re-observe",
            element.0, observation.0
        ))),
        Some(_) => Ok(()),
    }
}

/// The full stale-token check an `Element` target must pass. `stored`
/// is the referenced snapshot's element list; `fresh` is a walk taken
/// now. Returns the element's position inside `fresh` — the caller maps
/// that to its live handle (AX node, DOM node, state slot).
pub fn resolve_element_ref(
    stored: Option<&[Element]>,
    fresh: &[Element],
    observation: ObservationId,
    element: ElementId,
) -> Result<usize, DriverError> {
    let stored = stored_element(stored, observation, element)?;
    let idx = fresh.iter().position(|e| e.id == element);
    verify_identity(stored, idx.map(|i| &fresh[i]), observation, element)?;
    Ok(idx.expect("position was Some — verify_identity refuses None"))
}

/// Semantic/Focused resolution against a fresh observation — ambiguity
/// and not-found fail closed identically across drivers, and a partial
/// tree says so in the error instead of pretending definitiveness.
pub fn resolve_semantic<'o>(
    obs: &'o Observation,
    target: &dexter_core::Target,
) -> Result<&'o Element, DriverError> {
    dexter_world_model::resolve_element(obs, target).map_err(|e| match e {
        DexterError::Ambiguous(m) => DriverError::Ambiguous(m),
        DexterError::NotFound(m) => {
            if obs.elements_truncated {
                DriverError::NotFound(format!(
                    "{m} (element list truncated — result not definitive)"
                ))
            } else {
                DriverError::NotFound(m)
            }
        }
        other => DriverError::Platform(other.to_string()),
    })
}
