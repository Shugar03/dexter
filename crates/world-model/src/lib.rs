//! World model: queries and digest rendering over normalized observations.
//!
//! Pure functions — no I/O, no platform APIs. This is the seam where drivers
//! hand off normalized elements and where decision engines (including Laya)
//! get their `state`.

use dexter_core::{
    DexterError, Element, ElementSource, Observation, SemanticTarget, Target, Window,
};

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
/// Shared filter for digests and agent-facing element lists.
pub fn digest_worthy(e: &Element) -> bool {
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

/// Order-independent fingerprint of what the world contains — the change
/// detector behind per-act verification. Element ids are excluded on
/// purpose (AX ids regenerate per observation and would mark every world
/// "changed"); bounds are quantized to an 8px grid so sub-cell jitter
/// doesn't flip the signature but a real move/resize does. Sorted window
/// titles, a screenshot-presence bit and per-role counts ride along.
///
/// Menu-catalog elements are excluded: they outnumber window controls
/// ~10:1, almost never carry verification-relevant state, and dropping
/// them lets verification re-observes skip the menu-bar walk entirely
/// while signatures stay comparable.
pub fn signature(obs: &Observation) -> u64 {
    use std::collections::BTreeMap;
    use std::hash::{Hash, Hasher};
    type Item<'e> = (
        &'e str,
        &'e str,
        &'e str,
        Option<bool>,
        bool,
        Option<(i64, i64, i64, i64)>,
    );
    let cell = |v: f64| (v / 8.0).floor() as i64;
    let mut items: Vec<Item> = obs
        .elements
        .iter()
        .filter(|e| !is_menu_element(e))
        .map(|e| {
            (
                e.role.as_deref().unwrap_or(""),
                e.name.as_deref().unwrap_or(""),
                e.value.as_deref().unwrap_or(""),
                e.enabled,
                e.focused,
                e.bounds
                    .map(|b| (cell(b.x), cell(b.y), cell(b.w), cell(b.h))),
            )
        })
        .collect();
    items.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    items.hash(&mut h);
    let mut titles: Vec<&str> = obs
        .windows
        .iter()
        .map(|w| w.title.as_deref().unwrap_or(""))
        .collect();
    titles.sort_unstable();
    titles.hash(&mut h);
    obs.screenshot.is_some().hash(&mut h);
    let mut role_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for e in obs.elements.iter().filter(|e| !is_menu_element(e)) {
        *role_counts
            .entry(e.role.as_deref().unwrap_or(""))
            .or_default() += 1;
    }
    role_counts.hash(&mut h);
    h.finish()
}

/// Menu-catalog roles — `AXMenu*` raw or the normalized equivalents.
/// Counted out of [`signature`] so menu-presence differences between
/// observations never read as world changes. Also used by the engine:
/// an expectation that only a menu's appearance could satisfy is
/// unverifiable wherever menu windows can't be observed.
pub fn is_menu_element(e: &Element) -> bool {
    e.raw_role
        .as_deref()
        .is_some_and(|r| r.starts_with("AXMenu"))
        || matches!(
            e.role.as_deref(),
            Some("menu" | "menu_item" | "menu_bar" | "menu_bar_item")
        )
}

/// Render an observation as compact text — the `state` a decision engine
/// (e.g. Laya) consumes. Interactive/named elements come first, truncation is
/// explicit, and no values are included beyond short labels.
pub fn digest(obs: &Observation, max_lines: usize) -> String {
    let mut lines = header_lines(obs);
    let mut shown = 0usize;
    let mut skipped = 0usize;
    let mut menu_roots: Vec<&str> = Vec::new();
    let mut menu_count = 0usize;
    for e in &obs.elements {
        if !digest_worthy(e) {
            continue;
        }
        // Menu catalog collapses to one line — items are reachable via
        // key chords and `map`, not worth a digest line each.
        if is_menu_element(e) {
            menu_count += 1;
            if matches!(e.role.as_deref(), Some("menu_bar_item")) {
                if let Some(n) = e.name.as_deref() {
                    menu_roots.push(n);
                }
            }
            continue;
        }
        if shown >= max_lines {
            skipped += 1;
            continue;
        }
        shown += 1;
        lines.push(element_line(e));
    }
    if menu_count > 0 {
        lines.push(menu_summary_line(menu_count, &menu_roots));
    }
    if skipped > 0 {
        lines.push(format!("... truncated: {skipped} elements not shown"));
    }
    lines.join("\n")
}

fn menu_summary_line(count: usize, roots: &[&str]) -> String {
    if roots.is_empty() {
        format!("menubar: {count} items (key chords or map for verbs)")
    } else {
        format!("menubar: {count} items ({})", roots.join(", "))
    }
}

