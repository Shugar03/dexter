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

Item tags: `[windows]` / `[macos]` / `[linux]` at the start of an item
mean the work needs that platform — on the builder's Linux box do not
attempt a `[windows]`/`[macos]` item locally: dispatch a child Devin
session on that platform (the org has macOS and Windows machines) with
the same process and hard rules, and have it check the item off here
when it lands. Untagged items run anywhere.

Never let this file run dry: when fewer than 2 unchecked items remain,
seed the next Fase from the still-pending Etapas in ROADMAP.md and any
slices recent PRs flagged as remaining — same `- [ ]` format, platform
tags where needed — in the same run's docs PR.

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

- [x] Browser protected sessions — persistent profiles via WebDriver
  capabilities: `--browser-profile <dir>` maps to
  `goog:chromeOptions.args [--user-data-dir=…]` /
  `moz:firefoxOptions.args [-profile …]` as `firstMatch` entries
  (safaridriver matches none — limitation documented, CLI rejects it
  up front); dir created + canonicalized, profile sessions never
  adopted — PR #47, 2026-10-02 (ROADMAP.md Etapa 2 — last open item)
- [x] Decision cascade — `--engine cascade` composite: `RuleBased`
  first, escalate to `LayaSidecar` on `Abstain`, then to
  `OpenAiProvider`; each hop journaled so eval can report which tier
  answered. Hermetic tests over fake providers: abstain → escalate →
  answer, and abstain at every tier → `Abstain` (never an invented
  act). `DecisionEngine::decide_traced` → `DecisionMade.hops`, eval
  `ItemVerdict.tier`; invented acts decay to `Abstain`, tier errors
  propagate — PR #49, 2026-10-02 (ROADMAP.md Etapa 3 — cascada
  reglas → Laya → LLM)
- [x] Laya concrete uses — blocking-modal detection and
  ambiguous-target resolution as typed `Question`s through the
  decision layer, pinned by sim scenarios (a modal blocking the goal;
  two identical labels). `dexter_decision::questions` (`blocking_modal`
  Bool, `disambiguate` Choice); `RuleBased` abstains on both, Laya
  escalates to a human / picks the twin; `modal-blocking` +
  `ambiguous-twin` scenarios — PR #51, 2026-10-02 (ROADMAP.md Etapa 3
  — usos concretos)
- [x] Threshold calibration — sweep `min_confidence` and rule-based
  priors over `datasets/` via `dexter eval matrix`; publish the
  before/after coverage/accuracy table to `docs/eval-numbers.md`.
  `dexter_eval::calibrate` (`sweep`, `pick` = max utility with false
  acts at 2× cost → fewest false acts → most conservative),
  `eval matrix --act-threshold a,b,c` / `--min-confidence a,b,c`;
  pick = 0.65 = shipped default (no move). Laya τ wired but
  unmeasured (no checkpoint on the builder) — PR #53, 2026-10-02
  (ROADMAP.md Etapa 3 — calibración de umbrales)
- [x] Linux driver skeleton — `dexter-linux` crate mirroring
  `dexter-windows`: every operation honestly `Unsupported` (all
  capability flags false) plus the AT-SPI role → normalized-role
  table for the future backend. `atspi_role_name` (`AtspiRole`
  protocol id → canonical name, generated from `atspi-constants.h`)
  + `atspi_role` (name → normalized role); SDD `docs/sdd/linux.md` —
  PR #55, 2026-10-02 (ROADMAP.md Etapa 5)

## Fase 6 — platform backends + depth

Seeded from the slices the last PRs flagged as remaining plus
ROADMAP.md Etapas 1, 3, 4, 5 and 6. `[windows]`/`[macos]` items are
dispatched to child sessions (see Item tags above).

- [x] `[windows]` `act()` slice — UIA patterns first:
  `Invoke`/`Toggle`/`SelectionItem`/`ExpandCollapse`/`LegacyIAccessible`
  press ladder, writable `Value` → `set_value`/`type_text`,
  `SetFocus`/`SetForegroundWindow` → `focus`, `ScrollItem`/`Scroll` →
  `scroll`; Win32 `SendInput` only behind `ctx.allow_coordinates`
  (+ foreground pid for keys). `Target::Element` re-walks the live UIA
  tree and identity-checks (mismatch → `StaleReference`), `Semantic`
  resolves via `dexter_world_model`, `Focused` via `GetFocusedElement`
  pid-scoped; `Navigate` on a closed http/https/mailto allowlist.
  Remaining: context-menu patterns, OCR targets, `background_input`,
  capture slice — PR #57, 2026-10-04 (Etapa 4; observe landed in PR #45)
- [x] `[windows]` screenshot/vision perception — `scope.screenshot`
  captures the capture window's visible frame
  (`DWMWA_EXTENDED_FRAME_BOUNDS`, listed bounds as fallback) as a
  per-display GDI crop: `EnumDisplayMonitors`/`MONITORINFOEXW` +
  `EnumDisplaySettingsW` raster → `geometry::monitor_geometry` scale
  (axes must agree), `CreateDCW("DISPLAY")` + `StretchBlt` +
  top-down `GetDIBits`; `capture_monitor`/`monitor_pixel_crop` pick
  and crop, spanning/off-screen/degenerate → `NotFound`.
  `capabilities().screenshots = capture::available()` — honest on
  displayless hosts. `scope.vision` mirrors the macOS augment over
  the captured rect; `dexter_vision::platform_provider()` gains a
  `WinOcr` provider (`Windows.Media.Ocr`, on-device, `ocr_word_rect`
  mapping, `Unsupported` with no language pack, `NaN` confidence —
  WinRT reports none). Remaining: occluded windows capture their
  on-screen pixels (display-raster semantics, same as macOS), OCR
  stays evidence-only (`Target::Point` + `allow_coordinates` to
  act), no Linux provider yet — PR #59, 2026-10-04 (Etapa 4)
