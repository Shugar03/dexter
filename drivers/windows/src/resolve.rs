//! Windows-specific press-pattern preference and URL allowlist.
//! Shared element identity rules live in `dexter_driver::identity`.

pub use dexter_driver::identity::{
    bounds_close, fresh_at_stored_index, resolve_error, same_element,
};

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
