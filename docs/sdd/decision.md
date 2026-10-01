# Decision providers — `docs/sdd/decision.md`

Slice: real decision layer behind the `DecisionEngine` seam.

## Contract

`DecisionEngine::decide(&DecisionContext) -> Result<Decision, DecisionError>`
is the only decision surface. `DecisionContext` already carries everything
a provider needs (goal, `state_digest`, ranked `candidates`,
`last_error`, `step`) — providers add zero new state to the loop.

Providers shipped:

- `RuleBased` — embedded deterministic decider (default).
- `LayaEngine` — Python sidecar over NDJSON.
- `OpenAiProvider` — any OpenAI-compatible chat-completions endpoint:
  Gemini (`v1beta/openai`), OpenAI proper, local `llama.cpp`, etc.
  Config: `base_url`, `model`, `api_key_env` (the *name* of the env var
  holding the key — read per request, never logged).
- `Cascade` — escalating composition of tiers in order
  (`--engine cascade` = `RuleBased → LayaEngine → OpenAiProvider`):
  a tier's `Abstain`/`EscalateLlm` route or an engine error hands the
  step to the next tier; every other decision is final. The last
  tier's verdict always stands — a final abstain or escalation
  surfaces to the runtime as usual.

## Invariants

- **The model never invents actions.** It may only pick one of the
  already-generated candidates by index, or take a route
  (`abstain` | `retry` | `reobserve` | `{"wait": ms}`).
- **Incoherent answers decay to `Abstain`** — unparseable JSON, an
  out-of-range index, an unknown route. The model answered, the answer
  wasn't actionable; abstaining is the honest fail-closed move. Policy
  still gates whatever the model does pick.
- **Transport/config failures are `DecisionError::Engine`** — the task
  fails honestly, never a silent fallback to another engine.
- **Spend is bounded by construction:** ≤8 candidates, ≤1500-char
  digest, `max_tokens` 150, `temperature` 0. `health()` does not make
  live calls (env-var presence check only — probes would burn tokens).
- **Cascade escalation is never silent.** The returned rationale
  carries the chain (`rule-based: Abstain — … | laya: error — … →
  openai …`), so the journal shows why a heavier tier answered and
  what a broken middle tier did. `health()` aggregates: `Down` only
  when every tier is down; a single impaired tier degrades the report.

## Tests (crates/decision/tests/openai.rs)

Hermetic — a local `TcpListener` serves canned replies:

- `act_reply_maps_to_candidate` — `{"act":0}` → `Decision::Act` on
  candidate 0; bearer header and model name verified on the wire.
- `route_reply_maps_to_route` — `{"route":{"wait":500}}` →
  `Route::Wait`.
- `garbage_reply_abstains` / `out_of_range_candidate_abstains` —
  fail-closed on incoherent answers.
- `http_error_is_engine_error` — 4xx/5xx → `Engine` (task fails).
- `missing_key_is_engine_error_and_down_health` — no env key → `Down`
  health + `Engine` error.

`crates/decision/tests/cascade.rs` covers the composition with stub
engines: first-tier `Act` is final (later tiers uncalled), `Abstain`
and `EscalateLlm` escalate with the chain appended to the rationale,
`Wait`/`EscalateHuman` are final, mid-tier errors escalate and
last-tier errors surface with the trail, a fully-abstaining cascade
returns the last tier's `Abstain`, and `health()` aggregates
(all-down → `Down`, partial → `Degraded`).

## CLI

`--engine cascade` wires `RuleBased → LayaEngine → OpenAiProvider`:
`--engine-path`/`DEXTER_LAYA_WORKER` selects the laya worker command
(the laya tier fails to build without it — the cascade is explicit,
never a dropped tier), `--min-confidence` feeds the laya tier, and the
openai tier reads the env config below.

`--engine openai` in `task`/`eval`/`doctor --engine`/`mcp`:

- `DEXTER_OPENAI_BASE_URL` — default
  `https://generativelanguage.googleapis.com/v1beta/openai`
- `DEXTER_OPENAI_MODEL` or `--engine-path` — default
  `gemini-2.5-flash-lite` (cheapest flash-lite tier)
- `DEXTER_OPENAI_API_KEY`, else `GEMINI_API_KEY`
