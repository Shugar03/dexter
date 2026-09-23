# SDD: polish — native window observe, engine health, client SDK

Second hardening round. Three items left over from `hardening.md`'s
gap list plus the agent-integration roadmap, all under one contract set.

## N1. Driver-native window scoping (real incremental observe)

W9 filtered *after* the walk — the driver still paid a full AX
traversal. This slice pushes the scope into the driver.

**Contract**

- `ObservationScope.window: Option<u32>` — a hint the driver SHOULD
  honor natively. Semantics match `within_window`: the observation is
  about one window subtree; menubar/unpositioned elements are out of
  scope. Unknown id → `DriverError::NotFound`, never a silent empty
  observation.
- **macOS driver**: resolves the id via `CGWindowList` → bounds, then
  matches the `AXWindow`-ish root whose `AXPosition`/`AXSize` bounds
  equal the CG bounds within ε=2 px (public API only — no private
  `_AXUIElementGetWindow`). First match wins (documented limitation:
  two same-bounds windows are indistinguishable). Walks only that
  subtree — O(window), not O(app).
- **Graceful fallback** (verified live: Finder exposes no `AXWindow`s
  at all): when no AX window matches the bounds, the driver walks the
  full tree and the caller's `scope_to_window` bounds-filter still
  scopes correctly — slower, never wrong.
- **sim driver**: honors the field via `within_window` (keeps the
  contract testable).
- **browser driver**: a session is one document; honors via
  `within_window` for consistency (usually one window anyway).
- **Callers** (CLI `--window`, MCP `window`): pass the id into the
  scope AND post-filter with `within_window` only when the driver did
  not scope — detected by `obs.windows` already being `[target]`.
  Post-filtering a natively-scoped obs would wrongly drop subtree
  elements that overflow the window rect (popovers, menus).

**Tests**: sim honors `scope.window` (scoped elements + window list),
unknown id → error, natively-scoped obs is not re-filtered.

## N2. Engine health — `dexter_status` + doctor

Agents and operators need a liveness probe that doesn't start a task.

**Contract**

- `DecisionEngine::health() -> EngineHealth` — default `Ready` (the
  rule-based engine has no external deps). `EngineHealth` is
  `Ready` / `Degraded(reason)` / `Down(reason)`.
- `LayaEngine::health()` issues one `ping` via `rpc_once` — transport
  failure → `Down`, protocol failure → `Degraded`. **Read-only: it
  never respawns** (health checks must not mutate supervision state);
  `respawns_left` is included in the detail.
- `dexter_status` MCP tool: `{driver: {name, element_tree,
  background_input}, engine: {name, health, detail},
  journal: {events, dropped}, task_running}`.
- `dexter doctor --engine laya --engine-path <cmd>` builds the decider
  and prints its health alongside the permission report.

**Tests**: stub worker → `Ready`; dead worker → `Down` with respawns
untouched; `dexter_status` returns journal counts and engine health.

## N3. Python client SDK (`sdk/python/`)

Harnesses and agents are mostly Python. The SDK is a thin, typed
client over `dexter mcp` stdio — MCP is already the wire, the SDK just
makes it one `import` away.

**Contract**

- `Dexter(cmd=("dexter", "mcp"), args=(), timeout=30)` spawns the
  server, performs the MCP handshake (`initialize` +
  `notifications/initialized`), and exposes one method per tool:
  `observe`, `candidates`, `act`, `verify`, `task`, `cancel`,
  `journal`, `grant`, `status`. Each returns the tool's parsed JSON
  payload; `isError` results raise `DexterError` with the content.
- Framing: newline-delimited JSON-RPC 2.0 (MCP stdio spec). Requests
  carry incrementing ids; the reader skips server→client notifications.
- Every call has a wall-clock timeout — a hung server surfaces as
  `TimeoutError`, never a block. `Dexter` is a context manager and
  `close()` terminates the child.
- Trust model unchanged: trust flags belong to `cmd`/`args` at spawn
  (`args=("--approve-all",)`), not to per-call params.

**Tests**: against a fake MCP server subprocess exercising handshake,
call round-trip, error results, notification skipping, timeout. An
integration test against the real binary runs only when `DEXTER_BIN`
is set.

## Non-goals

- No `Driver::observe_window` trait method — `ObservationScope.window`
  on the existing `observe` keeps the seam to one method.
- No TS SDK yet — Python covers the agent/harness demographic; the MCP
  wire makes a TS port a straight transliteration when needed.
- No streaming task progress — `dexter_journal` polling is the
  documented live-monitoring path.
