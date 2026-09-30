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

## CLI

`--engine openai` in `task`/`eval`/`doctor --engine`/`mcp`:

- `DEXTER_OPENAI_BASE_URL` — default
  `https://generativelanguage.googleapis.com/v1beta/openai`
- `DEXTER_OPENAI_MODEL` or `--engine-path` — default
  `gemini-2.5-flash-lite` (cheapest flash-lite tier)
- `DEXTER_OPENAI_API_KEY`, else `GEMINI_API_KEY`
