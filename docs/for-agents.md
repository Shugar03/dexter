# Dexter for agents

Dexter is a computer-use **runtime**, not an agent. It does not replace
your agent loop — it gives it hands. The agent decides what to do;
Dexter observes the world, gates every action through policy, executes
semantically (no mouse capture), verifies the result, and reports back.

## Connect

Any MCP client that supports stdio servers can drive Dexter. The binary
is `dexter` (or `dexter-cu` package name):

```json
{"mcpServers": {"dexter": {"command": "dexter", "args": ["mcp"]}}}
```

Client-specific config locations:

| client | file |
|---|---|
| Claude Code / Desktop | `claude_desktop_config.json` or `.mcp.json` |
| Codex | `~/.codex/config.toml` (`[mcp_servers.dexter]`) |
| Zed | `settings.json` → `context_servers` |
| OpenCode | `opencode.json` → `mcp` |

Useful args: `--driver browser --browser-url http://localhost:9515` for
a WebDriver-backed browser session, `--engine laya --engine-path "…"` to
make `dexter_task` decide with the Laya model instead of the rule-based
baseline, `--min-confidence 0.3` to make low-confidence picks abstain.

## The loop you should run

1. **`dexter_observe {app?, window?}`** — returns `observation` id, a
   text `digest`, a `windows` array (`{id, app, title, bounds,
   on_screen}`) and a structured `elements` array (`{id:"e_4", role,
   name, value, enabled, focused, actions, bounds}`). Element ids are
   scoped to the observation that produced them — a stale id is
   rejected, so re-observe after the world changes. Pass
   `window: <id>` to scope to one window — smaller digest, fewer
   candidates; elements without bounds (menubar items) are dropped.
2. **`dexter_candidates {goal}`** — ranked plausible actions for your
   goal: `[{action, rationale, prior}]`. Priors are heuristic hints, not
   truth — *you* decide. Each `action` is ready-to-pass JSON for
   `dexter_act`.
3. **`dexter_act {action}`** — runs policy → act → verify. Statuses:
   `done`, `needs_approval` (carries a `fingerprint` — a human calls
   `dexter_grant {fingerprint}`, then you retry), `denied`, `failed`,
   `error`.
4. **`dexter_verify {expected}`** — check an `ExpectedState` against a
   fresh observation. Three-valued: VERIFIED / FAILED / UNCERTAIN.
5. **`dexter_task {goal, done, max_steps?, max_secs?}`** — hand the
   whole loop to Dexter when you don't want to drive it yourself.
   Bounds: goal ≤ 4 KB, done ≤ 64 KB, `max_steps` ≤ 200,
   `max_secs` ≤ 3600. Returns `done` / `aborted` / `timed_out` /
   `cancelled`.
6. **`dexter_cancel`** — cooperatively cancels the in-flight task
   (checked between steps; a running `wait` is interrupted).
7. **`dexter_journal`** — live audit trail: `events` (bounded, with a
   `dropped` count when the cap elides old ones) readable *while* a
   task runs.
8. **`dexter_status`** — liveness probe: driver capabilities, decision
   engine health (`ready`/`degraded`/`down` — probe a `laya` worker
   before trusting `dexter_task` with a goal), journal stats, whether a
   task is running. Never blocks on the engine lock.

## Python SDK

`sdk/python/dexter.py` — zero-dependency client over the same MCP
stdio wire:

```python
from dexter import Dexter

with Dexter() as d:                    # spawns `dexter mcp`
    obs = d.observe(app="TextEdit")
    for c in d.candidates("save the document"):
        print(c["action"], c["prior"])
    d.act(d.candidates("save the document")[0]["action"])
    print(d.status()["engine"]["health"])

# Operator trust flags go on the server command:
Dexter(args=("mcp", "--coords"))
```

## Targeting

Prefer semantic over coordinates — it's non-invasive and more robust:

```json
{"type":"click","target":{"name":"Save","role":"button"},"button":"left"}
{"type":"click","target":{"observation":12,"element":"e_4"},"button":"left"}
{"type":"type_text","target":{"focused":null},"text":"hello"}
```

`{"x":..,"y":..}` is **physical-tier**: it moves the real cursor and is
denied by default. Only the *operator* can allow it — launch the server
with `dexter mcp --coords`. Agents cannot request it per call; a
`coords` field in tool arguments is ignored.
Semantic targets (name/element/focused) never touch the user's mouse.

## The intrusiveness contract

Every action reports a tier in the journal:

- `background` — semantic DOM/AX mutation; the user's cursor and focus
  are untouched. Default path.
- `visual` — visible but non-capturing (navigate, focus window).
- `physical` — real pointer/keyboard. Denied by default; the operator
  opts in at server start (`--coords`), then the mutating policy still
  applies (usually `needs_approval` per action). An explicit
  `physical = "deny"` policy always wins over the flag.

While Dexter works, the `dexter-overlay` process can show the agent's
presence — a named floating cursor + state tag — without capturing input.

## Policy

A TOML file (`--policy file.toml`) can scope rules by app, action type,
target pattern and intrusiveness. The embedded default: reads free,
mutations need approval, physical denied. Decisions from `dexter_task`'s
engine are proposals — policy gates every one.
