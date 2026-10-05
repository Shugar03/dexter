# SDD — Linux driver: skeleton + AT-SPI2 observe

## Intent

`dexter-linux` establishes the Linux side of the driver seam before
any backend exists: a `LinuxDriver` that implements `ComputerDriver`
by *declining* everything honestly, plus the AT-SPI role →
normalized-role tables the real AT-SPI2 backend (X11 first) will use.
Mirrors the original `dexter-windows` skeleton (PR #13).

## Contract

- `capabilities()` reports `name: "linux"`, `element_tree: false,
  screenshots: false, background_input: false`. A skeleton that claims
  capabilities is simulating; false flags are the honest state.
- `windows()`, `observe()`, `act()` return
  `DriverError::Unsupported("linux AT-SPI backend not implemented —
  skeleton only")`. `wake()` keeps the trait default
  (`activated: false`) — nothing was activated.
- `atspi_role_name(id)` maps `AtspiRole` ids — the `u32`
  `org.a11y.atspi.Accessible.GetRole` returns, stable constants from
  `atspi-constants.h` — to the canonical name `atspi_role_get_name`
  yields (`7` → `"check box"`, `43` → `"button"`). Ids
  `>= ATSPI_ROLE_LAST_DEFINED` (131) → `None`. The backend reads the
  numeric role: `GetRoleName` strings are toolkit-reported (ATK still
  says `"push button"`), the enum is the protocol.
- `atspi_role(name)` maps canonical names (plus the `"push button"`
  ATK alias) to the shared role vocabulary (`button`, `text_field`,
  `check_box`, `menu_item`, `tab`, `dialog`, …) — the same names the
  AX, DOM and UIA walkers emit, so `SemanticTarget` matching stays
  platform-agnostic. Toggle buttons and switches are `check_box` (AX
  convention); `password text` is `secure_text_field`, so the backend
  inherits the value-redaction rule for sensitive roles. Roles with no
  semantic affordance (`invalid`, `unknown`, `canvas`, `separator`,
  `terminal`, …) → `None`; unmapped elements keep `raw_role`, they
  don't wear guesses. Matching is exact — `"Push Button"` is not a
  canonical name.

## Why a skeleton instead of nothing

- The workspace member, dependency edges and test harness exist now;
  the AT-SPI2 observe slice (roadmap Fase 6) fills
  `windows`/`observe` behind `#[cfg(target_os = "linux")]` and flips
  `element_tree` — no architectural churn later.
- The role tables are decidable today: they are protocol data, not
  platform API, and their coverage is testable everywhere.
- No platform dependencies yet: the crate builds on every host, so the
  macOS CI job type-checks and tests it like any other member.
- No CLI wiring: `dexter-cu` is macOS-only; a driver that only knows
  how to say `Unsupported` doesn't belong in a shipped binary.

## AT-SPI2 observe (Fase 6 slice, X11)

Fills `windows()`/`observe()` behind `#[cfg(target_os = "linux")]`
(`drivers/linux/src/bus.rs`) over blocking `zbus`; the protocol
decoding that needs no bus (`drivers/linux/src/atspi.rs`) is built and
tested on every host. Mirrors the Windows UIA observe slice.

### Transport

- One short-lived connection per call: session bus →
  `org.a11y.Bus.GetAddress` → the a11y bus. No session bus or no
  `at-spi-bus-launcher` → `DriverError::Platform`, and
  `capabilities().element_tree` is exactly that probe succeeding — a
  headless host reports `false`, never a claim that would fail.
- Properties are read uncached (`CacheProperties::No`): zbus's lazy
  cache registers a `PropertiesChanged` match rule per proxy, which is
  thousands of `AddMatch` calls per walk. `GetAll` on
  `org.a11y.atspi.Accessible` fetches `Name`, `Description`,
  `AccessibleId` and `ChildCount` in one round trip.

### Windows

- Applications are the registry root's children
  (`org.a11y.atspi.Registry`, `/org/a11y/atspi/accessible/root`);
  the pid is `org.freedesktop.DBus.GetConnectionUnixProcessID` of the
  owning unique name — bus truth, not a guess. `Window::app` is the
  root's `Name`, falling back to `/proc/<pid>/comm`.
- Frames are the application root's children that have a screen
  extent (`Component.GetExtents(SCREEN)` with positive size and not
  GTK's `(G_MININT, G_MININT, 1, 1)` unrealized sentinel). Hidden or
  unrealized frames aren't windows a user could see, so they aren't
  listed. `on_screen` is `SHOWING && VISIBLE && !ICONIFIED`;
  `bundle_id` is `None` (Linux has none to claim); `layer` is 0.
- `Window::id` is FNV-1a over `unique bus name + object path`
  (`atspi::window_id`), nonzero, stable for the application's life and
  reproducible across our own processes — the same id a later
  observation's `scope.window` will name.

### Observe

- Unscoped: windows only, no tree walk (same as every driver).
- `AppSelector::Pid` must be a registered application, else
  `AppNotFound`. `Name` matches the registered application name or the
  process `comm` case-insensitively; two pids → `Ambiguous`, none →
  `AppNotFound`. `BundleId` → `Unsupported`, never mapped by heuristics.
- `scope.window` is honored natively: not one of the app's frames →
  `NotFound`; otherwise the walk is that one frame.
- Walk: per frame, depth-first over `Accessible.GetChildren`, respecting
  `max_depth`; `max_elements` sets `elements_truncated` and stops the
  walk. Element ids are walk-ordinal (1-based), `parent`/`depth` follow
  the AT-SPI tree, `source: Accessibility`.
- Nodes that are not `SHOWING && VISIBLE` are skipped with their
  subtree (hidden notebook pages, unmapped dialogs — invisible controls
  are not targets). `DEFUNCT` nodes count as a collection error and
  stop. Any failed read on a node — the object vanished
  (`UnknownObject`), the app exited (`ServiceUnknown`), a property
  refused — increments `collection_errors` once and the walk continues
  with what it has.
- Per element: `role` via `atspi_role_name(GetRole)` → `atspi_role`
  (the numeric enum, not toolkit `GetRoleName` strings); `name` =
  `Name` else `Description`; `identifier` = `AccessibleId`; `bounds`
  via `extents_rect` when the node implements `Component`; `enabled` =
  `ENABLED && SENSITIVE`; `focused` = `FOCUSED`.
- `value`: toggle roles (`check box`, `radio button`, `toggle button`,
  `switch`, check/radio menu items) report `on`/`off`/`indeterminate`
  from `CHECKED`/`PRESSED`/`INDETERMINATE`; otherwise
  `Value.CurrentValue` when the node implements `Value`, else
  `Text.GetText` for editable text / text fields, capped at 500 chars
  (longer → `None`). `password text` never reports a value.
- `actions`: `Action.GetName(i)` verbs normalized by
  `atspi::action_name` into the shared vocabulary (`press`,
  `expand_collapse`, `show_menu`; unknown verbs dropped, not guessed);
  `set_value` for editable, non-read-only nodes; `focus` for
  `FOCUSABLE`.
- `ax_limited` = windows exist but the walk produced nothing (bridge
  not loaded in that process, every frame refused) — `not found`
  against the observation is not definitive, and verification degrades
  to UNCERTAIN as everywhere else.
- `scope.screenshot` → `Unsupported` (no capture backend; dropping the
  request silently would be a simulated success). `scope.vision` has no
  provider: the request adds one `collection_errors` so the degraded
  perception is visible.
- `act()` stays `Unsupported` — the next slice.

### Verification

`drivers/linux/tests/atspi.rs` (every host): state-word decoding,
enabled/showing/on-screen rules, extent sentinel, action and toggle
normalization, window-id stability, name matching.
`drivers/linux/tests/observe.rs` (Linux only): against a live a11y bus
with a registered GTK app (`at-spi-bus-launcher` +
`gtk3-widget-factory`) — real pids/ids, one root per window, parent
depth arithmetic, roles and `press` actions present, native window
scope accepted by `world_model::scope_to_window`, `max_elements`
truncation explicit, unknown app/pid/window fail closed, bundle ids
unsupported. Without a bus the live tests skip on stderr rather than
asserting against nothing; CI (macOS) compiles the crate off-Linux and
keeps the skeleton contract tests.
