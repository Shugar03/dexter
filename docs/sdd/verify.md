# SDD: Verifier (`dexter-verify`)

## Contract

`verify(&Observation, &ExpectedState) -> Verification`

Returns `VERIFIED` / `FAILED` / `UNCERTAIN` with per-check detail lines.
`UNCERTAIN` is never collapsible to success — the engine treats it as
"keep trying or escalate", never as done.

## Completeness rule

A verdict that depends on the *completeness* of the element tree must
degrade to `UNCERTAIN` when `obs.elements_truncated` or `obs.ax_limited`
is set:

- `ElementExists`: matches>0 → VERIFIED; 0 → FAILED, or UNCERTAIN when
  the tree is partial.
- `ElementAbsent`: matches>0 → FAILED; 0 → VERIFIED, or UNCERTAIN when
  partial.
- `ElementValue`: some match satisfies predicate → VERIFIED; matches
  exist but none satisfies → FAILED, or UNCERTAIN when the tree is
  partial (the satisfying element may sit outside the walked subtree);
  0 matches → FAILED/UNCERTAIN when partial. A predicate over a
  redacted (secure-field) value is UNCERTAIN `redacted_value`, never
  FAILED — the runtime cannot see the value it would be judging.
- `TextPresent`: digest is a rendering of the same partial data — the
  presence check is definitive for what was collected, so partial trees
  → UNCERTAIN on absence only.
- `WindowTitleContains`: if *every* window title is `None` (no
  screen-recording grant), the check is UNCERTAIN.
- `AppRunning`: window list is complete from the window server —
  definitive.
- `All`/`Any`/`Not`: standard three-valued logic (VERIFIED ∧ …,
  short-circuit FAILED; NOT swaps VERIFIED/FAILED, keeps UNCERTAIN).
