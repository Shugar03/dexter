# SDD — Linux driver skeleton

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
