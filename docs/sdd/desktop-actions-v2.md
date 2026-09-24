# SDD: Desktop Actions v2

## Purpose

After routing/recovery v2 is green, add the desktop primitives real agents
need. Every capability enters through `ExecutionPlan`; unsupported adapters
fail honestly and no physical fallback is silent.

## Action surface

### Element invocation

```rust
Action::Invoke {
    target: Target,
    action: String,
}
```

The requested normalized action (`press`, `open`, `show_menu`, `raise`, etc.)
must still be advertised by the freshly resolved element. macOS maps known
normalized names to AX actions; browser maps supported actions to DOM/API;
unknown/custom names are unsupported, not passed through blindly.

### Text

- `TypeText` inserts/appends at the target.
- `SetValue` replaces the complete value.
- macOS uses read+set only for scalar editable values where append is safe.
  Rich text that cannot preserve content offers a physical route or reports
  unsupported; it never silently replaces the document.
- Clipboard/payload text is sensitive in audit.

### Keys

Menu elements gain an optional structured `shortcut: KeyChord`, collected from
`AXMenuItemCmdChar` and modifier attributes. A macOS `Key` request first plans
an accessibility press on a unique enabled menu item with that shortcut. CGEvent
is the last physical route. Browser key dispatch remains DOM/WebDriver scoped.

### App lifecycle

```rust
Action::LaunchApp { app: AppSelector, activate: bool }
Action::QuitApp { app: AppSelector }
```

- Launch accepts name or bundle id; PID launch is invalid.
- macOS launches with `/usr/bin/open` argument arrays, never a shell; quit uses
  normal `NSRunningApplication::terminate`.
- Force quit is not included in v2.
- Wait, Navigate, Launch and Quit check only the permissions they need; a
  missing AX grant does not block them.
- Effects are verified by app/process/window observation.

### Windows and browser tabs

```rust
Action::Window {
    window_id: Option<u32>,
    operation: WindowOperation,
}

pub enum WindowOperation {
    New,
    Focus,
    Raise,
    Close,
    Minimize,
    Restore,
    Move { x: f64, y: f64 },
    Resize { width: f64, height: f64 },
}
```

- V1 `Focus { Target::Window }` normalizes to v2 Focus.
- macOS uses AXRaise, close button, AXMinimized, AXPosition and AXSize after
  revalidating CGWindowID to AX window by bounds.
- Browser maps New/Focus/Close to existing WebDriver tab methods. Unsupported
  geometry operations return unsupported.

### Clipboard text

```rust
Action::ReadClipboardText
Action::WriteClipboardText { text: String }
```

- Text/UTF-8 only with a bounded payload.
- Read is `SensitiveRead`; write is `SensitiveWrite`. Embedded policy requires
  approval for both.
- Read content is returned only as immediate authorized `ActionResult` output.
  It is never journaled, fingerprinted in plaintext, exported for training or
  shown in the overlay.
- Unit tests use an isolated named pasteboard. Live tests preserve and restore
  the user's general pasteboard.

### Click count and drag/drop

```rust
Action::Click {
    target: Target,
    button: MouseButton,
    count: u8, // default 1, valid 1..=3
}

Action::Drag {
    from: Target,
    to: Target,
    duration_ms: u64,
}
```

- A double-click request on an element advertising `open` plans AXOpen first.
- Otherwise macOS click count/drag use CGEvent and are physical,
  foreground-required and coords-gated.
- Drag resolves both endpoints before policy and revalidates both before down.
- Browser uses W3C `/actions` against resolved DOM nodes; this does not move the
  user's OS cursor.
- CG drag emits down -> one or more dragged events -> up and releases the button
  on every error path.

## Candidate generation

Candidate generation only proposes new primitives when the observation proves
the affordance. `open` may yield Invoke/Open. Clipboard, app lifecycle, window
geometry and drag are not heuristically invented in this slice; host agents
can call them through generic `dexter_act` until labeled datasets justify
rules/models.

## Policy kinds

V2 adds `invoke`, `launch_app`, `quit_app`, `window`, `clipboard_read`,
`clipboard_write` and `drag`. First-match behavior and v1 action names remain.
Clipboard sensitivity and physical routes have independent policy floors.

## Adapter test matrix

- Sim: deterministic state/effect support for every action.
- Browser fake WebDriver: Invoke, key, New/Focus/Close, click count and W3C drag.
- macOS pure routing tests: semantic route ordering and physical gating without
  permissions.
- macOS live opt-in: controlled temp document/app/window; clipboard restored;
  frontmost app restored; no user files dragged.

## TDD contract

1. Finder-style element advertising `open` uses AXOpen, not a physical
   double-click.
2. TypeText appends and SetValue replaces in sim/browser/macOS-safe scalar
   paths.
3. Key shortcut prefers a unique enabled menu item; ambiguous shortcut fails
   closed.
4. Launch/quit work without AX permission and verify process state.
5. Window Focus/Close/Minimize/Restore verify observed state.
6. Clipboard requires approval and its sentinel never appears in audit.
7. Drag cannot execute if either target becomes stale.
8. Physical double-click/drag are denied without coords and independently
   policy-gated with coords.
