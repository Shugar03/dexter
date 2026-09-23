# Dexter

**Local-first agent computer runtime.** Dexter gives AI agents reliable
control over real computers — starting with macOS — by combining semantic
perception, explicit policy, executed actions and *verified* results.

> The LLM decides **what** it wants to achieve. Dexter decides **how** to
> interact with the computer, executes the action, checks the result, and
> recovers within bounds if it fails.

This is not a screenshot-and-mouse-move toy:

- **Semantic first.** Actions target elements by role/name through the
  Accessibility tree. Coordinates exist — as an explicitly permitted
  fallback, never silently.
- **Verified, not claimed.** Every action can carry a post-condition
  (`ExpectedState`) checked against a *fresh* observation. Results are
  `VERIFIED` / `FAILED` / `UNCERTAIN` — a truncated tree can never
  produce a false `VERIFIED`.
- **Fail-closed policy.** A TOML policy outside the model decides
  allow / deny / require-approval per action and app. Mutations default
  to *require approval*. Approvals are fingerprint-bound to the exact
  action + app, single-use, and TTL-limited.
- **Honest degradation.** When macOS serves a degraded AX tree (granted
  to the terminal but not the binary — common on macOS 26), observations
  carry `ax_limited` instead of pretending completeness.
- **One path.** CLI, MCP server and SDKs all go through the same
  `Engine::run_step` — policy → act → re-observe → verify → bounded
  retry, with a structured JSONL event journal.

## Status

Early development (pre-0.1). macOS driver works; the full closed loop
(observe → candidates → decide → act → verify) runs via CLI and MCP.
Browser, Windows and Linux drivers are on the
[roadmap](ROADMAP.md).

## Install

```sh
git clone https://github.com/Shugar03/dexter
cd dexter
cargo build --release
# the binary is target/release/dexter
```

Requires macOS and Rust 1.85+. For real-machine use, grant the `dexter`
binary **Accessibility** and **Screen Recording** in System Settings →
Privacy & Security. Run `dexter doctor` to check.

**Stable identity matters:** an adhoc signature changes on every build
and TCC serves a *degraded* AX tree to it (observations report
`ax_limited`). Sign once with a self-signed cert so grants stick:

```sh
./scripts/devsign.sh release   # one-time cert setup documented inside
dexter doctor                  # grants now apply to com.dexter.cli
```

## Quickstart

```sh
# What can this binary do, on this machine, right now?
dexter doctor

# Windows + AX tree + digest for an app
dexter observe --app "TextEdit" --digest

# One action through the full path (policy will require approval)
dexter click --app "TextEdit" --target '{"role":"button","name":"Save"}'
# -> {"status":"needs_approval","fingerprint":"..."}

# Same, approved by the human running the command
dexter click --app "TextEdit" --target '{"role":"button","name":"Save"}' --approve

# A scenario: ordered steps with post-conditions, journal to JSONL
dexter run examples/textedit-hide.toml --approve-all --events out.jsonl

# A goal in closed loop: observe -> candidates -> decide -> act -> verify
dexter task "hide textedit" \
  --done '{"type":"element_exists","target":{"name":"Ocultar Editor de Texto"}}' \
  --app "TextEdit" --approve-all

# MCP server over stdio (Claude Desktop / MCP clients)
dexter mcp
```

### Browser (Safari / any WebDriver)

```sh
# one-time: Safari Settings → Developer → "Allow Remote Automation"
# (or `sudo safaridriver --enable`)

# a full scenario in one session — navigate, fill, click, verify
dexter --driver browser run examples/browser-form.toml --approve-all

# persistent session for agents — browser stays open across tool calls
dexter --driver browser mcp

# attach to a running WebDriver endpoint instead of spawning safaridriver
dexter --driver browser --browser-url http://localhost:9515 observe
```

Browser actions dispatch inside the page (`el.click()`, `el.value=`)
as `Mechanism::Dom` — no coordinates, no cursor, works on occluded or
background windows. `Target::Point` reports `unsupported` honestly.

Each CLI invocation is a fresh session; use `run`/`task`/`mcp` for
multi-step flows (see `docs/sdd/browser.md`).

Targets: `{"role":"button","name":"Save"}` semantic JSON (also
`name_contains`, `identifier`, `index`), `element:N` (from a fresh
observation), `focused`, or `point:x,y` (requires `--coords`).

## Policy

```toml
# dexter.toml — first match wins; unmatched mutating actions need approval
[[rule]]
action = "observe"
decision = "allow"

[[rule]]
action = "click"
app = "bundle:com.apple.Terminal"
decision = "allow"

[[rule]]
action = "*"
app = "name:System Settings"
decision = "deny"
reason = "never touch system settings"
```

Run with `dexter --policy dexter.toml <command>`. Without a file, the
embedded policy allows reads and requires approval for every mutation.

## MCP tools

`dexter_observe`, `dexter_act`, `dexter_grant`, `dexter_verify`,
`dexter_task`, `dexter_journal`. A `needs_approval` response carries a
fingerprint a human grants via `dexter_grant` — then the agent retries.

```json
// claude_desktop_config.json
{"mcpServers": {"dexter": {"command": "/path/to/dexter", "args": ["mcp"]}}}
```

## Decision engines

`run_task` uses `CandidateGenerator` (observation → ranked plausible
actions) + a `DecisionEngine` (pick one or a route:
wait/reobserve/retry/abstain/escalate). Ships with:

- `rule-based` — deterministic baseline, no dependencies.
- `laya` — sidecar worker over NDJSON stdio
  (`workers/laya/worker.py`; real SDK wiring pending — the `dev`
  provider is labeled and deterministic, not a model).

Decision engines only *propose*. Policy still gates every action.

## Architecture

```
apps/dexter        CLI + MCP entry (dexter-cu package)
crates/core        normalized types: Action, Target, ExpectedState, Event
crates/driver      ComputerDriver seam: capabilities/windows/observe/act
crates/world-model semantic queries + text digest (decision input)
crates/policy      fail-closed TOML rules + scoped approvals
crates/verify      three-valued ExpectedState verification
crates/decision    CandidateGenerator + DecisionEngine seam + RuleBased
crates/laya        LayaEngine — NDJSON sidecar protocol
crates/engine      the loop: policy -> act -> re-observe -> verify -> retry
crates/mcp         rmcp-based stdio server
drivers/macos      AX + CGEvent + CGWindowList + xcap implementation
drivers/browser    W3C WebDriver REST — safaridriver/chromedriver, DOM actions
drivers/sim        deterministic synthetic driver (test/dev/simulation)
workers/laya       Python NDJSON worker (laya + dev providers)
```

Invariants enforced by tests, not by docs:

- never simulate success;
- ambiguous or stale targets fail closed;
- coordinate input requires an explicit opt-in at the call site;
- `UNCERTAIN` never counts as `VERIFIED`;
- approvals bind the exact action fingerprint, once, inside a TTL.

## Documentation

- `docs/agent-computer-runtime.md` — the runtime contract (what/why/how).
- `docs/ml-architecture-v3.md` — decision-model architecture (Laya
  lessons, candidate scoring, data flywheel) — informs `crates/decision`.
- `docs/sdd/` — per-slice design contracts.
- `docs/sdd/browser.md` — WebDriver driver design + Safari setup.
- `ROADMAP.md` — staged plan through Windows/Linux/enterprise.

## License

MIT OR Apache-2.0, at your option.
