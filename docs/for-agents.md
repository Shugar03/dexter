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

1. **`dexter_observe {app?, window?, vision?, include_menu?}`** — returns
   `observation` id, a text `digest`, a `windows` array (`{id, app,
   title, bounds, on_screen}`) and a structured `elements` array
   (`{id:"e_4", role, source, name, value, enabled, focused, actions,
   bounds}`). Element ids are scoped to the observation that produced
   them — a stale id is rejected, so re-observe after the world
   changes. Pass `window: <id>` to scope to one window — smaller
   digest, fewer candidates; elements without bounds (menubar items)
   are dropped. Pass `include_menu: false` when you only need window
   controls — the menu catalog often outnumbers real elements ~10:1
   and dominates observe cost. Pass `vision: true` when the digest is
   thin or empty (`ax_limited`) — on-device OCR of the target window
   appends `[ocr]` elements. They are *evidence only*: no actions, no
   live handle — clicking one means `Target::Point` at its bounds
   center, which stays approval/policy-gated.
2. **`dexter_candidates {goal}`** — ranked plausible actions for your
   goal: `[{action, rationale, prior}]`. Priors are heuristic hints, not
   truth — *you* decide. Each `action` is ready-to-pass JSON for
   `dexter_act`.
3. **`dexter_act {action}`** — runs policy → act → verify. Statuses:
   `done`, `needs_approval` (carries a `fingerprint` plus the redacted
   `action` summary — a human calls `dexter_grant {fingerprint}`, then
   you retry), `denied`, `failed`, `error`. The grant binds the exact
   action: kind, mechanism, tier, sensitivity, resolved target
   identity, every non-secret parameter (chord, url, app, window op,
   button/click count, invoke name, deltas, drag destination, wait
   duration) and a digest of the payload — approving `key "return"`
   never covers `cmd+shift+q`.
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
{"type":"focus","target":{"window_id":2}}
```

`{"window_id":N}` targets a whole window — on the **browser driver each
tab is a window**, so `focus` on it switches tabs (an API switch, not
pointer input). `dexter_observe{window:N}` observes that tab directly.
Element refs are per-tab: an `{"observation","element"}` pair issued
while a *different* tab is active is rejected as stale — switch to the
window or re-observe. Same-origin iframes are flattened into the
observation (a `web_area` node with its descendants); cross-origin
frames count toward `collection_errors` — their content is invisible,
not silently absent.

`{"x":..,"y":..}` is **physical-tier**: it moves the real cursor and is
denied by default. Only the *operator* can allow it — launch the server
with `dexter mcp --coords`. Agents cannot request it per call; a
`coords` field in tool arguments is ignored.
Semantic targets (name/element/focused) never touch the user's mouse.

## Action vocabulary

The full `Action` wire format (`dexter_act {action}` or CLI
equivalents) — everything not listed under *physical* below stays in
the background tier:

```json
{"type":"click","target":{...},"button":"left","count":1}
{"type":"type_text","target":{...},"text":"hello"}
{"type":"key","chord":{"key":"s","modifiers":["cmd"]}}
{"type":"scroll","delta":{"dx":0,"dy":-3},"target":{...}}
{"type":"focus","target":{...}}
{"type":"set_value","target":{...},"value":"42"}
{"type":"observe"}
{"type":"wait","millis":500}
{"type":"navigate","url":"https://example.com"}
{"type":"invoke","target":{...},"action":"open"}
{"type":"launch_app","app":{"by":"name","value":"Calculator"},"activate":true}
{"type":"quit_app","app":{"by":"bundle_id","value":"com.apple.calculator"}}
{"type":"window","window_id":null,"operation":{"op":"close"}}
{"type":"read_clipboard_text"}
{"type":"write_clipboard_text","text":"..."}
{"type":"drag","from":{...},"to":{...},"duration_ms":300}
```

Notes worth knowing:

- **`click.count` 2–3** plans the element's advertised `open` action
  when it exists (double-click = open); a physical multi-click is the
  gated last resort. `count` above 1 on a non-left button is refused —
  a context menu is a single event, there is no double right-click.
- **`key`** tries a semantic menu route first: the chord is matched
  against menu items' advertised shortcuts and `AXPress`ed — the
  physical keyboard is only used when no menu advertises the chord
  (and `--coords` is set).
- **`invoke`** performs an action the element itself advertises —
  `open`, `confirm`, `cancel`, `pick`, `show_menu`... The element's
  `actions` list in `dexter_observe` tells you what exists; an
  unadvertised name fails closed, never guesses.
- **`window`** operations: `{"op":"new" | "focus" | "raise" | "close" |
  "minimize" | "restore" | "move" {"x","y"} | "resize" {"width","height"}}`.
  `window_id: null` targets the scoped app's frontmost window.
- **`app` selectors** are tagged: `{"by":"name"|"bundle_id"|"pid",
  "value":...}` — a bundle id can never be confused with a display name.
- **`launch_app`/`quit_app`** verify by world change — a localized app
  name ("Calculadora") still verifies even if you launched
  "Calculator". Launch by bundle id when you can.
- **`read_clipboard_text`/`write_clipboard_text`** carry a secrets
  floor: they need approval even when ordinary mutations are allowed,
  and the clipboard content never reaches the journal.
- **`drag`** resolves and validates both endpoints *before* the
  pointer moves — a stale endpoint aborts cleanly.
- **Mutating actions with a derivable effect are verified**: after
  acting, Dexter re-observes and checks the world actually changed (or
  the derived expectation holds). A no-op reports
  `failed`/`suspected_noop`, never silent success — trust
  `status: "done"` with `verification` present to mean *observed*
  success. Derived verification covers `click` on element targets,
  `set_value`, `type_text`, `focus`, `invoke`, `drag`, `launch_app`
  and `quit_app`.
- **Some actions are intentionally unverifiable** — the world model
  cannot express a reliable effect for them, so they run with
  `verification: null` rather than a fake check: `window` ops,
  clipboard reads/writes, `key`, `scroll`, `navigate`, `wait`,
  `click` on raw `{"point"}` coordinates, and menu elements under a
  pinned `--window` scope (the menu's effect lives outside the pinned
  window). Where the API accepts an explicit `expect`, you may still
  supply one.
- **A delivery error is verified before it is reported**: when
  `execute` fails but an expectation exists — a timed-out reply can
  postdate the side effect — Dexter runs the verify poll first. If the
  expected state holds, the step completes with `result: null` and the
  verification as the verdict; only then does an unverifiable error
  surface.
- **Secure fields are unverifiable by design**: `type_text`/`set_value`
  into a password or other secure field never derives a value
  expectation — the field's value is redacted at collection, so a
  successful entry reports `verification: null` and is never retried
  (a retry would append the secret twice). An explicit value
  expectation there reports `uncertain` (`redacted_value`), not
  `failed`.
- **A failed observation is journaled, not hidden**: if the pre-action
  `observe` itself fails, the act proceeds unverified and the journal
  records `observation_failed` (`pre_act`/`post_wake`/`goal_start`) —
  an unverified act can never be mistaken for a verified one.
- **Mechanism is part of the authorization**: routes declare the
  mechanism they'll take (`accessibility`, `dom`, `api`,
  `coordinates`...); if execution produces a different one, the step
  fails — a planned semantic act never silently becomes physical
  input. (`mechanism: null` is a compatibility seam for unmigrated
  drivers: policy still gates their intrusiveness, but the mechanism
  itself can't be fingerprinted — don't rely on it for new drivers.)

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

## Recording trajectories

`dexter task ... --export rows.jsonl` writes the run's decisions as
Laya training rows — each act labelled with its verified outcome.
Nothing is written when the task fails: a mislabeled trajectory is
worse than none. `--events run.jsonl` dumps the raw journal instead.
Action payloads are digest tokens in every mode; the *goal text*
itself is journaled verbatim — don't embed secrets in goals.

## Policy

A TOML file (`--policy file.toml`) can scope rules by app, action type,
target pattern and intrusiveness. The embedded default: reads free,
mutations need approval, physical denied. Decisions from `dexter_task`'s
engine are proposals — policy gates every one.
