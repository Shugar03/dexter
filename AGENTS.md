# Working on Dexter

## Build / test / lint

```sh
cargo build --workspace          # build everything
cargo test --workspace           # all tests are hermetic — no macOS
                                 # permissions or UI state needed
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

## Layout

`crates/` — runtime libraries (core types, driver seam, world-model,
policy, verify, decision, laya, engine, mcp). `drivers/` — platform
drivers (macos real, sim synthetic). `apps/dexter` — CLI + MCP entry.
`workers/laya` — Python NDJSON sidecar. `docs/` — specs; `docs/sdd/` —
per-slice design contracts. `examples/` — scenario TOMLs.

## Invariants (do not break)

- Never simulate success — results reflect what the driver did.
- Semantic targets preferred; coordinates require explicit opt-in.
- Ambiguous/stale targets fail closed.
- Incomplete perception (`elements_truncated`, `ax_limited`) degrades
  absence-dependent verification to UNCERTAIN.
- Policy is evaluated outside any model; approvals are fingerprint-bound,
  single-use, TTL-limited.
- CLI, MCP and future SDKs share `Engine::run_step` — don't fork the
  execution path.
- `Target` serde is untagged: structurally constrained variants must
  precede the catch-all `Semantic` (see core/src/target.rs).

## Workflow

Changes are built as vertical slices: SDD note in `docs/sdd/` → failing
test at the seam → minimal implementation → green → clippy → commit.
