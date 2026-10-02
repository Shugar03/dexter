# Decision cascade — `docs/sdd/cascade.md`

Slice: `--engine cascade` — cheap tiers first, expensive tiers only
when the cheap ones decline (ROADMAP.md Etapa 3, cascada reglas →
Laya → LLM).

## Contract

`Cascade::new(tiers)` is itself a `DecisionEngine` (`name() ==
"cascade"`), so `Engine::run_step`/`run_task`, MCP and eval use it
through the existing seam — no forked path.

`DecisionEngine::decide_traced(&ctx) -> TracedDecision { decision,
hops }` is the traced form of `decide`. The default impl wraps
`decide` in a single hop; `Cascade` overrides it and returns one
`DecisionHop { engine, decision }` per tier consulted, in order
(nested cascades flatten). `TracedDecision::answered_by()` is the
last hop's engine — the tier whose verdict stands.

Escalation, tier by tier:

- `Route::Abstain` or `Route::EscalateLlm` → ask the next tier.
- Any other route (`Wait`, `Retry`, `Reobserve`, `EscalateHuman`) or a
  valid `Act` is final — later tiers are not consulted.
- The last tier's verdict is returned as-is: abstain at every tier →
  `Abstain`.

CLI tiers for `--engine cascade`: `rule-based` → `laya` → `openai`
(each built exactly as its standalone `--engine`; `--engine-path`
configures the laya worker, the OpenAI model comes from
`DEXTER_OPENAI_MODEL`).

## Invariants

- **Never an invented act.** An `Act` from any tier must reference a
  generated candidate (`candidate_index: Some(i)`, `i` in range, same
  action). Anything else decays to `Abstain` for that tier (incoherent
  answers abstain — `docs/sdd/decision.md`) and escalation continues.
- **Errors are not abstentions.** A tier's `DecisionError` propagates —
  the task fails honestly; the cascade never silently falls through to
  another tier on transport/config failure.
- **Policy stays outside.** The cascade only proposes; the engine still
  gates whatever the answering tier picked.
- **Every hop is journaled.** `DecisionMade` carries `"hops"` next to
  `"engine"`; eval `ItemVerdict.tier` records `answered_by()`, so
  reports show which tier answered each item.
- `health()`: `Ready` when every tier is ready, `Down` when every tier
  is down, otherwise `Degraded` naming the impaired tiers.
- An empty cascade is an `Engine` error, never an implicit default.

## Tests

`crates/decision/tests/cascade.rs` (hermetic fakes):
first tier answers → one hop; abstain → escalate → answer; abstain at
every tier → `Abstain`; invented/out-of-range act → abstain + escalate,
never `Act`; non-abstain route is final; tier error propagates without
consulting later tiers; health aggregation; empty cascade errors.
`crates/engine/tests/e2e.rs`: `DecisionMade.hops` journals both tiers.
`crates/eval/tests/replay.rs`: verdict `tier` is the answering tier.
