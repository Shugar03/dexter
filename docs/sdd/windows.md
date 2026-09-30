# SDD — Windows driver skeleton

## Intent

`dexter-windows` establishes the Windows side of the driver seam
before any backend exists: a `WindowsDriver` that implements
`ComputerDriver` by *declining* everything honestly, plus the
UIA ControlType → normalized-role table the real backend will use.

## Contract

- `capabilities()` reports `element_tree: false, screenshots: false,
  background_input: false`. A skeleton that claims capabilities is
  simulating; false flags are the honest state.
- `windows()`, `observe()`, `act()` return
  `DriverError::Unsupported("windows UIA backend not implemented —
  skeleton only")`. The engine treats `Unsupported` as a real
  outcome — nothing downstream needs to know the backend is absent.
- `uia_role(control_type)` maps `IUIAutomationElement`
  `CurrentControlType` names to the shared role vocabulary
  (`button`, `text_field`, `check_box`, `menu_item`, `tab`, …) — the
  same names the AX walker and DOM walker emit, so `SemanticTarget`
  matching stays platform-agnostic. Unknown control types → `None`;
  unmapped elements keep `raw_role`, they don't wear guesses.

## Why a skeleton instead of nothing

- The workspace member, dependency edges and test harness exist now;
  the real UIA backend fills `observe`/`act` behind
  `#[cfg(target_os = "windows")]` and flips flags — no architectural
  churn later.
- The role map is decidable today: it is data, not platform API, and
  its coverage is testable cross-platform.
- No CLI wiring: `dexter-cu` is macOS-only by design; a driver that
  only knows how to say `Unsupported` doesn't belong in a shipped
  binary yet. The seam is where the work happens.
