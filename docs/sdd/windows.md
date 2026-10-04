# SDD — Windows driver (UIA observe + act + capture/OCR slices)

## Intent

`dexter-windows` is the Windows side of the driver seam. On `windows`
the UIA backend is real: `windows()` enumerates top-level windows
(Win32 `EnumWindows`), `observe()` resolves the `AppSelector` to a pid,
anchors each of its windows via `IUIAutomation::ElementFromHandle`,
and walks the ControlView tree into normalized `Element`s through
`uia_role()`. `act()` is semantic-first: UIA patterns carry every
control mutation, and `SendInput` is reachable only behind
`ctx.allow_coordinates` (plus a foreground check where keystrokes would
otherwise land in the wrong app). Perception beyond UIA mirrors the
macOS slice: `scope.screenshot` captures the target window to PNG via
GDI per-display crops, and `scope.vision` appends inert `source: ocr`
elements from the on-device `Windows.Media.Ocr` engine. Off Windows
the crate keeps the skeleton contract: every claim stays false, every
operation declines.

## Contract

- `capabilities()`: `element_tree` is `cfg!(windows)` — UIA needs no
  permission grant, so the honest flag is the platform. `screenshots`
  is `capture::available()`: the exact probes the capture path runs
  (a display device context exists and at least one monitor reports a
  measurable raster), so the flag is only true where a capture would
  succeed — session-0 or displayless hosts report `false`.
  `background_input` stays `false` (SendInput always targets the
  foreground — there is no background input path).
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
    `type=password`. `act()` never echoes a value into `detail` either.
  - Per-element read failures count into `collection_errors`; hitting
    `max_depth`/`max_elements` sets `elements_truncated`.
  - `ax_limited` when the app reports windows but UIA produced no
    elements — the honest "can't see" signature (elevated app, dead
    provider), degrading absence-dependent verification to UNCERTAIN.
  - `scope.screenshot` captures the capture window — `scope.window`'s
    HWND when set, else `pick_capture_window` (largest on-screen
    layer-0 window of the pid) — to `screenshot_path` or a temp file,
    exposed as `obs.screenshot`. The captured region is the window's
    *visible* frame: `DWMWA_EXTENDED_FRAME_BOUNDS` when the attribute
    reads (the `GetWindowRect` bounds carry invisible resize borders
    that would spill off-monitor on edge-snapped windows), the listed
    bounds otherwise. The capture is a per-display crop: monitors are
    `EnumDisplayMonitors` + `GetMonitorInfoW` mapped through
    `geometry::monitor_geometry` (scale = physical raster px per
    logical unit, from `EnumDisplaySettingsW` `dmPelsWidth/Height` over
    `rcMonitor` — the same number `GetDpiForMonitor/96` reports where
    Win32 does not virtualize coordinates, and still correct where it
    does), the monitor chosen by `dexter_vision::capture_monitor`, the
    monitor rasterized at physical size (`CreateDCW("DISPLAY")` +
    `StretchBlt`) and cropped by `dexter_vision::monitor_pixel_crop`.
    Spanning, off-screen or degenerate regions fail closed
    (`DriverError`, never a partial or guessed image), and a failed
    capture fails the request — an ignored screenshot is simulated
    success.
  - `scope.vision` mirrors the macOS augment exactly: opt-in only,
    fires only when `ax_limited || elements.is_empty() ||
    scope.window.is_some()`, appends `source: ocr` elements via
    `dexter_vision::tokens_to_elements` mapped over the rect actually
    captured, and a failed pass degrades (`collection_errors += 1`)
    rather than fails. The provider is `Windows.Media.Ocr` (WinRT,
    on-device, no downloads) via `dexter_vision::platform_provider()`;
    a box with no recognizer language pack degrades through
    `VisionError::Unsupported` — never fabricated tokens. `OcrWord`
    boxes (top-left origin pixels) map to `NormRect` through
    `dexter_vision::ocr_word_rect`; WinRT reports no per-word
    confidence, so tokens carry `NaN` — the honest "not measured",
    not an invented score.
  - A successful app-scoped observe stores `(ObservationId, pid,
    scope.window, elements)` in the driver's `ObsCache` (plain
    `Element` data — `WindowsDriver` stays `Send + Sync`, COM is
    initialized per call and never stored), bounded to the last 4.
