//! World model: queries and digest rendering over normalized observations.
//!
//! Pure functions — no I/O, no platform APIs. This is the seam where drivers
//! hand off normalized elements and where decision engines (including Laya)
//! get their `state`.

use dexter_core::{DexterError, Element, Observation, SemanticTarget, Target, Window};

/// Normalize a platform role (`AXButton`, `AXTextField`, ...) to a canonical
/// lowercase role (`button`, `text_field`). Unknown roles are lowercased and
/// keep their `AX` prefix removed when present.
pub fn normalize_ax_role(raw: &str) -> String {
    let stripped = raw.strip_prefix("AX").unwrap_or(raw);
    // CamelCase -> snake_case, lowercased. "AXTextField" -> "text_field",
    // "AXURL" -> "url" (consecutive capitals stay one word).
    let mut out = String::with_capacity(stripped.len() + 4);
    let chars: Vec<char> = stripped.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower =
                i > 0 && (chars[i - 1].is_lowercase() || chars[i - 1].is_ascii_digit());
            let acronym_boundary = i > 0
                && chars[i - 1].is_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev_lower || acronym_boundary {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

/// Elements matching every present field of `target`, in tree order.
pub fn find_elements<'o>(obs: &'o Observation, target: &SemanticTarget) -> Vec<&'o Element> {
    obs.elements
        .iter()
        .filter(|e| matches_target(e, target))
        .collect()
}

fn eq_ci(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// Role comparison: normalize both sides and ignore `_`/`-` separators so
/// `checkbox`, `check-box` and `AXCheckBox` all mean the same thing.
fn role_eq(query: &str, role: &str) -> bool {
    let strip = |s: &str| -> String {
        normalize_ax_role(s)
            .chars()
            .filter(|c| *c != '_' && *c != '-')
            .collect()
    };
    strip(query) == strip(role)
}

fn contains_ci(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

fn matches_target(e: &Element, t: &SemanticTarget) -> bool {
    if let Some(role) = &t.role {
        let hit = e.role.as_deref().is_some_and(|r| role_eq(role, r))
            || e.raw_role.as_deref().is_some_and(|r| role_eq(role, r));
        if !hit {
            return false;
        }
    }
    if let Some(name) = &t.name {
        if !e.name.as_deref().is_some_and(|n| eq_ci(n, name)) {
            return false;
        }
    }
    if let Some(sub) = &t.name_contains {
        if !e.name.as_deref().is_some_and(|n| contains_ci(n, sub)) {
            return false;
        }
    }
    if let Some(sub) = &t.value_contains {
        if !e.value.as_deref().is_some_and(|v| contains_ci(v, sub)) {
            return false;
        }
    }
    if let Some(id) = &t.identifier {
        if !e.identifier.as_deref().is_some_and(|i| eq_ci(i, id)) {
            return false;
        }
    }
    if let Some(enabled) = t.enabled {
        if e.enabled != Some(enabled) {
            return false;
        }
    }
    true
}

/// Resolve a [`Target`] to a concrete element of this observation.
///
/// Point/Window targets do not resolve to elements — callers route those to
/// the driver directly.
pub fn resolve_element<'o>(
    obs: &'o Observation,
    target: &Target,
) -> Result<&'o Element, DexterError> {
    match target {
        Target::Element {
            observation,
            element,
        } => {
            if *observation != obs.id {
                return Err(DexterError::InvalidInput(format!(
                    "element {element} belongs to observation {}, not {}",
                    observation.0, obs.id.0
                )));
            }
            obs.element(*element)
                .ok_or_else(|| DexterError::NotFound(format!("element {element}")))
        }
        Target::Semantic(t) => {
            let found = find_elements(obs, t);
            match t.index {
                Some(i) => found.get(i).copied().ok_or_else(|| {
                    if found.is_empty() {
                        DexterError::NotFound(format!("{t:?}"))
                    } else {
                        DexterError::Ambiguous(format!(
                            "index {i} out of {} matches for {t:?}",
                            found.len()
                        ))
                    }
                }),
                None => match found.as_slice() {
                    [one] => Ok(*one),
                    [] => Err(DexterError::NotFound(format!("{t:?}"))),
                    many => Err(DexterError::Ambiguous(format!(
                        "{} elements match {t:?} — set index to pick one",
                        many.len()
                    ))),
                },
            }
        }
        Target::Focused => obs
            .elements
            .iter()
            .find(|e| e.focused)
            .ok_or_else(|| DexterError::NotFound("no focused element".into())),
        Target::Point { .. } | Target::Window { .. } => Err(DexterError::InvalidInput(
            "target does not resolve to an element".into(),
        )),
    }
}

