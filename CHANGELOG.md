# Changelog

## Unreleased

### Security

- **`dexter_grant` can no longer mint approval for arbitrary
  fingerprints.** Fingerprints are deterministic, so an agent could
  compute one locally and grant it without any escalation — bypassing
  every `require_approval` rule. `ApprovalStore` now tracks pending
  requests: the engine records one on each `NeedsApproval`, and a grant
  is honored only while its request is live (bounded, TTL-bound). Both
  grant outcomes are journaled.
- **Browser walker no longer leaks password values into observations.**
  `input[type=password]` emitted its live `el.value` — the typed secret
  — into element records, digests and the event journal. Values are now
  nulled at collection inside the page (same rule as
  `is_sensitive_role` in the macOS walker).
- **`Action::Navigate` flag injection.** `open <url>` parsed a url
  beginning with `-` as flags (`-a App` opens an arbitrary
  application); it now passes `--` before the argument.
- **Sim driver: untargeted `Scroll` required no coordinate opt-in** —
  same gate as `Key` chords and the macOS scroll path; now fails
  `Unsupported` unless `allow_coordinates` is set.
- **Breaking (internal API):** `Engine::grant_approval` →
  `Engine::pre_grant_approval` for operator pre-authorization (scenario
  `grants` lists); the agent-facing path is `Engine::approve_pending`,
  which returns whether the request existed.

### Added

- **Browser driver: W3C `/actions` endpoint — real input for what DOM
  synthesis can't cover.** `allow_coordinates` (the same `--coords`
  consent that unlocks CGEvent on macOS) upgrades `Click`/`Key`/
  `TypeText`/untargeted `Scroll` to the browser's real input pipeline:
  pointer actions anchored at the element origin (its in-viewport
  center — still no raw coordinates), key-source sequences for chords
  and per-char typing, and a wheel source for untargeted scrolls. This
  reaches what DOM dispatch cannot: `isTrusted`-gated pages,
  `contenteditable`/rich-text editors where `el.value` inserts
  nothing, real `keydown` semantics, and wheel-driven scroll effects.
  Every `/actions` act reports `Mechanism::Coordinates` honestly
  (`background_input` stays true — it never moves the OS cursor); the
  element-reference probe runs the same stale/disabled guard as
  `exec_on` before any pointer act, and held input state is released
  on error. `Target::Point` stays `Unsupported` — semantic targets are
  always available in a page. Docs: `docs/sdd/browser.md`.
- **`OpenAiProvider` — model-backed decision engine over any
  OpenAI-compatible endpoint** (`--engine openai`). The model may only
  pick an index from the generated candidate list or take a route —
  incoherent replies (bad JSON, out-of-range index, unknown route)
  decay to `Abstain`, never an invented action. Transport/config
  failures stay honest engine errors. Defaults point at Gemini's
  `v1beta/openai` compat API with `gemini-2.5-flash-lite` and
  `GEMINI_API_KEY`; prompts are bounded (≤8 candidates, ≤1500-char
  digest, 150 completion tokens, temperature 0). Docs:
  `docs/sdd/decision.md`.
- **Laya worker protocol versioning.** Every spawn/respawn performs a
  `hello` handshake: the worker must answer `ok` with a matching
  `protocol` or the spawn fails — a pre-versioning or wrong binary can
  no longer serve as a silent stale sidecar. `predict` requests carry
  `"v"` for forward discrimination. `dexter doctor` now always probes
  an engine (`rule-based` by default) instead of skipping the engine
  section without `--engine`.
- **Scenario dataset expansion** (`datasets/scenarios/`). Four new sim
  worlds: `ocr-canvas` (goal element only exists as `source = "ocr"`),
  `modal-confirm` (2-step destructive-confirm dialog), `field-disabled`
  (present-but-disabled affordance must abstain) and
  `cancel-polarity` (negative characterization of the generator's
  polarity blindness — `expected = "max_steps"` flips red when the gap
  closes). `SpecElement.source` declares the perception layer per
  element (`ocr`/`vision`/`dom`/`accessibility`); a hermetic test now
  runs every sim spec in the dataset against its declared outcome.
  Second batch: `form-fill` (sequential goal — edit subgoal
  auto-completes on the value change, submit closes) and
  `ocr-label-only` (OCR text with no affordance must abstain).
  `crates/eval/examples/suite_report.rs` prints the reproducible
  sim-suite metrics table; current numbers live in
  `docs/eval-numbers.md`.
- **`dexter-windows` driver skeleton.** The Windows side of the driver
  seam before a backend exists: `WindowsDriver` declines every
  operation with `Unsupported` (all capability flags false — no
  simulated claims) plus `uia_role()`, the UIA ControlType →
  normalized-role table the real backend will plug into. Docs:
  `docs/sdd/windows.md`.