/// Render with a character budget — for engines with a fixed context
/// window (Laya's encoder tops out at 8192 tokens; ~14k chars of this
/// mostly-ASCII digest stays under it). Elements are emitted in tree
/// order until the budget runs out; the tail is summarized honestly.
pub fn digest_budget(obs: &Observation, max_chars: usize) -> String {
    let mut lines = header_lines(obs);
    let mut used: usize = lines.iter().map(|l| l.len() + 1).sum();
    let mut skipped = 0usize;
    let mut menu_roots: Vec<&str> = Vec::new();
    let mut menu_count = 0usize;
    for e in &obs.elements {
        if !digest_worthy(e) {
            continue;
        }
        // Same collapse as `digest` — the menu catalog would eat the
        // whole context budget on menu-heavy apps.
        if is_menu_element(e) {
            menu_count += 1;
            if matches!(e.role.as_deref(), Some("menu_bar_item")) {
                if let Some(n) = e.name.as_deref() {
                    menu_roots.push(n);
                }
            }
            continue;
        }
        let line = element_line(e);
        if used + line.len() + 1 > max_chars {
            skipped += 1;
            continue;
        }
        used += line.len() + 1;
        lines.push(line);
    }
    if menu_count > 0 {
        lines.push(menu_summary_line(menu_count, &menu_roots));
    }
    if skipped > 0 {
        lines.push(format!(
            "... truncated: {skipped} elements not shown (context budget)"
        ));
    }
    lines.join("\n")
}

/// Scope an observation to a single window: elements filtered to those
/// whose bounds intersect the window's rect, windows list narrowed to
/// the target, digest rebuilt. Elements without bounds (menubar items
/// and other unpositioned nodes) don't live inside a window — they're
/// dropped, which is the honest semantic. `None` when the window id
/// isn't in this observation — a miss, not an empty fake.
pub fn within_window(obs: &Observation, window_id: u32) -> Option<Observation> {
    let win = obs.windows.iter().find(|w| w.id == window_id)?.clone();
    let mut scoped = obs.clone();
    scoped.windows = vec![win.clone()];
    scoped
        .elements
        .retain(|e| e.bounds.is_some_and(|b| rects_intersect(b, win.bounds)));
    scoped.digest = digest(&scoped, 500);
    Some(scoped)
}

fn rects_intersect(a: dexter_core::Rect, b: dexter_core::Rect) -> bool {
    a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
}

/// Apply `ObservationScope.window` semantics to a driver-returned
/// observation. If the driver already scoped natively (`windows` is
/// exactly `[window_id]`), the observation is returned unchanged —
/// re-filtering by bounds would wrongly drop subtree elements that
/// overflow the window rect (popovers, menus). Otherwise the post-walk
/// `within_window` filter applies. `Err` on an unknown id — a miss,
/// never a silent empty observation.
pub fn scope_to_window(obs: Observation, window_id: u32) -> Result<Observation, String> {
    if obs.windows.len() == 1 && obs.windows[0].id == window_id {
        return Ok(obs);
    }
    within_window(&obs, window_id).ok_or_else(|| format!("window {window_id} not in observation"))
}

fn header_lines(obs: &Observation) -> Vec<String> {
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
    lines
}

fn element_line(e: &Element) -> String {
    let indent = "  ".repeat((e.depth as usize).min(8));
    // OCR-derived elements are marked: no live handle, lower confidence —
    // the agent must not treat them like AX-resolvable nodes.
    let source = match e.source {
        ElementSource::Ocr => "[ocr] ",
        ElementSource::Vision => "[vision] ",
        _ => "",
    };
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
    format!(
        "{indent}{}{source}{role}{name}{flags}{actions}{bounds}",
        e.id
    )
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

/// A summarized capability map of an observed app — what it is, what
/// its controls do, where they live. The artifact an agent consumes to
/// theorize about an interface it has never seen: built from one
/// observation, no per-app hand-authored knowledge.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct AppMap {
    /// Window titles + bounds (windows are named surfaces of work).
    pub windows: Vec<MapWindow>,
    /// Elements per role, most frequent first — the app's shape.
    pub role_counts: Vec<(String, usize)>,
    /// Menubar menu_item labels — the app's verb vocabulary.
    pub menu_verbs: Vec<String>,
    /// Named pressable controls (button/tab/menu_button/...).
    pub controls: Vec<String>,
    /// Editable surfaces — text fields/areas, search, combo, sliders.
    pub editable: Vec<String>,
    /// Navigation surfaces — tabs, radio groups, sidebars/outlines.
    pub navigation: Vec<String>,
    /// Inferred capabilities from label/role evidence.
    pub capabilities: Vec<String>,
    /// True when CG sees windows but AX exposes none — the app is not
    /// frontmost or its window is off-Space. Map is menubar-only.
    pub ax_limited: bool,
}

/// One window surface in the map.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MapWindow {
    pub title: Option<String>,
    pub bounds: Option<dexter_core::Rect>,
}

/// Roles whose presence means the app can navigate.
const NAV_ROLES: &[&str] = &[
    "radio_button",
    "tab",
    "tab_group",
    "outline",
    "scroll_area",
    "pop_up_button",
];