/// A named window line for the digest.
fn window_line(w: &Window) -> String {
    let title = w.title.as_deref().unwrap_or("<redacted>");
    format!(
        "window {} \"{}\" {} [{},{},{}x{}]",
        w.id, w.app, title, w.bounds.x, w.bounds.y, w.bounds.w, w.bounds.h
    )
}

/// Whether an element is worth feeding to a decision engine: named,
/// actionable, or meaningfully typed. Anonymous containers are noise.
fn digest_worthy(e: &Element) -> bool {
    if e.name.as_deref().is_some_and(|n| !n.is_empty()) {
        return true;
    }
    if !e.actions.is_empty() {
        return true;
    }
    // Keep structural landmarks even when anonymous.
    matches!(
        e.role.as_deref(),
        Some("window" | "dialog" | "sheet" | "toolbar" | "menu_bar" | "tab_group")
    )
}

/// Render an observation as compact text — the `state` a decision engine
/// (e.g. Laya) consumes. Interactive/named elements come first, truncation is
/// explicit, and no values are included beyond short labels.
pub fn digest(obs: &Observation, max_lines: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    let app = obs
        .app
        .as_ref()
        .map(|a| match a {
            dexter_core::AppSelector::Pid(p) => format!("pid {p}"),
            dexter_core::AppSelector::BundleId(b) => b.clone(),
            dexter_core::AppSelector::Name(n) => n.clone(),
        })
        .unwrap_or_else(|| "screen".into());
    lines.push(format!(
        "observation {} of {app} ({} windows, {} elements)",
        obs.id.0,
        obs.windows.len(),
        obs.elements.len()
    ));
    for w in &obs.windows {
        lines.push(window_line(w));
    }

    let mut shown = 0usize;
    let mut skipped = 0usize;
    for e in &obs.elements {
        if !digest_worthy(e) {
            continue;
        }
        if shown >= max_lines {
            skipped += 1;
            continue;
        }
        shown += 1;
        let indent = "  ".repeat((e.depth as usize).min(8));
        let role = e.role.as_deref().unwrap_or("element");
        let name = e
            .name
            .as_deref()
            .filter(|n| !n.is_empty())
            .map(|n| format!(" \"{}\"", truncate(n, 80)))
            .unwrap_or_default();
        let flags = [
            e.enabled.map(|v| if v { "enabled" } else { "disabled" }),
            e.focused.then_some("focused"),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(",");
        let actions = if e.actions.is_empty() {
            String::new()
        } else {
            format!(" actions=[{}]", e.actions.join(","))
        };
        let bounds = e
            .bounds
            .map(|b| format!(" [{},{},{}x{}]", b.x, b.y, b.w, b.h))
            .unwrap_or_default();
        let flags = if flags.is_empty() {
            String::new()
        } else {
            format!(" {flags}")
        };
        lines.push(format!(
            "{indent}{}{role}{name}{flags}{actions}{bounds}",
            e.id
        ));
    }
    if skipped > 0 {
        lines.push(format!("... truncated: {skipped} elements not shown"));
    }
    lines.join("\n")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}