- **Browser driver honesty polish.** Every element act now runs
  through a shared in-page guard: `disabled`/`aria-disabled` elements
  report `ActionResult::failure(Failed)` instead of taking a
  programmatic click that lands but does nothing a user could do —
  the "never simulate success" invariant applied to Target::Element
  binds that bypass the generator's enabled filter.
- **Cleanup sweep #1.** `parse_goal` now scans words at their real
  byte offsets (a word inside a consumed phrase-verb no longer
  swallows a later standalone occurrence); `is_editable` covers
  `secure_text_field`; `RuleBased.max_empty_steps` dead knob removed;
  `AppSelector::parse` only treats bundle-id-shaped strings as bundle
  ids ("TextEdit 1.2" stays a name); `token_rect` returns `Option`
  instead of NaN bounds on degenerate windows; resolving an all-None
  `SemanticTarget` is `InvalidInput`, never a wildcard match; the
  browser walker skips `aria-hidden` subtrees; workspace MSRV is now
  honestly `1.87` (the code already used `is_multiple_of`, stabilized
  in that release).
- **Index-qualified generated targets.** When several elements share a
  label, the generator's `{role, name}` target could never resolve —
  the resolver failed closed as Ambiguous on every offer. Generated
  targets now carry `SemanticTarget.index` (position among matches in
  tree order, via `world_model::find_elements_in` — the same matching
  semantics the resolver uses), so duplicate labels are resolvable.
- **Generator polarity veto.** A label matching the ANTONYM of a goal
  term (and not the term itself) is now skipped before scoring —
  `"confirmar el pedido"` can no longer press `"Cancelar pedido"`.
  `ANTONYMS` covers es+en pairs (confirmar↔cancelar, aceptar↔rechazar,
  guardar↔descartar, open↔close, save↔discard, ...). A goal naming
  both polarities vetoes everything and abstains — fail-closed on a
  genuinely ambiguous intent. `cancel-polarity` flipped from the
  pinned wrong-act outcome to `abstained`, as designed.
- **Goal negation.** `NEGATORS` markers ("no", "not", "never",
  "nunca", "jamas", "sin", contractions like "don't"→"don"+"t") flip
  the next term's polarity: the negated term vetoes its own labels
  and its antonym (when known) joins the wanted terms — "no guardar
  el borrador" offers "Descartar borrador", never "Guardar".
  `negate-discard.toml` pins the flow end-to-end.

### Fixed

