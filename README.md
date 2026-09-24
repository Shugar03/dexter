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

Early development (pre-0.1). macOS (Accessibility + CGEvent) and
browser (W3C WebDriver, DOM actions) drivers work; the full closed loop
(observe → candidates → decide → act → verify) runs via CLI and MCP.
A deterministic sim driver backs the hermetic test suite. Windows and
Linux drivers are on the [roadmap](ROADMAP.md).

## Install

### Homebrew (recommended)

```sh
brew install --cask shugar03/dexter/dexter
xattr -d com.apple.quarantine $(which dexter) $(which dexter-overlay)
```

Release binaries are **ad-hoc signed, not notarized** — Gatekeeper
quarantines them on first run; the `xattr` line clears that once.
(The same applies if you downloaded a release tarball directly.)

### From source

```sh
git clone https://github.com/Shugar03/dexter
cd dexter
cargo build --release
# the binary is target/release/dexter
```

Requires macOS 13+ and Rust 1.85+. For real-machine use, grant the
`dexter` binary **Accessibility** and **Screen Recording** in System
Settings → Privacy & Security. Run `dexter doctor` to check.

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

# Offline eval: replay labeled decision points against a decision engine
dexter eval run datasets/browser/items.jsonl --engine rule-based
# Task-level utility: goal-driven tasks end-to-end on sim worlds —
# success rate, steps-over-optimal, phase latency, recoveries. The
# sim-backed scenarios (no [live]/[browser] section) are the hermetic
# subset — they need no OS grants or browser endpoint, so the suite's
# success number is reproducible anywhere.
dexter eval scenario datasets/scenarios --check datasets/scenarios/baseline.toml
# Harvest new labeled items by observing real pages (browser) or apps (macOS)
dexter --driver browser --browser-url http://localhost:9515 \
  eval harvest datasets/browser/manifest.toml -o items.jsonl
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
observation), `focused`, or `point:x,y` (physical tier — `--coords`
plus the normal approval path).

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

# Rules can also scope on mechanism, sensitivity and structured targets.
# Note: [rule.target] opens a sub-table — keep decision/reason above it.
[[rule]]
action = "click"
mechanism = "accessibility"     # coordinate routes never match this
decision = "allow"
[rule.target]
role = "button"

[[rule]]
action = "*"
app = "name:System Settings"
decision = "deny"
reason = "never touch system settings"
```

Run with `dexter --policy dexter.toml <command>`. Without a file, the
embedded policy allows reads and requires approval for every mutation.
Parsing is fail-closed: an unknown or typo'd key is a load error, not a
silently wider rule.

### Intrusiveness tiers

Every action derives an `intrusiveness` from its *target* — never from
the model: element/semantic actions are `background` (DOM/AX mutations,
your cursor never moves), `Navigate`/`Focus`/window targets are `visual`
(visible, captures nothing), and `point:x,y` clicks, key chords and
untargeted typing/scroll are `physical` (real CGEvent input).

Physical is **denied by default** — a batch `--approve-all` never covers
moving your pointer. `--coords` lifts the deny floor for the invocation;
the mutating policy then still applies (a point click typically needs
approval too, or pair with `--approve-all`/`--approve`), or set the tier
in the policy file:

```toml
[defaults]
physical = "require_approval"   # absent = deny

