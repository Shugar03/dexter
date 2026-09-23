# SDD: eval — offline decision replay

## Contract

An `EvalItem` freezes one decision point: the goal verbatim, the full
`Observation`, and the gold answer. Replaying the item against a
`CandidateGenerator` + `DecisionEngine` pair measures decision quality
without touching the machine — deterministic, hermetic, CI-safe.

```
EvalItem { id, goal, observation, gold, source, meta }
Gold     = Act { target, element } | Route { route } | AnyOf { options }
```

## Metrics — kept separate on purpose

- **coverage** — was the gold element among the generated candidates?
  A generator property: if the right action was never offered, no
  engine can pick it.
- **act-accuracy** — of covered act-golds, how often did the engine pick
  the gold element? An engine property.
- **routes correct** — gold says "don't act"; did the engine route the
  right way? Compared by route *variant* (Wait{500} ≈ Wait{1000} —
  the decision is "wait", the duration is a parameter).
- **false_acts** — engine acted when gold was a route. The dangerous
  direction; should stay 0.
- **false_routes** — engine routed when gold was an action. Over-caution.

## Dataset

`datasets/browser/` — TOML manifest of labeled pages + committed HTML +
harvested `items.jsonl`. Labels are teacher-authored (a human or this
assistant), never inferred from the engine under test.

```
dexter --driver browser --browser-url http://localhost:9515 \
    eval harvest datasets/browser/manifest.toml -o items.jsonl
dexter eval run items.jsonl --engine rule-based
```

`harvest` navigates each page, observes, resolves the declared
`gold.target` to an element id (fail-closed if ambiguous), emits JSONL.
The stored observation lets any future generator be re-measured on the
same frozen worlds.

## Replayability

`CandidatesGenerated` journal events carry the complete
`DecisionContext` (goal, digest, candidate actions+priors, last_error,
step) — live traces are replayable through the same eval path.

## Non-goals

- No end-to-end task success here — that's the sim/scenario layer.
- No model training — this measures decision quality, it doesn't change
  weights.