- **macOS AX messaging timeout only covered the app root.**
  `set_messaging_timeout(1.5)` was applied to the `AXApplication`
  element alone; every window, child and action ref copied out of it
  ran on the global default (~6 s per message), so a hung app could
  stall a tree walk for minutes. AX roots now come from
  `ax::app_element`, which also arms the timeout process-wide on the
  system-wide element (once) (PR #26).
- **macOS `type_text` could split a surrogate pair across events.**
  Text was posted in fixed 20-unit UTF-16 slices, so an emoji or other
  astral char straddling a boundary went out as two lone surrogates
  (typing U+FFFD or nothing). Chunking now goes through
  `dexter_driver::utf16_chunks`, which only breaks on char boundaries
  (PR #24).
- **`run_step` paid a cosmetic observe on every act.** The overlay's
  `target_bounds` lookup ran a full observe before each non-`Point`
  single step even with no consumer (AX walk live; advanced `on_tick`
  effects in sim). It now runs only when a live journal sink is
  attached; otherwise `target_bounds` is `null` (PR #22).
- **Laya worker protocol desync.** Responses were paired positionally:
  a request that timed out still queued its late reply on the channel,
  and the *next* request consumed it as its own (wrong answers or
  phantom "ok but no answers" errors — e.g. after a slow `health()`
  probe). Responses are now matched by request `id`; stale or
  unattributable lines are dropped, while id-less `ok:false` lines
  (worker startup failures) still surface honestly. `worker_cmd` is
  shell-split (`shlex`) so quoted paths with spaces work.
- **`dexter map`/`dexter_map` stranded borrowed focus on error.** A
  failed post-wake re-observe propagated before `restore()`, leaving
  the target app frontmost. Restore now runs before `?`.
- **`Route::Retry` never retried.** The task loop treated it as a bare
  continue — the "repeat the last action" contract was silently
  dropped. Retry now replays the last attempt once.
- **Repeated-digit goals abstained deterministically.** The expression
  stall check compared the last pressed label against the pending plan
  step — every consecutive-digit goal ("5 más 22") decayed to prior
  0.4 → abstain. Only the error signal marks a stall now.
- **`collect_window` fallback false-scoped single-window apps.** When
  AX couldn't reach the window subtree, the driver returned the full
  app element tree with `windows == [win]` — which callers read as
  natively scoped and skipped their post-filter. The fallback now
  bounds-filters to the window rect.
- **Eval route-gold coverage inflation.** `Gold::Route` items counted
  as `covered` unconditionally — "routes are always available" meant
  every route item inflated `coverage()` without the generator
  offering anything. Coverage is now `covered / act_items` (generator
  recall over actionable items); `accuracy()` numerator and
  denominator are act-only (previously mixed); `route_accuracy()`
  reported separately (`routes_correct / route_items`).
- **Eval CI gate red since live scenarios landed.** Live scenario
  specs target Spanish AX names but CI runners are en-US — the decider
  abstained deterministically on `calc-scientific` and `clock-timer`
  (suite 0.80 < baseline 1.00). `prep` now pins each live app to
  `AppleLanguages = [es]` and `teardown` deletes the override; a
  contract test guards the convention. Preps also quit via `pkill`
  instead of `osascript ... to quit`: AppleScript *launches* the app to
  deliver the quit, so on a cold start the app booted English before
  the pin landed and `open -a` reactivated that instance — the race
  that kept `clock-timer` abstaining even after pinning. Launches also
  pass `-AppleLanguages '(es)'` as a launch arg (NSArgumentDomain
  outranks any prefs domain), and the live runner prints the last
  decision digest on abstain so future live failures show the world
  the decider actually saw.
### Packaging

- **Homebrew cask has one source of truth.** The release `tap` job
  rendered the cask from an inline heredoc that had drifted from the
  published tap (it still advertised the removed `--no-quarantine`
  flag). It now renders `packaging/homebrew/dexter.rb.in` via
  `scripts/render-cask.sh` (strict version/sha256 validation), writes
  the rendered cask to the run summary when `TAP_GITHUB_TOKEN` is
  absent, and a golden test (`scripts/test-render-cask.sh`, CI
  `packaging` job) pins it byte-for-byte to the live `0.1.0-rc.1` cask.
- **Universal binary gated on every PR.** CI `release-build` now builds
  both `aarch64` and `x86_64` slices of `dexter` and `dexter-overlay`,
  `lipo`s them and asserts both architectures (`-verify_arch`) — the
  release `package` job asserts the same before signing.
- **README demo GIF** (`docs/assets/suite.gif`) replaying a real run of
  the hermetic sim suite; reproducible with `scripts/demo_gif.py`.

## 0.1.0

First public release. Dexter is a local-first Agent Computer Runtime:
the agent decides *what* to achieve; Dexter decides *how* to interact,
executes, verifies the result and recovers — under an operator-owned
security policy.

### Runtime

- Single synchronous engine shared by CLI, MCP server and Python SDK.
- Observe → decide → act → verify loop with bounded steps, cooperative
  cancellation (`dexter_cancel`) and per-task wall-clock timeouts.
- Three-valued verification: VERIFIED / FAILED / UNCERTAIN — uncertainty
  is never reported as success.
- Bounded journal with a live disk sink and an honest `dropped` count.

### Perception

- macOS Accessibility driver: app/window-scoped observation, native
  `AXWindow` subtree walk with bounds-matched post-filter fallback.
- `ax_limited` degraded-AX detection; explicit `collection_errors`.
- Opt-in OCR fallback (`observe --vision`, `dexter_observe {vision}`):
  on-device Apple Vision text recognition appends inert `[ocr]`
  elements — evidence, never implicit agency.
- Browser driver (WebDriver/W3C) and simulation driver.

### Security

- Fail-closed policy engine; agents cannot raise their own trust —
  approvals and coordinate input are operator flags at startup
  (`dexter mcp --approve-all --coords`).
- Physical input is a permission floor, not a verdict: enabling
  `--coords` lifts the deny but mutating actions still require approval.
- MCP payload bounds: goal ≤ 4 KB, done/action ≤ 64 KB,
  `max_steps` ≤ 200, `max_secs` ≤ 3600; one task at a time.

### Integrations

- MCP server (`dexter mcp`): 9 tools — observe, candidates, act,
  verify, task, cancel, grant, journal, status.
- Python SDK (`sdk/python`): zero-dependency stdio client.
- Laya sidecar: supervised worker with bounded respawn, health probe
  (`dexter doctor --engine`), min-confidence abstention.

### Packaging

- Universal macOS binary (aarch64 + x86_64, `lipo`), ad-hoc signed.
- GitHub Releases + `Shugar03/homebrew-dexter` cask tap.
- Release binaries are ad-hoc signed, **not** notarized — see the
  install docs for the quarantine caveat.
