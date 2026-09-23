# SDD: hardening — efficiency, responsiveness, trust boundaries

Architectural review of the runtime as a tool *for* agents. Verified
strong points first (so the fixes don't regress them), then the weak
points this slice fixes.

## Verified strong

- Policy is fail-closed; engines only propose. Physical-tier actions are
  denied by default and `--approve-all` never covers them.
- `Target::Element` carries the producing `ObservationId` — stale
  references are rejected at resolve time.
- The overlay is click-through and injects no input.
- Eval harness replays frozen worlds; decision quality is measurable.

## Weak points found

### W1. Self-approval bypass (security, critical)

`dexter_act` and `dexter_task` accept `approve`/`approve_all`/`coords`
as tool arguments. The calling agent can therefore self-authorize
mutations and physical input — the `needs_approval` → `dexter_grant`
human-in-the-loop flow is fully bypassable by the very entity it's meant
to constrain.

**Contract**: trust level is set by the operator at server start
(`dexter mcp --approve-all`, `--coords`), never by the agent per call.
Tool schemas lose those params — `approve`/`coords` fields in call
arguments are ignored (serde drops unknown keys). A `needs_approval` is
resolvable only by `dexter_grant` (human) or a policy rule.

**Physical floor is a gate, not the verdict**: `permit_physical`
(`--coords`, or MCP `allow_coords`) lifts the implicit deny, then the
mutating default still applies — a coordinate click lands on
`needs_approval`, not silent execution. An explicit
`physical = "deny"` in the file always wins over the flag.

**Implemented**: `ServerConfig{approve_all, allow_coords}` on
`DexterRuntime`; `serve_stdio`/`DexterMcp::new`/`with_decider` take it;
`dexter mcp` gained `--approve-all`/`--coords`. Tests:
`agent_cannot_self_approve_or_request_physical`,
`operator_opt_in_allows_coords`.

### W2. MCP handlers block the async executor (responsiveness)

Tool handlers run synchronous driver/engine work (`observe` AX walks,
`run_task` minutes-long loops, `thread::sleep` on `Route::Wait`) inside
`async fn` on a current-thread runtime, holding a `std::sync::Mutex`
across the call. One `dexter_task` starves every other tool — even
`dexter_journal` can't be read mid-run.

**Contract**: engine work runs on the blocking pool
(`tokio::task::spawn_blocking`); the journal gets its own lock
(`Arc<Mutex<Journal>>` shared out of `Engine::journal_handle`) so audit
reads stay responsive while a task runs. Engine ops still serialize on
the engine mutex — that's correct (one world), they just don't starve
the runtime.

**Implemented**: `dexter_observe`/`dexter_candidates`/`dexter_act`/
`dexter_verify`/`dexter_task` all `spawn_blocking` engine work;
`dexter_journal` reads the shared handle with zero engine lock.

### W3. Unbounded journal growth (efficiency)

`Engine.events` is an ever-growing `Vec`; a long MCP session never
releases it.

**Contract**: the journal keeps at most `JOURNAL_CAP` (10k) events —
older events drop with a `dropped` count so audits stay honest about
what was elided.

**Implemented**: `Journal{events: VecDeque, dropped: u64}` in
`dexter-engine`; `dexter_journal` returns `dropped`. The file sink
(`set_journal_sink`) still receives *every* event live — the cap only
bounds memory, not the audit trail on disk.

### W4. Release binaries untuned (lightness)

No `[profile.release]`. Ship `lto = "thin"`, `codegen-units = 1`,
`strip = "symbols"` — smaller and faster binaries for
`cargo install` users.

**Implemented**: workspace `[profile.release]` (thin LTO, 1 CGU,
stripped symbols). Binario `dexter` ≈ 9.7 MB.

### W5. No task cancellation

`dexter_task` ran to its step budget; there was no way to stop a
misbehaving loop short of killing the server.

**Contract**: `TaskConfig.cancel` is a cooperative token (shared
`Arc<AtomicBool>`), checked before every observe and inside `Route::Wait`
(25 ms slices — a `wait 30s` is interruptible, not a blind sleep).
Cancellation journals `TaskCancelled` and returns
`TaskOutcome::Cancelled`. `dexter_cancel` flips the token of the
in-flight task; safe no-op when none is running.

**Implemented**: `Engine::run_task` checks `cancel` + deadline each
iteration; `Route::Wait` sleeps in cancellable chunks. Tests:
`task_cancels_via_token`, `wait_is_interruptible`, MCP
`dexter_cancel_stops_a_running_task` (real concurrent cancel against
a stub decider emitting long waits).

### W6. No per-task wall-clock budget

`max_steps` bounded steps but not time; a `Wait`-heavy or driver-stuck
task could run arbitrarily long.

**Contract**: `TaskConfig.max_duration: Option<Duration>` checked at the
top of every iteration alongside cancellation. Expiry journals
`TaskTimedOut` and returns `TaskOutcome::TimedOut { elapsed }`. CLI
`--max-secs`, MCP `max_secs` (capped at 3600).

**Implemented**: engine loop + `dexter task --max-secs` + `TaskParams.
max_secs`. Test: `task_times_out_via_deadline`.

### W7. Unbounded MCP payloads

`goal`, `done`, `action` JSON and `max_steps` were deserialized
unbounded — a hostile or buggy client could force huge allocations or
an effectively infinite task.

**Contract**: `goal` ≤ 4 KB, `done` ≤ 64 KB, `action` ≤ 64 KB,
`max_steps` ≤ 200, `max_secs` ≤ 3600. Violations are `McpError`s, not
tool results — they fail the call before any engine work.

**Implemented**: validated at the top of `dexter_task`/`dexter_act`.
Test: `oversized_goal_is_rejected`, `task_params_are_bounded`.

### W8. Laya worker unsupervised

A crashed or hung sidecar surfaced as a permanent decision error —
the first child process was the engine's only lifetime.

**Contract**: `WorkerProc` owns one child + its pipes; `LayaEngine`
holds it under a mutex with a bounded respawn budget
(`MAX_RESPAWNS = 2`). Transport errors (timeout, broken pipe, EOF) →
respawn + retry the request once. Protocol errors (bad JSON,
`ok: false`, wrong shape) are *not* retried — the transport is alive,
the reply is wrong; retrying masks a provider bug. `recv_timeout`
bounds every read. A crash-loop fails hard after the budget.

**Implemented**: `crates/laya` — `is_transport_error` classifier,
`rpc()` → `rpc_once()` + respawn. Test:
`dead_worker_is_respawned_and_request_retried` (stub that exits once,
then serves).

### W9. `observe` is whole-app only

Every observe paid the full AX walk; agents couldn't ask for just the
dialog they're acting on, and large trees pushed the digest into its
truncation tail.

**Contract**: `dexter_world_model::within_window(obs, window_id)`
returns a scoped `Observation` — `windows` narrowed to the target,
`elements` filtered to bounds intersection with the window rect,
digest rebuilt. Elements without bounds (menubar items) are dropped —
they don't live inside a window. Unknown window id → error, never a
silent empty observation. Exposed as `dexter_observe { window }` and
`dexter observe --window`. `dexter_observe` now returns a structured
`windows[]` (id/app/title/bounds) so agents can pick the id.

**Implemented**: world-model `within_window` + `rects_intersect`;
MCP `ObserveParams.window`; CLI `--window`. Test:
`within_window_scopes_to_intersecting_elements`.

## Remaining gaps (known, not yet scheduled)

- **Incremental observe** — scoping filters *after* the walk; the
  driver still walks the app tree. A future `Driver::observe_window`
  could ask AX for one window subtree only (bigger win on huge apps,
  needs per-driver support).
- **Laya health endpoint** — supervision covers crashes/timeouts, not
  model load progress; no `dexter doctor` check for the worker yet.

## Non-goals

- No new driver capabilities; no policy model changes beyond W1's
  param removal.
- No async rewrite of the engine — it stays sync; MCP bridges via the
  blocking pool.
