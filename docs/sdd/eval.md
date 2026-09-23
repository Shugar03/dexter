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

## Laya provider notes

- **Digest budget** — `digest_budget(obs, 14_000)` caps the state the
  engine sees (~Laya's 8k-token encoder window) with an explicit
  `... truncated` footer. Runtime and eval share the same budget, so
  frozen items measure what production sends.
- **Calibrated abstention** — the worker forwards Laya's per-pick
  `confidence`; `LayaEngine::with_min_confidence(τ)` turns picks below
  τ into `Route::Abstain`. `--min-confidence` on `task`/`eval run`.
  Measured confidences on the frozen datasets (0.00–0.71) don't cleanly
  separate correct from wrong yet, so the default stays 0 — the gate is
  wired and reported, not enabled blindly.
- **Checkpoints** — `--subfolder multilingual` (mmBERT, localized UIs)
  vs `root` (english). Measured on both frozen datasets:

  | checkpoint | browser act | routes | macos act | routes | false_acts |
  |---|---|---|---|---|---|
  | rule-based | 18/18 | 3/3 | 8/8 | 2/2 | 0 |
  | laya root | 13/18 | 1/3 | 5/8 | 0/2 | 1 |
  | laya multilingual | 4/18 | 2/3 | 0/8 | 2/2 | 0 |

  English checkpoint acts decisively (mostly right); multilingual
  routes almost everything — safe failure mode, near-zero usefulness.
  The single false_act: a legitimately-enabled element the model
  preferred over the gold (candidate filter already drops disabled).

## Non-goals

- No end-to-end task success here — that's the sim/scenario layer.
- No model training — this measures decision quality, it doesn't change
  weights.
