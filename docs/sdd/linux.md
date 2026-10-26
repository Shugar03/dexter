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
- Screenshot and OCR behavior is specified in the X11 capture and
  Tesseract slice below.
- Linux action behavior is specified in the AT-SPI2 act slice below.

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

## AT-SPI2 act (Fase 7 slice)

Linux actions resolve semantic and focused targets from a fresh
app-scoped AT-SPI walk. `Target::Element` uses a bounded cache of the
last eight app-scoped observations, re-walks the same pid/window scope,
and checks the stored tree index with the shared
`dexter_driver::identity` rules; missing, moved, or changed elements
fail with `StaleReference`. Semantic ambiguity is preserved, and a
truncated walk keeps not-found non-definitive.

Left click uses `Action.DoAction` in the preference order click,
activate, press, toggle, jump. `SetValue` writes a finite numeric value
through `Value.CurrentValue`, otherwise uses writable
`EditableText.SetTextContents`; `TypeText` uses the same editable-text
path. Focus uses `Component.GrabFocus`. Disabled and read-only controls
fail before mutation. AT-SPI calls report their actual bool result;
missing interfaces and unsupported operations never simulate success.

## Linux Navigate (Fase 9 slice)

`Action::Navigate` is independent of AT-SPI, X11, and
`allow_coordinates`. The driver validates the input before spawning a
process: only case-insensitive `http`, `https`, and `mailto` schemes
with a non-empty remainder are allowed. Paths, empty input, and all
other schemes are refused as `Unsupported` with
`"refusing to open non-URL input — only http/https/mailto reach xdg-open"`.
Allowed input is passed as one argument to `xdg-open` without a shell.
Exit status 0 succeeds; a nonzero status fails; a missing `xdg-open`
returns `Unsupported`; other spawn errors are platform errors.
Navigation remains `Intrusiveness::Visual`; policy defaults are
unchanged.

## XTest physical input + AT-SPI focus/scroll (Fase 8 slice)

Physical input is opt-in through `ActContext.allow_coordinates`.
`Click(Point)`, `Key`, and untargeted `Scroll` remain
`Intrusiveness::Physical`, so the engine's existing physical policy
(`defaults.physical`) gates them; this driver adds no policy defaults or
capability flags. `TypeText` uses XTest only as a fallback when the
target has no `EditableText`, and is gated by `allow_coordinates` in the
same way as the Windows keyboard fallback. Semantic clicks and editable
text continue to use AT-SPI. `background_input` remains false.

The XTest backend refuses Wayland sessions: XTest through XWayland
cannot reach native Wayland windows. On X11, GTK with `GDK_SCALE > 1`
reports AT-SPI bounds in logical pixels while XTest expects root-window
device pixels. Coordinate clicks multiply observed points and element
centers by the target app's scale; a bare point without an app selector
uses scale 1.0. The scale is read from `/proc/<pid>/environ`: unset or
`1` means 1.0, and integer values 2 through 4 are supported. Invalid
values or unreadable environ fail closed with `Unsupported`. The
backend reports success only after the X server processes the queued
requests; X errors are platform errors, never simulated success.

- A point click moves to `(x, y)` and presses/releases button 1 (left),
  3 (right), or 2 (middle). A non-left semantic/element click uses the
  element bounds center and is unsupported when bounds are unavailable.
- Key chords use explicit canonical key names (`a`–`z`, `0`–`9`,
  `return`, `tab`, `escape`, `space`, `backspace`, `delete`, arrows,
  `home`, `end`, `pageup`, `pagedown`, `f1`–`f12`) and modifiers
  `cmd`/`ctrl`/`alt`/`shift`. Keysyms resolve through the current X keymap;
  unknown names or missing keycodes are unsupported without sending
  events. When an app scope is supplied, one of that pid's AT-SPI frames
  must be `ACTIVE` before keyboard input.
- The non-EditableText `TypeText` fallback first calls
  `Component.GrabFocus` and verifies `FOCUSED`, then checks app
  foreground. It pre-resolves every character against the current
  keymap before typing: Latin-1 keysyms are direct, other Unicode uses
  the X Unicode keysym form, and level 1 requires Shift. If any
  character is unavailable, the whole text is unsupported and nothing
  is typed. Keycodes are never rebound; characters absent from the
  current keymap fail closed.
- Untargeted scroll rounds each axis in 40-pixel notches. Positive
  `dy` uses wheel button 5, negative `dy` button 4; positive `dx` uses
  button 7, negative `dx` button 6. A zero-notch request succeeds
  without events.
- `Focus(Window)` resolves the AT-SPI frame by `window_id` (restricted
  to the requested app when present), then tries `Component.GrabFocus`.
  If it returns true and the frame becomes `ACTIVE` within about 500 ms,
  the action succeeds. Otherwise, on X11 with an EWMH-capable window
  manager, it matches a client window using the frame's PID, AT-SPI
  `Name`, and screen extents. Client geometry is compared both directly
  and after expansion by `_NET_FRAME_EXTENTS` (zeroes when absent); each
  rect field must match within ±8 px. It then requests
  `_NET_ACTIVE_WINDOW`. AT-SPI
  remains the source of window identity and activation verification;
  EWMH only requests activation. The frame must become `ACTIVE` within
  about 1 s for success. This fallback is needed because GTK3 frames can
  return false from `GrabFocus`. Wayland, no window manager, or no
  `_NET_ACTIVE_WINDOW` support is unsupported; no match is failed and
  multiple matches are ambiguous. Window activation is visual, moves no
  cursor, and is not gated by coordinates.
