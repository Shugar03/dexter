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
stripped symbols).

## Non-goals

- No new driver capabilities; no policy model changes beyond W1's
  param removal.
- No async rewrite of the engine — it stays sync; MCP bridges via the
  blocking pool.
