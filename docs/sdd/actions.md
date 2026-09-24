# SDD: Semantic Actions (macOS)

## Contract

> V2 note: `act` below is the deprecated v1 seam, kept one release for
> unmigrated drivers. The engine calls `plan`/`execute` —
> `docs/sdd/execution-routing-v2.md` is the normative contract; this
> table still documents the mechanism ladder each macOS route plans.

`ComputerDriver::act(&Action, &ActContext) -> Result<ActionResult, DriverError>`

- `ActionResult.status.ok()` is the only success signal; non-success
  statuses (`UNSUPPORTED`, `FOREGROUND_REQUIRED`, `PERMISSION_DENIED`) are
  returned as data, not simulated.
- `DriverError` is for failures that prevented a verdict: app not found,
  ambiguous resolution, stale reference, platform error.

## Mechanism ladder (per action)

| Action   | First choice              | Fallback                              | Notes |
|----------|---------------------------|----------------------------------------|-------|
| Click    | `AXPress` (left) / `AXShowMenu` (right,middle) | CGEvent mouse at `point:` **only** with `allow_coordinates` | `window:` targets unimplemented |
| TypeText | `AXValue` set when `is_settable` | CGEvent unicode typing — needs `allow_coordinates` **and** app frontmost | never types into a background app |
| Key      | —                         | CGEvent key chord; needs `allow_coordinates`; refuses if scoped app not frontmost | |
| Scroll   | `AXScrollToVisible` on target | CGEvent scroll wheel without target — needs `allow_coordinates` | |
| Focus    | `AXFocused=true`          | —                                      | |
| SetValue | `AXValue` set             | —                                      | |
| Wait     | thread sleep              | —                                      | |

`Mechanism::Accessibility` and `Mechanism::Coordinates` are reported
honestly in every `ActionResult`.

## Target resolution

- `Semantic` → fresh AX walk + `world_model::resolve_element` (fail-closed
  on ambiguity; `NotFound` flagged when the walk was truncated).
- `Element { observation, element }` → the driver keeps only *data* from
  the last 4 observations (`ObsCache`, no AX pointers — they are `!Send`).
  `act` re-walks the tree fresh and verifies the element at the same
  index still matches (role, name, parent, depth, bounds ±2px). Any
  deviation → `StaleReference`. Never act through a stored pointer.
- `Focused` → `AXFocusedUIElement` of the scoped app.
- `Point` → CGEvent path only; rejected without `allow_coordinates`.

## Degraded AX detection

macOS can answer `AXIsProcessTrusted()==true` (via the responsible
process) yet serve a *degraded tree* to an unsigned/adhoc binary:
`AXWindows` returns a single `AXApplication` proxy and no real window
content. Detected and reported:

- `collect()` only treats `AXWindow`-family roles as window roots; when
  none are found it falls back to the app's `AXChildren` (menu bar still
  reachable).
- `Observation.ax_limited` is set when CGWindowList reports layer-0
  windows for the pid but the AX tree contains only application/menu
  machinery. `not found` results are non-definitive when set.
- `AXApplication` children are recorded but never descended — they are
  self-cycle boundaries (observed on macOS 26: every app's children
  contain the app itself).

## Verified (this environment, degraded tree)

- `AXPress` on `menu_item` → real effect confirmed via `on_screen`.
- Ambiguity fail-closed; stale rejection; `--coords` gating.
- Paths not smoke-testable under a degraded grant: `AXValue` typing,
  `AXScrollToVisible`, `AXFocused`, CGEvent paths — implemented, pending
  a machine where the binary itself holds the AX grant.
