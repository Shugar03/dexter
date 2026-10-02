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
  `v1beta/openai` compat API — PR #8, 2026-09-30
- [x] Gemini provider = `OpenAiProvider` defaults: `GEMINI_API_KEY`,
  `gemini-2.5-flash-lite` (cheapest flash-lite tier); prompts bounded
  (≤8 candidates, ≤1500-char digest, 150 max_tokens, temperature 0) —
  same PR
- [x] Worker protocol versioning (`hello` handshake at spawn/respawn,
  `"v"` on requests) + `dexter doctor` always probes an engine —
  PR #9, 2026-09-30

## Fase 3 — measurable reliability

- [x] Scenario coverage expanded — `ocr-canvas` (OCR path via
  `SpecElement.source`), `modal-confirm`, `field-disabled` (negative:
  disabled affordance), `cancel-polarity` (negative characterization
  of the polarity-match gap) — PR #11, 2026-09-30
- [x] Route-gold coverage inflation fixed — coverage is act-golds
  only (`covered / act_items`), `route_accuracy()` separate — PR #10,
  2026-09-30
- [x] OSWorld-style scenario set + published numbers — 13 sim specs
  across affordance categories (enable chains, dialogs, OCR +/-
  paths, negatives, sequential goals), reproducible suite report
  (`dexter-eval` example) + `docs/eval-numbers.md` — PR #12,
  2026-09-30

## Fase 4 — reach

- [x] Browser driver polish — disabled guard on every element act
  (`Target::Element` binds bypass the generator's enabled filter;
  programmatic clicks on disabled controls now report `Failed`
  instead of simulating success) — PR #14, 2026-09-30
- [x] Windows driver skeleton — `dexter-windows` crate: honest
  `Unsupported` seam + `uia_role()` control-type → role table —
  PR #13, 2026-09-30
- [x] Release hygiene: universal binary, homebrew tap refresh, demo
  GIF in README — universal `lipo -verify_arch` gate in CI, cask
  single-sourced in `packaging/homebrew/` with a golden test against
  the live tap, `docs/assets/suite.gif` — PR #20, 2026-09-30

## Fase 5 — decision depth + reach

Seeded from ROADMAP.md (Etapas 2–5) now that Fases 1–4 closed.

- [ ] Browser protected sessions — persistent profiles via WebDriver
  capabilities: `--browser-profile <dir>` maps to
  `goog:chromeOptions.args [--user-data-dir=…]` /
  `moz:firefoxOptions.args [-profile …]` (safaridriver documents its
  limitation), so cookies/logins survive across runs. Session
  creation currently sends `alwaysMatch: {}` — add the capabilities
  layer, the per-browser arg table in `docs/sdd/browser.md`, and
  tests on the generated session payload. (ROADMAP.md Etapa 2 — last
  open item)
- [ ] Decision cascade — `--engine cascade` composite: `RuleBased`
  first, escalate to `LayaSidecar` on `Abstain`, then to
  `OpenAiProvider`; each hop journaled so eval can report which tier
  answered. Hermetic tests over fake providers: abstain → escalate →
  answer, and abstain at every tier → `Abstain` (never an invented
  act). (ROADMAP.md Etapa 3 — cascada reglas → Laya → LLM)
- [ ] Laya concrete uses — blocking-modal detection and
  ambiguous-target resolution as typed `Question`s through the
  decision layer, pinned by sim scenarios (a modal blocking the goal;
  two identical labels). (ROADMAP.md Etapa 3 — usos concretos)
- [ ] Threshold calibration — sweep `min_confidence` and rule-based
  priors over `datasets/` via `dexter eval matrix`; publish the
  before/after coverage/accuracy table to `docs/eval-numbers.md`.
  (ROADMAP.md Etapa 3 — calibración de umbrales)
- [ ] Linux driver skeleton — `dexter-linux` crate mirroring
  `dexter-windows`: every operation honestly `Unsupported` (all
  capability flags false) plus the AT-SPI role → normalized-role
  table for the future backend. (ROADMAP.md Etapa 5)

## Platform-gated — manual dispatch only (skip on Linux)

Not `- [ ]` items so the builder never picks them: each needs a
session on its own platform. Dispatch by hand and, when it lands,
record it in the Cleanup backlog above with the PR link.