- `act(action, ctx)`:
  - `Target::Element { observation, element }` resolves through the
    `ObsCache`: a missing observation → `StaleReference`; the stored
    element is then verified against a *fresh* walk of the same
    app/window scope — the element at the same index must still match
    (role, name, parent, depth, bounds ±2px) or the reference is
    stale. Never act on the stored snapshot itself.
  - `Target::Semantic` resolves over a fresh walk of the app's windows
    through `dexter_world_model::resolve_element` — `Ambiguous` when
    several match, `NotFound` when none (flagged "not definitive" when
    the walk truncated).
  - `Target::Focused` → `IUIAutomation::GetFocusedElement`, refused
    (`NotFound`) when the focused element belongs to a foreign pid —
    never act on another app.
  - Mutating element acts (`Click`, `TypeText`, `SetValue`) fail with
    `FAILED` when `CurrentIsEnabled` is false — the browser driver's
    disabled guard, same rule: never simulate success on a disabled
    control. `Focus`/`Scroll` are view operations and are not gated.
  - Pattern ladder for a left `Click`: `InvokePattern.Invoke` →
    `TogglePattern.Toggle` → `SelectionItemPattern.Select` →
    `ExpandCollapse` (`Expand` unless `Expanded`, then `Collapse`) →
    `LegacyIAccessiblePattern.DoDefaultAction`; none present →
    `UNSUPPORTED`. Right/middle clicks have no pattern: `UNSUPPORTED`
    with `Mechanism::Accessibility`, or a `SendInput` click at the
    element's bounds center when `ctx.allow_coordinates`.
  - `SetValue`/`TypeText` write through `ValuePattern.SetValue` when
    `!CurrentIsReadOnly`; read-only reports `FAILED`, a missing pattern
    `UNSUPPORTED`. `TypeText` without a writable `Value` falls back to
    `SendInput` unicode keystrokes — only with `ctx.allow_coordinates`
    (physical input is the coordinates path, same rule as macOS), after
    `SetFocus` re-verifies the element took focus, and only while the
    element's own pid owns the foreground window; otherwise
    `FAILED`/`FOREGROUND_REQUIRED`.
  - `Focus` → `IUIAutomationElement.SetFocus` on element targets;
    `Target::Window` → `SetForegroundWindow` (a `FALSE` return reports
    `FAILED` — Windows refuses the raise, we do not pretend it
    happened); `Target::Point` → `UNSUPPORTED`.
  - `Scroll` with a target → `ScrollItemPattern.ScrollIntoView` first,
    else `ScrollPattern.Scroll` with small/large increments by |delta|
    (positive dy scrolls down/right, negative up/left); without a
    target → `SendInput` wheel notches, `ctx.allow_coordinates` only.
  - `Key` → `SendInput` virtual-key chord (`keymap` name → `VK_*`;
    `cmd` is the Windows key): `ctx.allow_coordinates` required, and a
    scoped app must own the foreground window or the verdict is
    `FOREGROUND_REQUIRED`.
  - `Click { Target::Point }` → `SendInput` only with
    `ctx.allow_coordinates`, else `UNSUPPORTED` with
    `Mechanism::Coordinates` (same gate as macOS).
  - `Navigate { url }` → `ShellExecuteW("open", url)` restricted to
    `http`/`https`/`mailto` schemes: `open` on a bare path or an
    exotic scheme executes programs, so non-URL input is
    `UNSUPPORTED` — fail-closed by allowlist.
  - `Wait` sleeps; `Observe` stays an engine directive → `Unsupported`.
  - `Mechanism` is honest per call: `Accessibility` for UIA patterns,
    `Coordinates` for `SendInput`, `NativeAutomation` for
    `SetForegroundWindow`/`ShellExecuteW`/`Wait`. `SUCCESS` is returned
    only when the underlying call returned success (`S_OK`, nonzero
    BOOL, `ShellExecuteW` > 32).
- UIA's built-in MSAA bridge already surfaces most legacy `IAccessible`
  content as ControlTypes; a dedicated MSAA fallback pass remains open
  work.

## Why UIA first

- UIA is the one API with no permission gate — the `element_tree`
  capability is a fact, not a grant.
- Anchoring trees on real `HWND`s keeps `obs.windows` and
  `obs.elements` consistent: every element root is a listed window.
- COM is initialized per call (`CoInitializeEx`, MTA) and torn down —
  `WindowsDriver` stays `Send + Sync` with no COM state held. The
  observe lock serializes UIA client init for `act` too: a live walk
  for an action does the same `CoCreateInstance` + walker setup that
  races when concurrent.
- No CLI wiring: `dexter-cu` is still macOS-only; the crate's seam is
  exercised by tests and future Windows binaries.
