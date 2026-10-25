# Changelog

## Unreleased

## 0.1.0 — 2026-10-20

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

### Added

- **Recovery ladder rung 3 — alternative semantic target.** A decider
  that keeps asking for an action that already failed twice in a row
  (`Route::Retry`, or `Act` on the same candidate — original attempt +
  one replay, fresh observation in between) no longer gets a third
  try: `Engine::run_goal` executes the next-best *generated* candidate
  of that step instead (highest prior, not behind a modal, never one
  that already failed), journaled as `RecoveryStarted { rung: 3 }`.
  With no such candidate the task ends `Escalated { EscalateHuman }`
  rather than spinning to `max_steps`. Deciders that move on by
  themselves (rule-based, via the generator's already-tried penalty)
  are untouched. `dexter_sim::Effect::Fail(detail)` (spec `{ type =
  "fail", detail }`) models a control that refuses the press;
  `dead-end-save` pins the scenario. SDD: `docs/sdd/recovery.md`.

- **Browser protected sessions — persistent profiles.**
  `--browser-profile <dir>` (with `--browser-url`) /
  `BrowserDriver::connect_with_profile` creates the WebDriver session
  on a persistent profile so cookies and logins survive across runs:
  `goog:chromeOptions.args [--user-data-dir=<dir>]` for chromedriver,
  `moz:firefoxOptions.args [-profile <dir>]` for geckodriver, offered
  as `firstMatch` entries so either endpoint matches its own. The dir
  is created if missing and passed canonical; profile sessions are
  never adopted (an adopted session would ignore the profile).
  safaridriver has no profile capability — documented in
  `docs/sdd/browser.md`, rejected up front by the CLI.

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
- **`dexter-linux` driver skeleton.** The Linux side of the driver
  seam before a backend exists: `LinuxDriver` declines every operation
  with `Unsupported` (all capability flags false) plus the AT-SPI
  tables the AT-SPI2 backend will plug into — `atspi_role_name()`
  (`AtspiRole` protocol id → canonical name) and `atspi_role()`
  (name → normalized role). Docs: `docs/sdd/linux.md`.
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
- **macOS AX eval dataset — 20 decision points over 5 real apps.**
  `datasets/macos/` grew from TextEdit + Finder to include Calculator,
  Clock and System Settings, all harvested from real accessibility
  trees via `dexter eval harvest`. Every `prep` pins its app to es-ES
  (`AppleLanguages` defaults + launch arg — the same contract the live
  scenario specs use), so the manifest re-harvests deterministically
  on any host locale; each app's last entry quits it and deletes the
  override. Baseline (rule-based): coverage 100%, act 12/15 (80%),
  routes 4/5 — the misses are generator weaknesses the dataset now
  measures, reported per app in `docs/eval-numbers.md` (PR #44).

- **Windows driver — real UIA perception and act.** `dexter-windows`
  enumerates top-level HWNDs and walks UIA ControlView trees into
  normalized `Element`s (`ax_limited` when a windowed app yields no UIA
  elements). `act()` uses the UIA pattern ladder
  (`Invoke`/`Toggle`/`SelectionItem`/`ExpandCollapse`/
  `LegacyIAccessible` press, writable `Value` → `set_value`/`type_text`,
  `SetFocus`/`SetForegroundWindow` → focus, `ScrollItem`/`Scroll` →
  scroll); `Target::Element` re-walks the live tree and identity-checks
  (mismatch → `StaleReference`); `Navigate` is restricted to an
  http/https/mailto allowlist. Right `Click` opens context menus
  (posted `WM_CONTEXTMENU`, then `IUIAutomationElement3::
  ShowContextMenu`), and Win32 `SendInput` (middle/right click, keys)
  stays behind `ctx.allow_coordinates`. A dedicated MSAA
  (`IAccessible`) walk covers only measured UIA gaps (legacy Win32),
  deduped on name+bounds. `scope.screenshot` captures the window's
  visible frame as a per-display GDI crop (scale-aware;
  spanning/off-screen → `NotFound`), `capabilities().screenshots`
  stays honest on displayless hosts, and a `WinOcr` provider
  (`Windows.Media.Ocr`, on-device) feeds the opt-in vision path —
  evidence-only, same rule as macOS OCR. (PRs #45, #57, #59, #61,
  #75, #83)
- **Linux driver — AT-SPI2 observe and act, X11/Wayland capture, XTest
  input.** `dexter-linux` walks the AT-SPI2 registry over `zbus` (apps →
  pids, `GetExtents` frames as windows, non-`SHOWING` subtrees skipped)
  into the same normalized `Element`s with `ax_limited`/
  `collection_errors`/`elements_truncated` honesty. `act()` runs the
  `Action.DoAction` press ladder, `EditableText`/`Text` →
  `set_value`/`type_text`, `Component.GrabFocus` → focus, AT-SPI window
  focus and targeted scroll; `Target::Element` re-walks and
  identity-checks (`StaleReference` on mismatch). `scope.screenshot`
  crops the window frame via X11 `GetImage` (refused on displayless or
  pure-Wayland hosts), `GDK_SCALE` HiDPI mapping covers capture and
  input, and under Wayland the XDG desktop-portal Screenshot backend
  covers XWayland targets — native Wayland windows fail closed. A
  Tesseract provider feeds `observe --vision` (evidence-only; OCR
  elements stay inert). `Action::Navigate` goes through an allowlisted
  `xdg-open`. XTest physical input only behind `ctx.allow_coordinates`
  + `defaults.physical`. (PRs #55, #63, #70, #71, #80, #84, #85)
- **Desktop app — Tauri 2 operator surface.** `apps/dexter-desktop` is a
  read-only shell over the runtime: journal tail + policy summary
  (ordered rules + effective defaults, a failed `--policy` load shown
  as an error, never the embedded default), an approvals view that
  lists pending `Approve`-routed steps and decides them through the
  same single-use fingerprint channel as the CLI (deny is the default
  on close), and a session list + recording/journal stepper.
  (PRs #69, #79, #82)
- **Decision cascade — `--engine cascade`.** `RuleBased` answers first,
  `Abstain` escalates to `LayaSidecar`, then to `OpenAiProvider`; every
  hop is journaled (`DecisionMade.hops`, eval `ItemVerdict.tier`) and an
  invented act decays to `Abstain`. (PR #49)
- **Laya typed questions.** `blocking_modal` (Bool), `disambiguate`
  (Choice), `action_effect` and `goal_complete` Bools route through the
  decision layer like a pick; `RuleBased` abstains on all of them and
  `goal_complete` never completes a goal on its own. When
  `blocking_modal` is answered true and the dialog offers a
  polarity-safe dismiss label ("Más tarde", "Cerrar"), the generator
  offers it as a real candidate — the rule-based ladder never does.
  (PRs #51, #73, #78)
- **Scenario recording/replay.** `dexter_eval::recording` turns a
  journal (in-memory or strict JSONL) into an executable trace;
  `dexter record` / `dexter replay` re-run each action through the
  single `Engine::run_step` path with policy re-evaluated —
  `done_when` is never fabricated, replay exits nonzero on divergence.
  (PRs #72, #81)
- **Eval measurement.** `dexter eval matrix` sweeps
  `act_threshold`/`min_confidence` and picks max utility with false
  acts at 2× cost (shipped default stays 0.65); typed questions get
  `expect_questions` gold labels and per-type coverage/accuracy; the
  datasets grew to 36 browser items, 20 macOS AX decision points and 8
  es-ES-pinned live specs on stock macOS apps — numbers in
  `docs/eval-numbers.md`. (PRs #53, #65, #66, #77)
- **`laya-mlx` fine-tune seam.** `--provider laya-mlx` worker backend
  (same NDJSON protocol) plus `workers/laya/finetune_mlx.py`, an
  MLX-native port of the torch recipe (frozen encoder, cached hidden
  states); measured on the frozen datasets and *not* shipped as the
  default — the held-out numbers didn't justify it. (PR #67)
- **Browser credential fill — `Action::FillCredential`.** A local-first
  TOML store (`--credentials <file>`; name → origin/username/
  password_env, so the secret only ever lives in an env var) with
  explicit consent: a `*` rule never matches `fill_credential`, and
  `--yes` doesn't grant it under `require_approval` — only a real
  single-use fingerprint does. The browser driver checks
  `location.origin` and field kind in-page, atomically; the journal and
  MCP carry only the credential name and driver details are scrubbed of
  the secret. Non-browser drivers refuse. (PR #86)

### Fixed

- **`--app <bundle id>` picked an arbitrary instance.** `pid_for_bundle`
  took the first `NSRunningApplication` for the bundle id, so with two
  instances running (or a helper sharing the id) actions could land in
  a window the caller never chose. Resolution now goes through
  `dexter_driver::unique_app_pid`: several distinct pids fail closed as
  `Ambiguous` (use `--pid`), same as `--app <name>` (PR #40).
- **Monitor-crop screenshots and OCR captured only the primary
  display.** Windows on a secondary monitor were cropped out of the
  primary's image (wrong pixels or `NotFound`), and windows hanging off
  an edge were silently clipped. The crop now uses the display that
  fully contains the window (`dexter_vision::capture_monitor`), offset
  from that display's origin at its own scale; spanning or off-screen
  windows fail closed (PR #38).
- **`dexter mcp` built the driver twice.** The CLI engine's driver
  was dropped and `run_mcp` called `build_driver` again — a second
  `safaridriver` session under `--driver browser`. MCP now takes the
  CLI engine's driver via `Engine::into_driver` (PR #36).
- **`dexter windows --app <bundle id>` matched the owner name.** The
  bundle id was substring-matched against `kCGWindowOwnerName`, so it
  matched nothing or the wrong app. `Window` now carries an optional
  `bundle_id` and `AppSelector::matches_window` compares it exactly;
  windows without one never match a bundle selector (PR #34).
- **`dexter observe --digest` dropped perception warnings.** The
  `elements_truncated` / `ax_limited` stderr warnings were printed only
  on the JSON path, so the digest — the decision-engine input — gave no
  sign that `not found` was non-definitive. Both formats now emit
  `Observation::perception_warnings()` to stderr (PR #32).
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
