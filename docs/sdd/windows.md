# SDD — Windows driver (UIA observe slice)

## Intent

`dexter-windows` is the Windows side of the driver seam. The UIA
observe backend is real on `windows`: `windows()` enumerates top-level
windows (Win32 `EnumWindows`), `observe()` resolves the `AppSelector`
to a pid, anchors each of its windows via `IUIAutomation::ElementFromHandle`,
and walks the ControlView tree into normalized `Element`s through
`uia_role()`. Actions remain `Unsupported` — observe is the honest
slice. Off Windows the crate keeps the skeleton contract: every claim
stays false, every operation declines.

## Contract

- `capabilities()`: `element_tree` is `cfg!(windows)` — UIA needs no
  permission grant, so the honest flag is the platform. `screenshots`
  and `background_input` stay `false` in this slice.
- `windows()` returns every top-level `HWND` as a `Window`: `id` is the
  HWND truncated to `u32` (HWNDs are 32-bit in practice), `app` the
  process image stem (`notepad.exe` → `notepad`), `bundle_id` the
  AppUserModelId for packaged apps, `on_screen` = visible and not
  minimized. Nothing is guessed: no AUMID means `None`.
- `observe(scope)`:
  - `AppSelector::Pid` scopes directly; `Name` matches the windowed
    process image stem/`Name` case-insensitively; `BundleId` matches
    AUMID — both through `unique_app_pid`, so several running instances
    fail closed as `Ambiguous`.
  - `scope.window` selects one `HWND` subtree (`NotFound` when the id
    is not a window of the app) — natively scoped, so callers' bounds
    post-filter is a no-op.
  - Elements: `CurrentControlType` → `control_type_name` → `uia_role`
    for `role`; the ControlType programmatic name is `raw_role`;
    `CurrentName` → `name`; `CurrentBoundingRectangle` → `bounds`
    (empty rect → `None`); `CurrentAutomationId` → `identifier`;
    `CurrentIsEnabled`/`CurrentHasKeyboardFocus` → `enabled`/`focused`;
    supported patterns → `actions` (`Invoke`/`Toggle`/`SelectionItem`
    → `press`, writable `Value` → `set_value`, `ExpandCollapse`,
    `Scroll`, `ScrollItem` → `scroll_into_view`, keyboard-focusable →
    `focus`); `ValuePattern`/`TogglePattern` state → `value`.
  - `CurrentIsPassword` elements never leak `value` — redacted at
    collection time, same rule as AX secure fields and DOM
    `type=password`.
  - Per-element read failures count into `collection_errors`; hitting
    `max_depth`/`max_elements` sets `elements_truncated`.
  - `ax_limited` when the app reports windows but UIA produced no
    elements — the honest "can't see" signature (elevated app, dead
    provider), degrading absence-dependent verification to UNCERTAIN.
  - `scope.screenshot`/`scope.vision` return `Unsupported`: the flags
    stay false in capabilities, and an ignored request is simulated
    success.
- `act()` stays `Unsupported` — no simulated input.
- UIA's built-in MSAA bridge already surfaces most legacy `IAccessible`
  content as ControlTypes; a dedicated MSAA fallback pass remains open
  work alongside the act slice.

## Why UIA first

- UIA is the one API with no permission gate — the `element_tree`
  capability is a fact, not a grant.
- Anchoring trees on real `HWND`s keeps `obs.windows` and
  `obs.elements` consistent: every element root is a listed window.
- COM is initialized per call (`CoInitializeEx`, MTA) and torn down —
  `WindowsDriver` stays `Send + Sync` with no COM state held.
- No CLI wiring: `dexter-cu` is still macOS-only; the crate's seam is
  exercised by tests and future Windows binaries.
