# SDD: Execution Routing v2

## Purpose

Policy must authorize the mechanism Dexter will actually execute. Drivers may
offer semantic and physical routes, but they may not choose a more intrusive
fallback after policy has run.

## Interface

The execution seam remains `ComputerDriver`; no pass-through router crate is
introduced.

```rust
pub enum Sensitivity {
    Standard,
    Secrets,      // secure/password fields, clipboard — never journaled
    Destructive,  // irreversible operations (delete, purchase, send)
}

pub struct TargetDescriptor {
    pub role: Option<String>,
    pub name: Option<String>,
    pub identifier: Option<String>,
    // Element id within `observation`, when resolved to a live element.
    pub element: Option<ElementId>,
    // Observation the element id belongs to — staleness is checkable.
    pub observation: Option<ObservationId>,
    pub window_id: Option<u32>,
    // Raw coordinate target, when the route is a point.
    pub point: Option<Point>,
    // The route targets whatever element holds focus.
    pub focused: bool,
}

pub struct ExecutionRoute {
    pub action: Action,
    // Resolved target identity — what policy matches and fingerprints bind.
    pub target: TargetDescriptor,
    // `Some` is enforced against `ActionResult.mechanism`; `None` is the
    // legacy escape for unmigrated drivers (policy still gates on
    // intrusiveness and the mechanism is whatever `act` reports).
    pub mechanism: Option<Mechanism>,
    pub intrusiveness: Intrusiveness,
    pub sensitivity: Sensitivity,
    pub requires_foreground: bool,
}

pub struct ExecutionPlan {
    pub requested: Action,
    pub routes: Vec<ExecutionRoute>,
}

pub trait ComputerDriver: Send + Sync {
    fn capabilities(&self) -> DriverCapabilities;
    fn windows(&self) -> Result<Vec<Window>, DriverError>;
    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError>;
    fn plan(&self, action: &Action, ctx: &ActContext)
        -> Result<ExecutionPlan, DriverError>;
    fn execute(&self, route: &ExecutionRoute, ctx: &ActContext)
        -> Result<ActionResult, DriverError>;
    fn wake(&self, app: &AppSelector) -> Result<WakeHandle, DriverError>;
    fn restore(&self, handle: &WakeHandle);
}
```

`act(action, ctx)` remains as a deprecated v1 convenience for one release. It
plans and executes only the first route. Engine v2 never calls it.

## Planning contract

- `plan` is read-only. It may observe and populate bounded caches, but it may
  not wake an app, move focus, inject input, mutate the clipboard, launch or
  terminate anything.
- Routes are ordered highest fidelity first: API, DOM, Accessibility, native
  automation, vision-derived point, coordinates.
- A route contains the concrete action that will execute. A semantic request
  is narrowed to an observation-bound element whenever possible.
- A route that cannot be executed without foreground declares
  `requires_foreground`; it does not foreground the app itself.
- If planning cannot inspect a hidden macOS window, it returns
  `DriverError::ForegroundRequired`. Engine represents wake as a separate
  visual route, authorizes it, wakes once, re-plans and then authorizes the
  concrete route. No side effect occurs before policy.
- Routes using a physical mechanism are present only when the operator enabled
  coordinate input. Their presence is not authorization: policy still gates
  each route.
- An empty route list is `Unsupported`, never simulated success.

## Execution contract

- `execute` receives exactly one authorized route.
- It re-resolves and validates the target immediately before the side effect.
- It verifies that the selected mechanism still applies. If it no longer does,
  execution fails stale/unsupported; it never selects another route.
- `ActionResult.mechanism` must equal `ExecutionRoute.mechanism`.
- `ActionResult.element` is set when a concrete element was acted upon.
- The engine consumes a single-use approval immediately before one execute.
  Verification polls do not consume approvals; a second execute does.

## Policy input

Policy evaluates action kind, app, planned mechanism, planned intrusiveness,
sensitivity and structured target metadata. Free-form target hints and model
rationales are audit text only and cannot authorize anything.

Rules remain first-match-wins. Existing v1 fields continue to parse. V2 adds:

```toml
[[rule]]
action = "click"
app = "bundle:com.apple.TextEdit"
mechanism = "accessibility"
intrusiveness = "background"
decision = "allow"

[rule.target]
role = "button"
name = "Save"
```

The approval key canonically binds action kind, every non-secret action
parameter (chord, url digest, app selector, window op, button, count, invoke
name, scroll delta, drag destination/duration, wait), app, mechanism,
intrusiveness, sensitivity, stable target identity and a hash of sensitive
payload. Observation ids and screen coordinates derived from semantic targets
are excluded because they are ephemeral; explicit point coordinates remain.

## Driver route matrix

- Sim: API routes.
- Browser: DOM/API routes; DOM key dispatch is background, W3C pointer actions
  are browser-scoped and do not move the user's OS cursor.
- macOS: AX action/value/focus routes first; native app/window operations next;
  CGEvent routes last and physical.

## Compatibility

- V1 `Action::intrusiveness()` remains as a conservative/deprecated hint for
  callers, but policy and Engine v2 use the route tier.
- Existing drivers keep `act` during one minor release.
- Existing action JSON remains accepted; new fields are additive or have v1
  defaults.

## TDD contract

1. Targeted text whose AX value is not settable plans a physical route, so it
   cannot pass a background-only policy.
2. Browser key dispatch plans DOM/background without `--coords`.
3. Execute cannot switch from AX to coordinates after AX failure.
4. A stale concrete target fails before side effect.
5. A policy target rule distinguishes two same-app actions by structured
   metadata.
6. One approval permits one execute only.