- Windows UIA backend — real observe/act for `dexter-windows` on UI
  Automation + MSAA fallback via windows-rs (ROADMAP.md Etapa 4);
  needs a Windows session.
- ~~macOS AX eval dataset — an `eval harvest` manifest over real AX
  trees producing `datasets/macos/`~~ — Done in PR #44 (2026-10-02):
  20 items over TextEdit, Finder, Calculator, Clock, System Settings,
  es-ES pinned per app; numbers in `docs/eval-numbers.md`.
- Permission onboarding polish — `doctor` + system prompt flow
  (ROADMAP.md Etapa 1); macOS-only surface.

## Cleanup backlog (low severity, pick when convenient)

- Done in PR #16: `secure_text_field` editable, `parse_goal` real
  word offsets, `max_empty_steps` dead field removed, `AppSelector`
  bundle-id shape, `token_rect` → `Option` on degenerate windows,
  empty `SemanticTarget` → `InvalidInput`, MSRV bumped to 1.87
  (`is_multiple_of` was already in use, stabilized there), browser walker skips `aria-hidden` subtrees.
- Done in PR #17: index-qualified generated targets (duplicate
  labels resolvable via `SemanticTarget.index`).
- Done in PR #18: polarity veto — antonym labels (`confirmar` vs
  `cancelar`, `save` vs `discard`, ...) skipped before scoring;
  `cancel-polarity` scenario re-pointed at `abstained`.
- Done in `fb2b6f7` (landed directly on main, no PR): goal negation —
  `NEGATORS` flip the next term's polarity ("no guardar" offers
  "Descartar"); `negate-discard.toml` pins it end-to-end.
- Done in PR #22 (2026-09-30): cosmetic per-step observe for overlay
  bounds — `run_step` only takes the bounds observe when a live
  journal sink consumes it.
- Done in PR #5 (verified 2026-09-30): `split_whitespace` parse edges
  — the only production use was laya `worker_cmd`, now `shlex`-split;
  the remaining one is the test-only `fake_webdriver` HTTP parser.
- Done in PR #24 (2026-09-30): UTF-16 surrogate split in
  `cg_type_text` — chunking via `dexter_driver::utf16_chunks`, which
  breaks only on char boundaries.
- Done in PR #26 (2026-10-01): AX messaging timeout root-only —
  AX roots come from `ax::app_element`, which also arms the timeout
  process-wide on the system-wide element.
- Done in PR #32 (2026-10-01): `--digest` skips truncation warnings
  — both output formats emit `Observation::perception_warnings()` on
  stderr.
- Done in PR #34 (2026-10-01): `windows --app bundle:` matches
  `Window.bundle_id` (macOS: `NSRunningApplication.bundleIdentifier`)
  via `AppSelector::matches_window`, never the owner name; windows
  without a bundle id fail closed.
- Done in PR #36 (2026-10-01): `dexter mcp` builds the driver twice
  — MCP takes the CLI engine's driver via `Engine::into_driver`.
- Done in PR #38 (2026-10-01): primary-monitor-only bounds —
  monitor-crop captures (OCR + screenshot fallback) use the display
  that fully contains the window (`dexter_vision::capture_monitor`),
  cropped relative to that display's origin and scale; spanning or
  off-screen windows fail closed.
- Done in PR #40 (2026-10-01): `pid_for_bundle` no ambiguity check
  — `--app <bundle id>` resolves via `dexter_driver::unique_app_pid`;
  several running instances fail closed as `Ambiguous` (use `--pid`).
- Done in PR #44 (2026-10-02): macOS AX eval dataset —
  `datasets/macos/` grew from 10 to 20 decision points harvested from
  real AX trees (Calculator, Clock and System Settings added;
  TextEdit/Finder re-harvested). Preps pin every app to es-ES via the
  `AppleLanguages` defaults + launch-arg contract the live specs use,
  so the manifest is deterministic on any host locale. Rule-based
  baseline: cov 100%, act 12/15 (80%), routes 4/5, fa 1 — the four
  misses are measured generator/engine weaknesses, reported in
  `docs/eval-numbers.md`.
- Remaining: none.
