# SDD: threshold calibration

Slice: sweep the decision knobs over the frozen datasets and pick the
operating point with a stated, fail-closed rule (ROADMAP.md Etapa 3 —
calibración de umbrales).

## Knobs

- `RuleBased.act_threshold` — minimum prior of the top generated
  candidate for the rule-based engine to act (default 0.65). Below it
  the engine abstains. It is the cut over the generator's priors; it
  is also the cascade's first tier.
- `LayaEngine.min_confidence` (τ) — picks whose calibrated confidence
  is below τ become `Abstain` (default 0 = off). Answers without a
  confidence (the `dev` provider) are never gated.

Neither is policy: policy still gates every action the engine proposes.

## Contract

```
dexter_eval::calibrate::sweep(items, generator, values, make)
    -> Result<Vec<SweepPoint { value, report }>, E>
sweep_act_threshold(items, generator, thresholds) -> Vec<SweepPoint>
SweepPoint::score()   = correct + routes_correct
SweepPoint::utility() = score − FALSE_ACT_COST(2) · false_acts
pick(points) -> Option<f32>
```

`make(value)` builds the engine for one value; a build error aborts
the sweep (no silently missing rows).

`pick` order: highest `utility` → fewest `false_acts` → highest value.
A false act costs more than a correct answer earns (it displaced a
correct route *and* touched the world), so accuracy bought with false
acts loses. Pure false-acts-first is rejected: on the frozen sets it
picks `act_threshold = 1.0`, i.e. abstain on nearly everything — that
switches the engine off rather than calibrating it. Ties go to the more
conservative (higher) value for both knobs.

## CLI

```
dexter eval matrix datasets/*/items.jsonl --act-threshold 0.5,0.65,0.8
dexter eval matrix datasets/*/items.jsonl --engine laya --min-confidence 0,0.25,0.5
```

Comma-separated values sweep the knob: one per-app matrix block per
value, then one row per value over all datasets with its utility, then
the pick. One knob at a time — sweeping both is rejected (the pick is
1-D). `cargo run -p dexter-eval --example calibrate` prints the same
rule-based sweep as a markdown table and runs on any host.

## Invariants

- The default only moves when the sweep's pick moves — and the move
  ships with its before/after table in `docs/eval-numbers.md`.
- Thresholds are re-derived per Laya checkpoint (fine-tuned heads have
  different confidence scales), never inherited.