- Targeted scrolling resolves the element with the normal stale and
  ambiguity rules. It uses the target itself when it is a Value-capable
  scroll bar, otherwise its direct Value-capable scroll-bar children
  that are both `SHOWING` and `VISIBLE`. `VERTICAL` and `HORIZONTAL`
  states select axes. Before any write, all requested axes must have a
  bar; each value is clamped from
  `CurrentValue + delta` to `MinimumValue..=MaximumValue`. GTK
  adjustments use pixels; other toolkits may use different units.
  No bar/orientation is unsupported, values already at the requested
  limit fail with `"already at scroll limit"`, and success requires a
  changed `CurrentValue` after writing. `Component.ScrollTo` alone does
  not count because it only reveals content.

## X11 capture + Tesseract OCR (Fase 7 slice)

Linux screenshots use X11 `GetImage` on the root window and crop the
selected AT-SPI window frame. In a Wayland session, screenshots use the
session-bus `org.freedesktop.portal.Screenshot.Screenshot` API and wait
for `org.freedesktop.portal.Request.Response` on the expected request
handle path using the same D-Bus connection as the caller (the response
is unicast). XWayland clients retain the existing placement contract:
the client must be in `_NET_CLIENT_LIST`, have the target PID, and its
root-translated geometry must match the AT-SPI frame after applying the
target's `GDK_SCALE`. Multiple matches are ambiguous. Same-PID
candidates that do not match return `StaleReference` with the frame and
candidate rectangles.

Native Wayland placement is supported only under Sway and only when
`SWAYSOCK` is available. The driver reads the Sway tree over i3 IPC and
matches a visible `xdg_shell` window by PID, AT-SPI frame name, and
`window_rect` size (±1 logical pixel). The AT-SPI frame must start at
`(0, 0)` and Sway's client `geometry` offset must be `(0, 0)`. The
absolute content origin is `rect.x/y + window_rect.x/y`; that origin is
added to the frame and every descendant's AT-SPI bounds. In the measured
GTK3 setup, `GetExtents(SCREEN)` and `GetExtents(WINDOW)` are identical,
the frame starts at `(0, 0)`, and child extents are relative to the
content origin (for example, “Greet” is `(12, 38, 129, 34)`). Both Sway
and AT-SPI values are logical coordinates. A single active output with
integer scale 1–4 is required; the portal image is device-pixel sized
(for example, scale 2 yields a `3200×1800` image for a `1600×900` logical
output).

Native capture re-reads Sway placement and requires its origin to match
the already-translated window bounds within ±1. It accepts only
`GDK_SCALE == 1`, a portal image exactly matching the logical output
bounds multiplied by its scale, and a window fully contained by that
output. Other compositors, unset `SWAYSOCK`, nonzero frame origins,
client-side decoration offsets, stale or ambiguous name/size matches,
fractional scales, multiple active outputs, unverified GDK scale, image
dimension mismatches, and out-of-output windows fail closed. An
observation still succeeds when placement cannot be verified: its
original AT-SPI bounds are retained and no guessed offset is applied.

The existing XWayland root/image scale validation remains unchanged.
For XWayland, GTK AT-SPI extents are logical pixels when `GDK_SCALE > 1`;
capture reads the target app's scale from `/proc/<pid>/environ` and passes
it through `MonitorGeometry.scale` and `monitor_pixel_crop` to produce a
device-pixel crop. The portal image scale is multiplied by `GDK_SCALE`;
unset or `1` means 1.0, and integer values 2 through 4 are supported.
Invalid values or unreadable environ fail closed with `Unsupported`,
never a scale-1 fallback. Regions not fully covered by the image fail
closed.

The WLR portal backend may write to a fixed path such as `/tmp/out.png`.
The image's modification time must be at least the time immediately
before the request, preventing a stale file from being accepted as the
current capture. The portal file is removed after successful decoding
and after decode errors. A missing Screenshot interface/backend maps to
`Unsupported`; a denied or cancelled response maps to
`PermissionDenied`; other portal errors map to `Platform`; and a missing
response within ten seconds maps to `Timeout`.

`capabilities().screenshots` is true in Wayland when either XWayland is
reachable with a non-empty root or Sway IPC reports one valid active
output, and the portal Screenshot interface's `version` property is
readable. On X11 it remains true only when an X connection reports a
non-empty root. Only 24/32-bit TrueColor pixel layouts supported by the
X11 capture implementation are accepted; other formats return
`Unsupported`. XTest input and EWMH activation
remain refused on Wayland, including for native Wayland targets. There is
no compositor-agnostic, permissionless native input mechanism; XTest
through XWayland cannot reach native windows. Semantic AT-SPI actions
remain available. wlroots virtual-pointer/keyboard protocols and
Sway-IPC focus are possible future, explicitly unimplemented input paths.

The Linux `dexter-vision` provider invokes the local `tesseract` CLI
(`--psm 11`, TSV output, default English). It is available only when
`tesseract --version` exits successfully; `DEXTER_TESSERACT_LANG`
selects an alternate language. A missing provider degrades a requested
vision pass through `collection_errors`. OCR tokens become inert,
evidence-only elements with `source: Ocr`; they are not actionable
accessibility targets.
