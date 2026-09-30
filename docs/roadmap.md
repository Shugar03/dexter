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

- [x] CI gate red — live scenarios pinned to es-ES per app
  (`AppleLanguages` defaults + `open --args` launch pin) — PR #3,
  2026-09-30 (suite 100%, `clock-timer` 2/2 steps)
- [x] Approvals forgeable: pending-request binding for grants — PR #1
- [x] Laya NDJSON protocol desync (id-matched responses, shlex-split
  `worker_cmd`) — PR #5
- [x] Driver hardening (password-value redaction, `open --` Navigate,
  sim scroll gate) — PR #6
- [x] Engine/CLI correctness (map restore-before-error, `Route::Retry`
  replay, repeated-digit stall fix, `collect_window` fallback
  bounds-filter) — PR #7

## Fase 2 — real decision layer (`DecisionProvider`)

- [x] `OpenAiProvider` behind the existing `DecisionEngine` trait —
  works with any OpenAI-compatible endpoint; Gemini reached via its
  `v1beta/openai` compat API — PR #TBD, 2026-09-30
- [x] Gemini provider = `OpenAiProvider` defaults: `GEMINI_API_KEY`,
  `gemini-2.5-flash-lite` (cheapest flash-lite tier); prompts bounded
  (≤8 candidates, ≤1500-char digest, 150 max_tokens, temperature 0) —
  same PR
- [x] Worker protocol versioning (`hello` handshake at spawn/respawn,
  `"v"` on requests) + `dexter doctor` always probes an engine —
  PR #TBD, 2026-09-30

## Fase 3 — measurable reliability

- [x] Scenario coverage expanded — `ocr-canvas` (OCR path via
  `SpecElement.source`), `modal-confirm`, `field-disabled` (negative:
  disabled affordance), `cancel-polarity` (negative characterization
  of the polarity-match gap) — PR #TBD, 2026-09-30
- [x] Route-gold coverage inflation fixed — coverage is act-golds
  only (`covered / act_items`), `route_accuracy()` separate — PR #TBD,
  2026-09-30
- [x] OSWorld-style scenario set + published numbers — 13 sim specs
  across affordance categories (enable chains, dialogs, OCR +/-
  paths, negatives, sequential goals), reproducible suite report
  (`dexter-eval` example) + `docs/eval-numbers.md` — PR #TBD,
  2026-09-30

## Fase 4 — reach

- [x] Browser driver polish — disabled guard on every element act
  (`Target::Element` binds bypass the generator's enabled filter;
  programmatic clicks on disabled controls now report `Failed`
  instead of simulating success) — PR #TBD, 2026-09-30
- [x] Windows driver skeleton — `dexter-windows` crate: honest
  `Unsupported` seam + `uia_role()` control-type → role table —
  PR #TBD, 2026-09-30
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
