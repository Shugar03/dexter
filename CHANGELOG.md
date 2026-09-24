# Changelog

## Unreleased

### Fixed (runtime-reliability-v2 review)

- macOS batched AX reads now detect `AXValue`-wrapped `AXError` slots
  correctly — the constant was `kAXValueCGRectType` (3), not
  `kAXValueAXErrorType` (5), so real error slots fell through to
  per-attribute decoders.
- Browser `drag` and multi-click (`count` 2–3) dispatch real event
  sequences again — script args were read from the inner function's
  `arguments` (the element) instead of the forwarded WebDriver args;
  `drag` also resolves the *destination* node rather than the source.
- `type_text`/`set_value` into secure fields no longer derive an
  unsatisfiable value expectation: a successful password entry reports
  `verification: null` and is never retried (a retry would append the
  secret). Explicit value expectations there report
  `uncertain`/`redacted_value`, not `failed`.
- Plan→execute contract enforced on macOS: an authorized mechanism is
  a promise — if the world moved and only a different mechanism
  applies, execute refuses instead of escalating to physical input.
- Training-row labels: interim `VerificationFailed` poll events mark a
  row provisionally false but a later `VerificationPassed` overwrites
  it — delayed-but-verified acts export `verified: true`.
- `maybe_wake` only fires when a real observation showed a windowless
  app *and* the action needs the stage — a background `launch_app` or
  `wait` no longer steals focus, and a failed observation never wakes.
- Failed pre-action observations are journaled as
  `observation_failed` (`pre_act`/`post_wake`/`goal_start`) instead of
  being silently swallowed — an unverified act is never mistaken for
  a verified one.
- macOS depth-boundary truncation flags `elements_truncated` when
  unvisited children exist, matching `walk_menu`.
- macOS `Window::New` plans empty (no route) instead of a route that
  only fails at execute; `ActionResult.element` is populated wherever
  an element was resolved; coordinate clicks skip the useless
  pre-act observation.
- Baseline latency gates (`max_observe_p95_ms`, `max_verify_p95_ms`)
  are now set in `datasets/scenarios/baseline.toml`.
- `NSString` autoreleased via a shared helper instead of leaking one
  object per clipboard/app lookup; stale-resolution `.expect()`
  replaced with a fail-closed error.

### Fixed (runtime-reliability-v2 review, round 2)

- `kCGMouseEventClickState` corrected to field 1 (`CGEventTypes.h`) —
  multi-click counts were written into scroll-wheel field 23, so the
  "real double-click" contract silently wasn't delivered. A regression
  test now pins the CGEvent field ids to the SDK, matching the AXValue
  precedent.
- `write_clipboard_text` payloads now bind the approval fingerprint —
  a grant for one clipboard text no longer covers another.
- macOS `plan` no longer drops route metadata when the AX grant is
  missing for actions that don't need it: clipboard and lifecycle
  routes keep their mechanism and the `Secrets` sensitivity floor in
  the degraded-permission case.
- Browser `invoke` validates the element's advertised actions against
  the observation its id was minted from — element ids collide across
  cached snapshots, so a cross-observation search could approve
  against the wrong world. `type_text` now checks the `__dexter_err`
  stale sentinel like every other element act.
- Secrets sensitivity is enforced at the engine seam for every
  driver: a route into a secure/password field (role, subrole or raw
  role) upgrades to `Secrets` before policy sees it. macOS sensitivity
  now checks subrole as well as role, matching `Element::is_sensitive`.
- Element-target approvals bind the `(element, observation)` pair —
  a grant for one handle no longer covers a same-shaped element.
