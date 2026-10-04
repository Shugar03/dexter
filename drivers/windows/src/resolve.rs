//! Pure halves of the `act()` target-resolution rules: identity
//! comparison between a stored element and its fresh counterpart, the
//! activation-pattern preference order, the world-model error mapping,
//! and the `Navigate` URL allowlist. Kept platform-free so the rules
//! are testable on every platform — the `cfg(windows)` act module only
//! wires these to live UIA/Win32 calls.

use dexter_core::{DexterError, Element, Rect};
use dexter_driver::DriverError;

/// Fresh UIA bounds can drift a pixel or two on focus changes; the
/// identity check tolerates ±2px on every edge, same as the AX slice.
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

/// The live `fresh` element `stored` still refers to: same flat index
/// (element ids are 1-based tree-order positions within an observation)
/// and still the same element. Anything else is a stale reference.
pub fn fresh_at_stored_index<'a>(stored: &Element, fresh: &'a [Element]) -> Option<&'a Element> {
    let idx = usize::try_from(stored.id.0.checked_sub(1)?).ok()?;
    let f = fresh.get(idx)?;
    same_element(stored, f).then_some(f)
}

/// Where a left click can land, in UIA pattern-preference order:
/// `Invoke` is real activation, `Toggle` and `SelectionItem` carry the
/// same press semantics on their controls, `ExpandCollapse` opens
/// disclosure affordances, and `LegacyIAccessible.DoDefaultAction` is
/// the MSAA bridge's last resort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PressPattern {
    Invoke,
    Toggle,
    SelectItem,
    ExpandCollapse,
    LegacyIAccessible,
}

/// The order press patterns are tried — `PRESS_ORDER[0]` wins.
pub const PRESS_ORDER: [PressPattern; 5] = [
    PressPattern::Invoke,
    PressPattern::Toggle,
    PressPattern::SelectItem,
    PressPattern::ExpandCollapse,
    PressPattern::LegacyIAccessible,
];

/// First pattern in `PRESS_ORDER` the element reports — `None` means no
/// activation pattern exists and the click must report `Unsupported`.
pub fn first_press_pattern(have: impl Fn(PressPattern) -> bool) -> Option<PressPattern> {
    PRESS_ORDER.into_iter().find(|&p| have(p))
}

/// World-model resolution failures → driver errors. A `NotFound` on a
/// truncated walk is flagged non-definitive — the element may exist
/// beyond the walk's depth/count cap.
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

/// Whether `url` may go to `ShellExecuteW("open", ...)`. Anything but a
/// small allowlist of document schemes can resolve to executable
/// content (a bare `file` path, `shell:`/`ms-settings:` shortcuts), so
/// navigation fails closed on the scheme.
pub fn navigable_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
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
        // Tree shrank: the stored index is gone.
        assert!(fresh_at_stored_index(&el(9), &fresh).is_none());
        // Same index, different element — the id slot means nothing alone.
        let mut renamed: Vec<Element> = (1..=5).map(el).collect();
        renamed[2].name = Some("Changed".into());
        assert!(fresh_at_stored_index(&el(3), &renamed).is_none());
    }

    #[test]
    fn press_order_prefers_real_activation() {
        let none = first_press_pattern(|_| false);
        assert_eq!(none, None);
        // With every pattern present, Invoke wins; without it the next
        // highest-preference pattern does.
        assert_eq!(first_press_pattern(|_| true), Some(PressPattern::Invoke));
        assert_eq!(
            first_press_pattern(|p| matches!(
                p,
                PressPattern::Toggle | PressPattern::LegacyIAccessible
            )),
            Some(PressPattern::Toggle)
        );
        assert_eq!(
            first_press_pattern(|p| matches!(
                p,
                PressPattern::SelectItem | PressPattern::ExpandCollapse
            )),
            Some(PressPattern::SelectItem)
        );
        assert_eq!(
            first_press_pattern(|p| p == PressPattern::LegacyIAccessible),
            Some(PressPattern::LegacyIAccessible)
        );
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

    #[test]
    fn navigable_url_allowlists_document_schemes() {
        for ok in [
            "https://example.com",
            "http://localhost:8080/x",
            "mailto:a@b.c",
        ] {
            assert!(navigable_url(ok), "{ok}");
        }
        for bad in [
            "notepad.exe",
            "C:\\Windows\\System32\\calc.exe",
            "file:///c:/windows/system32/calc.exe",
            "shell:AppsFolder\\x",
            "ms-settings:display",
            "javascript:alert(1)",
            "vbscript:x",
            "",
            "http:",
        ] {
            assert!(!navigable_url(bad), "{bad}");
        }
    }
}
