# SDD — Windows MSAA coverage: hidden-but-reachable + partial interior

## Intent

PR #61's MSAA pass skipped two shapes whole:

1. **Hidden descendant HWNDs** (`!IsWindowVisible`) — inactive tab
   pages, collapsed panes. Their controls were invisible to the
   observation, so "click the option on tab 3" was `NotFound` even
   though activating the tab would reveal them.
2. **Unclaimed HWNDs whose interior UIA already covered** — the
   `interior_covered` gate skipped the whole child window, losing any
   region UIA bridged only partially (OLE hosts that report some
   children windowless and others not at all).

Both gaps keep the tree honest today only by omission. This slice
keeps the honesty *and* recovers the coverage: hidden content is
walked and reported — marked — and partially-covered windows are
walked with the merge proving what UIA missed.

## Contract

- `Element.hidden: Option<bool>` carries the reachability verdict:
  `Some(true)` = not on screen (inside a hidden container, or the
  source's own offscreen state — UIA `CurrentIsOffscreen`, MSAA
  `STATE_SYSTEM_INVISIBLE|OFFSCREEN`), `Some(false)` = on screen,
  `None` = unreported (treated as on screen). Serde-defaulted so old
  observations still deserialize. The field is a perception fact,
  never an action gate by itself.
- The MSAA trigger (`msaa::augment`) no longer uses visibility or
  interior coverage as *skip* tests. Every unclaimed, non-nested
  descendant HWND is a walk root:
  - visible windows walk as before (state-marked nodes still appear;
    an `INVISIBLE|OFFSCREEN` node is now kept and marked `hidden`
    rather than dropped with its subtree — scrolled-out list items
    and collapsed content are reachable too);
  - hidden windows (a non-empty `GetWindowRect` is required) walk in
    hidden mode: every node inherits `hidden` from the root HWND plus
    its own state. Hidden roots queue behind visible ones so a deep
    hidden subtree can never starve visible coverage of budget.
  - `IsChild` nesting still dedupes walk roots across both queues.
- `merge_msaa` dedupe is visibility-aware: a node only collapses
  into a UIA element (or a kept MSAA element) when `hidden` matches —
  two tab pages can carry same-named controls at the same rect; they
  are different controls and must both survive.
- **Partial coverage stays flagged.** When an HWND's rect had UIA
  interior content and its MSAA walk still kept ≥1 new element, the
  interior was *proven* only partially covered: `augment` reports it
  and `observe` sets `ax_limited` — absence-dependent verification
  stays UNCERTAIN exactly as for a degraded grant.
- `SemanticTarget.hidden` mirrors `enabled`: `Some(true)` matches
  only hidden elements, `Some(false)` only on-screen ones, `None`
  unconstrained — verify can demand on-screen presence explicitly.
  Hidden elements still resolve semantically (they exist and are
  reachable); they are not *directly actionable*.
- Mutating acts on a hidden resolved element fail closed
  (`hidden_verdict` — `FAILED`, `Mechanism::Accessibility`, "reveal
  it first"): `Click` (any button — a coordinate fallback would land
  on whatever is actually drawn at that rect, i.e. a different
  control), `TypeText`, `SetValue`, `Focus`. `Scroll` with a target
  is deliberately exempt — `ScrollItemPattern.ScrollIntoView` is the
  legitimate reveal path, and `SelectionItem.Select` on the *tab
  element* (itself visible) is how a hidden page becomes visible.
  The verdict reads the fresh walk's element, so a control revealed
  between observe and act is not refused.
- Candidate generation skips hidden elements: they cannot produce
  `CandidateAction`s, so engines never propose them. The digest lists
  them with a `hidden` flag — reachable, marked, not proposed.
- Identity (`same_element` for stale checks) does **not** compare
  `hidden`: a reveal between observe and act must not stale out a
  still-identical control; the act-layer verdict re-reads the fresh
  element's flag.

## Honest bounds

- "Hidden" is exactly what the source reports: MSAA
  `INVISIBLE|OFFSCREEN` state or a hidden ancestor root, UIA
  `IsOffscreen`. Providers that lie get believed — no screen-space
  geometry inference is added.
- The flag fires only on measured evidence (`interior_covered` was
  true and kept elements emerged). A covered child whose walk adds
  nothing changes nothing — same tree, same dedupe, no flag.
- Cost: hidden roots add one `OBJID_CLIENT` COM walk each; visible
  roots are unchanged in order and budget priority. `max_elements`
  still bounds the merged list; starvation shows as
  `elements_truncated`, never silently.

## Measured (Windows box, before → after)

Same observe on each app via `drivers/windows/examples/probe.rs`:

| App | Class | elements | MSAA-sourced | hidden | ax_limited |
| --- | --- | --- | --- | --- | --- |
| msconfig.exe | legacy MFC tabbed | 28 → 285 | 0 → 257 | 260 | false → true |
| charmap.exe | legacy Win32 | 34 → 141 | 0 → 107 | 110 | false → false |
| odbcad32.exe | legacy Win32 | 33 → 54 | 0 → 21 | 23 | false → true |
| powershell_ise.exe | WPF | 221 → 244 | 0 → 23 | 32 | false → false |
| Chrome (new tab) | Chromium (Electron-class) | 61 → 93 | 0 → 32 | 6 | false → true |

msconfig's four inactive property pages surface fully marked hidden;
charmap's collapsed combo items likewise. odbcad32/Chrome/msconfig
keep `ax_limited` honest — the MSAA merge found real controls inside
HWNDs whose interior UIA had covered only partially (odbcad32's list
container chrome, Chrome's whole rendered document tree, msconfig's
non-client chrome). Where coverage was already complete the flag
stays off (ise, charmap). Cross-provider rect drift (MSAA inner
client rect vs UIA frame, ±4px per edge) dedupes by name+role so the
same control is not listed twice — measured on odbcad32's
`User Data Sources:` list.