- `execute` errors are verified before they are reported when an
  expectation exists: a timed-out delivery report can postdate the
  side effect, so the verify poll runs first — a landed effect
  completes with `result: null` and the verification as the verdict
  (the SDD's "verify before considering another action" contract).
- Training-mode journal events scrub action payloads — typed, set and
  clipboard values appear only as `{len, sha256}` digest tokens while
  the context stays deserializable for replay. The redaction contract
  ("never in any mode") now actually holds.
- `ActionProposed` carries `target_bounds` for `invoke` and `drag`,
  closing the overlay presence gap.
- Menu targets under a pinned `window_scope` derive no expectation —
  menu elements are signature-excluded and the menu window can't enter
  the pinned list, so `WorldChanged` would poll for an invisible
  change.
- `click.count` above 1 on a non-left button is refused on macOS and
  browser — a context menu is a single event, and `right x2` no longer
  silently degrades to one `show_menu`/`contextmenu`.
- `element_value` verification is honest under mixed sensitivity: a
  redacted secure-field candidate alongside visible candidates yields
  `uncertain`/`redacted_value`, not `failed` — the hidden value could
  hold the expected text.
- AX `walk` no longer abandons sibling subtrees at the first
  depth-boundary node — depth truncation marks the tree partial but
  every branch within the element budget is still visited; only the
  element cap is a hard stop.
- The unused `AXRoleDescription` attribute was dropped from the
  batched read — one fewer slot fetched per element.
- `Event` records now carry `schema_version: 2` (serde default 1) per
  the agent-contract SDD. The dead `Effect`/`Escalation`/
  `classify_effect`/`classified` vocabulary was removed — the live
  taxonomy is the journal's `effect` string computed from verification
  status.

### Fixed (runtime-reliability-v2 review, round 3)

- Approval fingerprints now bind every non-secret action parameter —
  chord, url (digested; query strings can carry tokens), app selector,
  window operation, mouse button/click count, invoke name, scroll
  delta, drag destination+duration and wait time join kind, mechanism,
  tier, sensitivity, target and payload digest in the canonical tuple.
  A grant covers the action the operator approved, not the class:
  `key "return"` no longer covers `cmd+shift+q`. `needs_approval`
  responses carry the redacted `action` summary so the operator can
  see what they are approving.
- The secrets floor now sees focus-bound routes: `type_text` with no
  target, `key` and explicit `Target::Focused` resolve the focused
  element — a focused password field upgrades to `Secrets` on every
  driver under `mutating = "allow"`. `TargetDescriptor::from_action`
  mirrors the focus resolution `act` performs, so `rule.target`
  matchers and the fingerprint bind the focused element's identity.
- Every engine observation path applies the pinned `window_scope`
  post-driver: a degraded scoped walk (macOS `collect_window`
  fallback, or a driver ignoring `scope.window`) can no longer smuggle
  an app-wide world into verification — `WorldChanged` signatures and
  element checks evaluate only the pinned window, and a vanished pin
  is an honest observe error.
- Sim window ops on an empty window list return `NotFound` instead of
  an index-0 panic unwinding through `execute`.
- AX collection redacts values by the same any-of test
  `Element::is_sensitive` applies (role, subrole *or* raw role) — a
  subrole-only secure field no longer materializes its value at
  collection while policy treats it as secret.
- `menu_item_for_chord` matches on the batched `walk_menu` catalog —
  one IPC roundtrip per menu item instead of ~5 per attribute.
- `Verification::uncertain` requires its `UnknownReason` at
  construction — uncertain-without-a-why is unrepresentable.
- Sim `Scroll` without a target enforces `allow_coordinates` like
  `plan` declares and `Key` already does; `Focus` counts as a
  mutating act for auto-completion (a verified focus can finish a
  `done_when: None` subgoal); scenario `grants` replace
  `approve_all` when declared, so a scenario can assert the
  `needs_approval` outcome.

### Fixed (runtime-reliability-v2 review, round 4)

- Browser `invoke` scroll vocabulary is one name: the walker advertises
  `scroll_into_view` and the executor now maps it — the capability is
  reachable instead of dead in both directions.
- The `__dexter_err` stale sentinel converts to `StaleReference`
  inside `exec_on_args`, not at each call site — targeted `Scroll` (and
  `SetValue`/`TypeText`, now routed through the same helper) can never
  report success on a node that vanished between resolve and dispatch.
- Sim `Click` enforces the contract macOS and browser share: `count`
  is 1..=3 and multi-click is a left-button gesture — the test double
  no longer accepts what production refuses.
- The routing/recovery/desktop-actions SDDs describe shipped types —
  `Sensitivity::{Standard,Secrets,Destructive}`, the real
  `TargetDescriptor`/`ExecutionRoute`/`RunConfig`/`TaskOutcome` shapes
  and wake derived from `needs_stage` — instead of pre-implementation
  sketches (`SensitiveRead`, `verify_attempts`, `WakeMode`).

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