/// Roles whose presence means the app can take text input.
const EDIT_ROLES: &[&str] = &[
    "text_field",
    "text_area",
    "search_field",
    "combo_box",
    "secure_text_field",
];

/// Roles that are pressable controls.
const CONTROL_ROLES: &[&str] = &["button", "menu_button", "check_box", "link", "stepper"];

/// Label hints for the calculator inference — digits plus operators.
const CALC_OPS: &[&str] = &[
    "sumar",
    "restar",
    "multiplicar",
    "dividir",
    "igual",
    "add",
    "subtract",
    "multiply",
    "divide",
    "equals",
    "+",
    "-",
    "×",
    "÷",
    "=",
];

/// Label hints that mark a document-editing surface.
const DOC_VERBS: &[&str] = &["guardar", "save", "exportar", "export", "print", "imprimir"];

/// Build the map: clusters by role, the menubar verb vocabulary, then
/// capability inference over the collected evidence.
pub fn app_map(obs: &Observation) -> AppMap {
    let mut map = AppMap::default();
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();

    for e in &obs.elements {
        let role = e.role.clone().unwrap_or_else(|| "?".into());
        *counts.entry(role.clone()).or_default() += 1;
        let name = e.name.as_deref().unwrap_or("").trim();
        match role.as_str() {
            "window" | "dialog" | "sheet" | "drawer" => map.windows.push(MapWindow {
                title: if name.is_empty() {
                    None
                } else {
                    Some(name.into())
                },
                bounds: e.bounds,
            }),
            "menu_item" if !name.is_empty() => map.menu_verbs.push(name.into()),
            r if NAV_ROLES.contains(&r) && !name.is_empty() => {
                map.navigation.push(format!("{r} '{name}'"));
            }
            r if EDIT_ROLES.contains(&r) => map.editable.push(if name.is_empty() {
                if e.focused {
                    format!("{r} (unnamed, focused)")
                } else {
                    format!("{r} (unnamed)")
                }
            } else {
                format!("{r} '{name}'")
            }),
            r if CONTROL_ROLES.contains(&r) && !name.is_empty() => {
                map.controls.push(format!("{r} '{name}'"));
            }
            _ => {}
        }
    }
    map.role_counts = counts.into_iter().collect::<Vec<_>>().tap_sort_desc();
    map.menu_verbs.sort();
    map.menu_verbs.dedup();
    map.controls.sort();
    map.controls.dedup();
    map.navigation.sort();
    map.navigation.dedup();

    infer_capabilities(obs, &mut map);
    map.ax_limited = obs.ax_limited || (!obs.windows.is_empty() && map.windows.is_empty());
    map
}

/// Evidence-based inference — small honest rules over the collected
/// labels. Each capability names its evidence so callers can judge it.
fn infer_capabilities(obs: &Observation, map: &mut AppMap) {
    let label_of = |e: &Element| e.name.as_deref().unwrap_or("").to_lowercase();
    let is_button = |e: &Element| e.role.as_deref() == Some("button");

    // Calculator: ≥6 distinct digit buttons plus ≥2 operator labels.
    let digits = obs
        .elements
        .iter()
        .filter(|e| is_button(e))
        .filter_map(|e| e.name.as_deref().map(str::trim))
        .filter(|n| n.len() == 1 && n.chars().all(|c| c.is_ascii_digit()))
        .collect::<std::collections::BTreeSet<_>>();
    let ops = obs
        .elements
        .iter()
        .filter(|e| is_button(e))
        .map(label_of)
        .filter(|l| CALC_OPS.iter().any(|op| l.contains(op)))
        .count();
    if digits.len() >= 6 && ops >= 2 {
        map.capabilities.push(format!(
            "calculator-like ({} digit buttons, {} operator controls)",
            digits.len(),
            ops
        ));
    }

    // Document editor: an editable surface plus a save/export verb.
    let has_edit_surface = !map.editable.is_empty();
    let has_doc_verb = map
        .menu_verbs
        .iter()
        .map(|v| v.to_lowercase())
        .any(|v| DOC_VERBS.iter().any(|d| v.contains(d)));
    if has_edit_surface && has_doc_verb {
        map.capabilities
            .push("document editor (editable surface + save/export verbs)".into());
    }

    // Menu-driven surface: many verbs, little window chrome.
    if map.menu_verbs.len() >= 20 {
        map.capabilities.push(format!(
            "menu-driven ({} menubar verbs reachable without focus)",
            map.menu_verbs.len()
        ));
    }

    // Navigation-rich: tabs/radio groups/sidebar present.
    if map.navigation.len() >= 3 {
        map.capabilities.push(format!(
            "sectioned UI ({} navigation controls)",
            map.navigation.len()
        ));
    }
}

trait TapSort {
    fn tap_sort_desc(self) -> Self;
}
impl TapSort for Vec<(String, usize)> {
    fn tap_sort_desc(mut self) -> Self {
        self.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        self
    }
}
