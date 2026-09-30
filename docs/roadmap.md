# Dexter roadmap

Single source of truth for autonomous work. Each item is a vertical
slice per AGENTS.md: SDD note (if architectural) → failing test at the
seam → minimal implementation → `cargo test` + `cargo clippy
--workspace --all-targets --all-features -- -D warnings` + `cargo fmt
--all` → PR → merge on green CI → check the item off here with the PR
link and date.

On Linux, `dexter-macos`, `dexter-cu` and `dexter-overlay` do not
compile — exclude them from every cargo command:
`--exclude dexter-macos --exclude dexter-cu --exclude dexter-overlay`.

Never break the AGENTS.md invariants: fail-closed, semantic targets
first (coords opt-in), ambiguous/stale targets fail closed, incomplete
perception degrades to UNCERTAIN, policy outside the model, single
`Engine::run_step` path.

## Fase 1 — close the trust moat (review findings)

- [ ] **CI gate is red on main**: the rule-based decider abstains on the
  live scenarios `calc-scientific` and `clock-timer` (suite success
  0.80 < baseline 1.00, failing since the live-eval commit). Fix the
  decider coverage for those scenarios or repair the scenario specs —
  until this is green every PR shows red CI. Do NOT relax the baseline
  to make it pass.
- [x] Approvals forgeable: pending-request binding for grants — PR #1
- [ ] Laya NDJSON protocol: `PredictResponse` carries no request id —
  post-timeout stale lines desync the stream. Add ids end-to-end
  (`LayaClient` ↔ `workers/laya/worker.py`), drop stale responses.
  Also `worker_cmd.split_whitespace()` breaks paths with spaces.
- [ ] Driver hardening:
  - `drivers/browser` walker emits `el.value` for `type=password`
    inputs into observations/digest/journal — redact like macOS
    `is_sensitive_role`.
  - `Action::Navigate` runs `open <url>` without flag-injection guard
    (`-a App` smuggling) — validate/prepend `--`.
  - `drivers/sim` untargeted `Scroll` is not gated on
    `allow_coordinates` — same gate as macOS Key/untargeted-Scroll.
- [ ] Engine/CLI correctness:
  - `dexter_map` (MCP + CLI `map`) skips `driver.restore` when the
    re-observe errors (`?` before restore) — stolen focus.
  - `Route::Retry | Route::Reobserve` are silent no-ops in the closed
    loop — implement or fail honestly.
  - `expr_next_candidate` stalled detection misfires on repeated-digit
    expressions ("22") — tighten the stalled flag.
  - `scope_to_window` on single-window apps produces false scoping.

## Fase 2 — real decision layer (`DecisionProvider`)

- [ ] Trait `DecisionProvider` in Rust replacing the ad-hoc laya
  protocol as the supported seam: `decide(observation, goal) ->
  Decision`. Providers: `stub`/`sim` (deterministic, tests),
  `http-openai-compatible`, `gemini`.
- [ ] Gemini provider using `GEMINI_API_KEY` (user secret) — default
  model the cheapest flash-lite variant available; minimal spend.
- [ ] Worker protocol versioning + laya health endpoint
  (`dexter doctor --engine` coverage is partial today).

## Fase 3 — measurable reliability

- [ ] Expand `datasets/scenarios` coverage (more apps, negative cases,
  OCR paths) and keep `eval scenario --check baseline.toml` green.
- [ ] Fix route-gold coverage inflation in eval (`Gold::Route` always
  counts covered).
- [ ] OSWorld-style scenario set + published numbers.

## Fase 4 — reach

- [ ] Browser driver polish (biggest cross-platform reach).
- [ ] Windows driver skeleton.
- [ ] Release hygiene: universal binary, homebrew tap refresh, demo
  GIF in README.

## Cleanup backlog (low severity, pick when convenient)

- `is_editable` misses `secure_text_field`; `parse_goal`
  first-occurrence find bug; no index-qualified candidates (ambiguous
  labels unresolvable); `max_empty_steps` dead field; cosmetic per-step
  observe for overlay bounds; parse edges (`split_whitespace`,
  `AppSelector` '.', `token_rect` div-zero, empty `SemanticTarget`
  wildcard); UTF-16 surrogate split in `cg_type_text`; MSRV 1.85 vs
  `is_multiple_of` (needs 1.87); AX messaging timeout root-only;
  `--digest` skips truncation warnings; `windows --app bundle:` matches
  window owner name not bundle id; `dexter mcp` builds the driver
  twice; aria-hidden elements in browser walker; primary-monitor-only
  bounds; `pid_for_bundle` no ambiguity check.
