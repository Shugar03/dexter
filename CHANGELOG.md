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
