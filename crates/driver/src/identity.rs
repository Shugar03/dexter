use crate::DriverError;
use dexter_core::{DexterError, Element, Rect};

/// Whether fresh bounds are within a small layout drift of the stored frame.
pub fn bounds_close(a: Option<Rect>, b: Option<Rect>) -> bool {
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

/// Whether `fresh` is plausibly the same UI element `stored` pointed at.
pub fn same_element(stored: &Element, fresh: &Element) -> bool {
    stored.role == fresh.role
        && stored.name == fresh.name
        && stored.parent == fresh.parent
        && stored.depth == fresh.depth
        && bounds_close(stored.bounds, fresh.bounds)
}

/// The live `fresh` element `stored` still refers to, by 1-based tree index.
pub fn fresh_at_stored_index<'a>(stored: &Element, fresh: &'a [Element]) -> Option<&'a Element> {
    let idx = usize::try_from(stored.id.0.checked_sub(1)?).ok()?;
    let f = fresh.get(idx)?;
    same_element(stored, f).then_some(f)
}

/// Map world-model resolution failures to driver errors, preserving the
/// uncertainty of a not-found result from a truncated tree.
pub fn resolve_error(e: DexterError, truncated: bool) -> DriverError {
    match e {
        DexterError::Ambiguous(m) => DriverError::Ambiguous(m),
        DexterError::NotFound(m) => {
            if truncated {
                DriverError::NotFound(format!(
                    "{m} (element list truncated — result not definitive)"
                ))
            } else {
                DriverError::NotFound(m)
            }
        }
        other => DriverError::Platform(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DriverError;
    use dexter_core::{ElementId, ElementSource};

    fn el(id: u64) -> Element {
        Element {
            id: ElementId(id),
            parent: Some(ElementId(1)),
            depth: 2,
            role: Some("button".into()),
            name: Some("OK".into()),
            bounds: Some(Rect {
                x: 10.0,
                y: 10.0,
                w: 40.0,
                h: 20.0,
            }),
            source: ElementSource::Accessibility,
            ..Element::default()
        }
    }

    #[test]
    fn bounds_close_tolerates_small_drift_only() {
        let a = Some(Rect {
            x: 10.0,
            y: 10.0,
            w: 40.0,
            h: 20.0,
        });
        let near = Some(Rect {
            x: 11.5,
            y: 9.0,
            w: 41.0,
            h: 20.0,
        });
        let far = Some(Rect {
            x: 15.0,
            y: 10.0,
            w: 40.0,
            h: 20.0,
        });
        assert!(bounds_close(a, near));
        assert!(!bounds_close(a, far));
        assert!(bounds_close(None, None));
        assert!(!bounds_close(a, None));
    }

    #[test]
    fn same_element_compares_identity_fields() {
        let stored = el(3);
        assert!(same_element(&stored, &el(3)));
        for change in [
            Element {
                role: Some("link".into()),
                ..el(3)
            },
            Element {
                name: Some("Cancel".into()),
                ..el(3)
            },
            Element {
                parent: Some(ElementId(2)),
                ..el(3)
            },
            Element { depth: 3, ..el(3) },
            Element {
                bounds: Some(Rect {
                    x: 40.0,
                    y: 10.0,
                    w: 40.0,
                    h: 20.0,
                }),
                ..el(3)
            },
        ] {
            assert!(!same_element(&stored, &change), "{change:?}");
        }
    }

    #[test]
    fn fresh_at_stored_index_uses_tree_order_position() {
        let fresh: Vec<Element> = (1..=5).map(el).collect();
        let stored = el(3);
        let hit = fresh_at_stored_index(&stored, &fresh).expect("index 2 exists and matches");
        assert_eq!(hit.id, ElementId(3));
    }

    #[test]
    fn fresh_at_stored_index_fails_closed() {
        let fresh: Vec<Element> = (1..=5).map(el).collect();
        assert!(fresh_at_stored_index(&el(9), &fresh).is_none());
        let mut renamed: Vec<Element> = (1..=5).map(el).collect();
        renamed[2].name = Some("Changed".into());
        assert!(fresh_at_stored_index(&el(3), &renamed).is_none());
    }

    #[test]
    fn resolve_error_maps_verdicts_and_marks_truncated_not_found() {
        assert!(matches!(
            resolve_error(DexterError::Ambiguous("two".into()), false),
            DriverError::Ambiguous(_)
        ));
        assert!(matches!(
            resolve_error(DexterError::NotFound("none".into()), false),
            DriverError::NotFound(m) if !m.contains("truncated")
        ));
        assert!(matches!(
            resolve_error(DexterError::NotFound("none".into()), true),
            DriverError::NotFound(m) if m.contains("not definitive")
        ));
        assert!(matches!(
            resolve_error(DexterError::Decision("x".into()), false),
            DriverError::Platform(_)
        ));
    }
}
