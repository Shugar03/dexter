# SDD: Agent-Cost & Recovery Metrics

## Purpose

Dexter's core claim — "the runtime absorbs the loop so the host model
doesn't pay for it" — needs evidence, not assertion. Two measurable
surfaces:

1. **Host-agent cost** — every `dexter_*` tool call is one agent
   round-trip whose response the model must ingest. Counting calls and
   response bytes at the MCP boundary approximates what a session costs
   the agent, and `dexter_task`'s internal step count is the avoided-cost
   number.
2. **Recovery rate** — `RecoveryStarted` is journaled today but nothing
   records whether the recovery worked. `RecoveryCompleted` closes the
   loop so `recovery_rate = completed / started` is a real number.

## Non-goals

- **No token metering of the host agent itself.** Dexter cannot see the
  agent's context or its provider bill — only what crosses the MCP wire.
  Estimates are labelled estimates.
- **No per-tool latency accounting** — the journal already carries
  `observe_ms`/`verify_ms`; session metrics count volume, not time.

## Session metrics (MCP)

`DexterRuntime` holds `SessionMetrics`:

```rust
pub struct SessionMetrics {
    /// Successful tool responses, by tool name.
    pub calls: BTreeMap<String, u64>,
    /// Serialized JSON bytes returned across all responses — the volume
    /// the agent's context absorbs.
    pub response_bytes: u64,
    /// Steps executed inside `dexter_task` — each is an observe/decide/
    /// act/verify cycle that cost the agent zero calls.
    pub task_internal_steps: u64,
}
```

Counting happens at the response boundary (`respond()` — the `v2()`
success path): a call increments `calls[tool]` and
`response_bytes += json_len(payload)` (serialized-size counter, no
extra allocation). Errors that never reach `respond` are not counted —
they are rare and their contribution is noise-level.

`dexter_task` additionally adds its outcome's `steps` to
`task_internal_steps` — the avoided-cost figure.

`dexter_status` exposes:

```json
"session": {
  "tool_calls": {"dexter_observe": 3, "dexter_act": 2},
  "tool_calls_total": 5,
  "response_bytes": 41230,
  "est_response_tokens": 10308,
  "task_internal_steps": 12
}
```

`est_response_tokens` is `response_bytes / 4` — a crude rule-of-thumb
divisor, named `est_` on purpose. It answers "order of magnitude", not
billing. Comparing `tool_calls_total` against `task_internal_steps`
across equivalent work is the honest before/after the thesis needs.

## Recovery completion (engine)

`EventKind::RecoveryCompleted` is emitted at the two points a recovery
actually lands:

- `strategy: "next_route"` — a route following an `Unsupported` verdict
  executes successfully → `outcome: "executed"`.
- `strategy: "verify_poll"` — a poll at `attempt > 1` verifies →
  `outcome: "verified"`.

A recovery that never lands emits no completion — `started − completed`
is the failure count. Nothing is inferred; each completion is emitted at
the observed success point only.

## Eval metrics

- `ScenarioRun.recovery_completed` counts `RecoveryCompleted`.
- `ScenarioMetrics.recovery_rate: Option<f64>` = completed/started when
  any recovery was attempted, else `None` (no opinion).
- `SuiteMetrics` gains `recoveries`/`recoveries_completed`/
  `recovery_rate` rolled up the same way.
- The per-scenario line prints `rec <completed>/<started>`.

## Hard-negative export

`eval scenario --export-negatives <file>` writes rows where
`verified == Some(false)` — candidate decisions whose act provably did
not land, labelled by real verification rather than trajectory outcome.
Unlike `--export`, negatives come from **all** runs (failed tasks
included) and carry `task_success: bool` so the trainer can weight them.
The positive-export contract is unchanged: a failed trajectory still
exports no `--export` rows, because its unverified steps have no label.

## TDD contract

1. A `verify_poll` retry that verifies emits `RecoveryCompleted`
   exactly once.
2. A successful fallback route after `Unsupported` emits
   `RecoveryCompleted{next_route}`.
3. A failed recovery emits no completion — rate < 1.
4. `dexter_status.session` counts calls and bytes; a `dexter_task`
   increments `task_internal_steps`.
5. `--export-negatives` emits only `verified:false` rows, each tagged
   with the run's `task_success`.