- [x] `[windows]` dedicated MSAA fallback — real MSAA walk for
  controls UIA misses (legacy Win32), beyond UIA's built-in
  LegacyIAccessible bridge. `msaa::augment` fires only on measured
  gaps — Server 2022 stock apps are fully covered by the bridge (0
  adds on msconfig/odbcad32/charmap/cleanmgr/dxdiag/netplwiz/classic
  CPLs/MMC snap-ins): a window whose UIA partition is empty or
  root-only (`warranted`) earns a whole `OBJID_CLIENT` walk, and
  unclaimed, visible (`IsWindowVisible`), non-nested,
  non-interior-covered descendant HWNDs earn their own. The walk
  (`AccessibleObjectFromWindow` → `IAccessible`, `accChildCount`/
  `AccessibleChildren` in chunks) maps `ROLE_SYSTEM_*` via
  `msaa_role` (`raw_role` carries the `msaa:` origin),
  `UNAVAILABLE` → `enabled=false`, `FOCUSED` → `focused`,
  `PROTECTED` → value never read, `INVISIBLE|OFFSCREEN` nodes
  skipped with subtrees; `merge_msaa` dedupes on name+bounds (±2px)
  plus identical-rect role tolerance (providers mis-report roles —
  measured on odbcad32), children reparent to the UIA twin,
  failures → `collection_errors`, caps → `elements_truncated`.
  act: `Click` → `accDoDefaultAction` (no default → `UNSUPPORTED`,
  disabled → `FAILED`, right/middle → `SendInput` behind
  `allow_coordinates`), `SetValue`/`TypeText` → `put_accValue`
  (else opt-in keyboard path), `Focus` → `accSelect(TAKEFOCUS)`,
  `Target::Element` re-walk identity check → `StaleReference`.
  Remaining: hidden-but-reachable containers (inactive tab pages)
  stay out of perception by the visibility rule; unclaimed HWNDs
  whose interior UIA partially covered are skipped whole — PR #61,
  2026-10-04 (Etapa 4)
- [ ] `[linux]` `dexter-linux` real observe — AT-SPI2 tree walk on
  X11 mirroring the Windows observe slice: window enumeration,
  app/window-scoped walk → normalized `Element`s, same
  `ax_limited`/`collection_errors`/`elements_truncated` flags.
  Depends on the Fase 5 skeleton. (Etapa 5)
- [ ] Recovery ladder rung 3 — alternative semantic target: when the
  chosen candidate keeps failing, try the next-best generated
  candidate before escalating (the engine currently replays the last
  act on `Route::Retry`). Pin with a sim scenario whose first-ranked
  target is a dead end. (Etapa 1 — recovery ladder pasos 1–3)
- [ ] Browser dataset expansion — `eval harvest --driver browser`
  over more pages into `datasets/browser/` (install chromedriver if
  missing); append honest numbers to `docs/eval-numbers.md`.
  (Etapa 3)
- [ ] `[macos]` live-scenario expansion — more apps into
  `datasets/scenarios/` live specs pinned es-ES per the
  `AppleLanguages` contract (e.g. Safari, Notes flows). (Etapa 3)
- [ ] `[macos]` Laya fine-tune — `dexter eval export` rows →
  fine-tune `laya-mlx` (MLX needs Apple silicon); eval before/after
  on the matrix and report honestly — only worth keeping if the
  numbers justify it. (Etapa 3)
- [ ] Desktop app scaffold — Tauri 2 shell (`apps/dexter-desktop`)
  rendering journal tail + policy state read-only; no control
  surface yet. (Etapa 6 — first slice)

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
- Done in PR #43 (2026-10-02): permission onboarding polish —
  `doctor` reports per-permission status (`permissions::Probe` seam,
  `Permission` metadata), `--request` prompts missing only, SDD note
  at `docs/sdd/permissions.md`; prompt verified live on macOS.
- Done in PR #44 (2026-10-02): macOS AX eval dataset —
  `datasets/macos/` grew from 10 to 20 decision points harvested from
  real AX trees (Calculator, Clock and System Settings added;
  TextEdit/Finder re-harvested). Preps pin every app to es-ES via the
  `AppleLanguages` defaults + launch-arg contract the live specs use,
  so the manifest is deterministic on any host locale. Rule-based
  baseline: cov 100%, act 12/15 (80%), routes 4/5, fa 1 — the four
  misses are measured generator/engine weaknesses, reported in
  `docs/eval-numbers.md`.
- Done in PR #45 (2026-10-02): Windows UIA observe — `dexter-windows`
  enumerates top-level HWNDs and walks UIA ControlView trees into
  normalized `Element`s (AppSelector via windowed pids +
  `unique_app_pid`, `scope.window` native, `ax_limited` when a
  windowed app yields no UIA elements). Remaining platform-gated:
  `act()`, dedicated MSAA fallback, screenshot/vision.
- Remaining: none.