[[rule]]
action = "click"
intrusiveness = "physical"      # rule matcher: background|visual|physical
decision = "allow"
reason = "coordinate clicks approved for this environment"
```

An explicit `physical = "deny"` in the file always wins over `--coords`.

### Presence overlay

`dexter-overlay` draws the agent's presence on screen — a labeled cursor
(gliding to each action's target bounds, tagged `dexter · <state>`) on a
borderless, click-through window. It tails the live journal; it never
injects or captures input.

Presence is **on by default whenever a human is watching**: `click`,
`type`, `task`, `run` and `eval scenario` auto-spawn the overlay when
stderr is an interactive terminal. `--no-overlay` opts out; `--overlay`
forces it on under pipes/CI. `dexter mcp --overlay` gives external
agents the same visible cursor on `dexter_act`/`dexter_task`.

```sh
dexter-overlay --events /tmp/journal.jsonl &   # live tail
dexter task "pay the order" --app Chrome --events /tmp/journal.jsonl
dexter-overlay --events /tmp/journal.jsonl --replay  # replay a journal
```

Physical-tier actions render red with `— physical input` while they run,
so exclusive control is always visible. The overlay exits ~8s after a
terminal state (done / failed / abstained / denied), and also when the
journal's writer dies mid-run — no orphaned windows.

## MCP tools

`dexter_observe`, `dexter_map`, `dexter_candidates`, `dexter_act`,
`dexter_grant`, `dexter_verify`, `dexter_task`, `dexter_cancel`,
`dexter_journal`, `dexter_status`. A `needs_approval` response carries a
fingerprint a human grants via `dexter_grant` — then the agent retries.
Launch with `--no-grants` to remove `dexter_grant` entirely, so an
autonomous agent cannot serve its own human-in-the-loop hook.

`sdk/python/dexter.py` wraps all of it in a zero-dependency Python
client — see `docs/for-agents.md` for per-client MCP configs.

```json
// claude_desktop_config.json
{"mcpServers": {"dexter": {"command": "/path/to/dexter", "args": ["mcp"]}}}
```

`dexter_observe` returns the digest **and** a structured `elements`
array (id/role/name/enabled/bounds) for programmatic targeting.
`dexter_candidates {goal}` returns the ranked action menu — the host
agent stays the decider, Dexter supplies what the world affords.
`dexter mcp --engine laya` makes `dexter_task` decide with the model
(worker spawned once at server start). Full agent-facing guide:
[`docs/for-agents.md`](docs/for-agents.md).

## Decision engines

`run_task` uses `CandidateGenerator` (observation → ranked plausible
actions) + a `DecisionEngine` (pick one or a route:
wait/reobserve/retry/abstain/escalate). Ships with:

- `rule-based` — deterministic baseline, no dependencies.
- `laya` — sidecar worker over NDJSON stdio
  (`workers/laya/worker.py`). Providers:
  - `dev` — deterministic heuristic, labeled, not a model.
  - `laya` — the real model: `pip install laya`, then
    `--engine-path "python3 workers/laya/worker.py --provider laya"`.
    Checkpoints: `--subfolder multilingual` (default; 100+ languages —
    right for localized UIs), `typed-decisions`, or `--subfolder ''` for
    the English root. Model loads once at worker startup; ~160ms/predict
    on Apple-Silicon CPU.

Current measured baseline (same frozen items, `eval run`):

| engine | browser 21 | macOS AX 10 | false_acts |
|---|---|---|---|
| rule-based | 18/18 act + 3/3 routes | 8/8 act + 2/2 routes | 0 |
| laya root (english) | 14/18 act + 1/3 routes | 5/8 act + 0/2 routes | 1 |
| laya root, τ=0.25 | — | 2/8 act + 2/2 routes | 0 |
| laya multilingual | 9/18 act + 1/3 routes | 0/8 act + 2/2 routes | 0 |
| **laya ft (fine-tuned head)** | **18/18 act + 1/3 routes** | **7/8 act + 1/2 routes** | **0** |

The generalist model still trails the tuned heuristic, but the gap is
closing via rendering/protocol levers (digest budget, typed criteria
keys, domain-aware prompt, warmup) — and a **fine-tuned decision head**
(`workers/laya/finetune.py`, data via `eval export`) matches the
rule-based baseline on browser with zero false acts. Honest caveat:
leave-one-domain-out shows no cross-domain transfer yet (browser-only
training scores the base 5/8 on macOS), and fine-tune temperatures may
need recalibration before `--min-confidence` thresholds apply.
`--min-confidence` converts shaky picks into honest abstains — at
τ=0.25 it catches the only false act. Details: `docs/sdd/eval.md`.

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
- `demo/` — animated observability mockups (open `demo/index.html`;
  self-contained, GSAP vendored). Three scenes — semantic target lock,
  agent flight path, verify-or-recover — driven by a real journal
  captured from `dexter task` against live Chrome.
- `site/` — product landing page (open `site/index.html`; static,
  GSAP vendored). The hero replays the presence-cursor concept live.
- `ROADMAP.md` — staged plan through Windows/Linux/enterprise.

## License

MIT OR Apache-2.0, at your option.
