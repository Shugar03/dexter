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

### Fixed

- **Laya worker protocol desync.** Responses were paired positionally:
  a request that timed out still queued its late reply on the channel,
  and the *next* request consumed it as its own (wrong answers or
  phantom "ok but no answers" errors — e.g. after a slow `health()`
  probe). Responses are now matched by request `id`; stale or
  unattributable lines are dropped, while id-less `ok:false` lines
  (worker startup failures) still surface honestly. `worker_cmd` is
  shell-split (`shlex`) so quoted paths with spaces work.

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
