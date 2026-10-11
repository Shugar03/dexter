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
  engine can pick it. Only act-golds feed it (`covered / act_items`) —
  a route-gold is scoreable no matter what was generated, so counting
  it inflated coverage by construction.
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
  vs `root` (english). Measured on both frozen datasets, criteria keys
  typed `c{i}`/`r{i}`:

  | checkpoint | browser act | routes | macos act | routes | false_acts |
  |---|---|---|---|---|---|
  | rule-based | 18/18 | 3/3 | 8/8 | 2/2 | 0 |
  | laya root | 14/18 | 1/3 | 5/8 | 0/2 | 1 |
  | laya root, τ=0.25 | — | — | 2/8 | 2/2 | 0 |
  | laya multilingual | 9/18 | 1/3 | 0/8 | 2/2 | 0 |

  English checkpoint acts decisively (mostly right); multilingual still
  routes on macOS but descriptive keys doubled its browser accuracy
  (4→9). τ=0.25 shows the abstention gate working on real data: it
  caught the one false_act (enabled "Abrir" over the gold route) and
  kept routes 2/2 — at the cost of 3 correct acts. That's the honest
  trade-off knob.

## Fine-tuned head (`workers/laya/finetune.py`)

`eval export` emits training rows with gold labels (gold element →
candidate index, gold route → `r{i}` key). The trainer freezes the
encoder, caches hidden states, augments with option-order permutations,
and trains the decision head with the calibration-aware proper-reward
loss (`--device cpu|mps|cuda`, `--epochs`, `--perms`, `--holdout`).

Measured on both frozen datasets (root/english base):

| checkpoint | browser act | routes | macos act | routes | false_acts |
|---|---|---|---|---|---|
| laya root (base) | 14/18 | 1/3 | 5/8 | 0/2 | 1 |
| **ft mixed** (train on all 31) | **18/18** | 1/3 | **7/8** | 1/2 | **0** |
| **ft browser-only** → macOS | 17/18 | 1/3 | 5/8 | 0/2 | 1 |

Honest reads:

- **In-domain transfer works**: browser-only training held 17/18 on
  browser and mixed training hit 18/18 + 7/8 with zero false acts.
- **Cross-domain does not transfer**: browser-only → macOS scored
  exactly the base model's 5/8 with the same false_act — the head
  learned browser-page patterns, not AX-tree picking. No regression,
  no gain. Claims of generalization need per-domain data.
- The remaining misses are route-granularity (`Reobserve`/`Wait` vs
  `Abstain`) and one wrong-element pick per dataset — safe failure
  modes, not unsafe actions.
- Confidence calibration after fine-tune is not yet re-measured — the
  checkpoint warns its temperatures may sit outside the calibrated
  range, so `--min-confidence` thresholds should be re-derived per
  checkpoint, not inherited.

## Cross-app generalization (`eval matrix`, `scripts/cross_app_eval.sh`)

Three frozen surfaces, grouped by provenance (`split_by_app`:
`meta.app → meta.url → observation.app`):

- `datasets/browser/` — 21 items, 16 distinct pages (DOM surface).
- `datasets/macos/` — 10 items, TextEdit + Finder (AX surface).
- `datasets/sim/` — 11 synthetic items: installer wizard, media
  player, file manager (`datasets/sim/gen.py` regenerates).
- `datasets/vision/` — 5 synthetic items mixing menu-only AX shells
  with inert `[ocr]` elements (`datasets/vision/gen.py`).

`eval matrix` reports per-app rows per dataset — the row for a held-out
group IS the transfer measurement; `eval export` stamps the same
`"app"` key on training rows so `cross_app_eval.sh` can train
minus-that-group (LODO) and re-measure.

**Environment caveat (measured 2026-09)**: harvesting new macOS items
needs the responsible process to hold the full AX grant — in a
background/agent session every stock app returns menu-only trees
(`ax_limited`) and windows report `on_screen: false` (captures come
back black). The existing macos items were harvested under a granted
context; sim/vision datasets are synthetic precisely so the matrix is
CI-reproducible.

### Measured (laya root, `eval matrix`)

| engine | browser | macos | sim | vision |
|---|---|---|---|---|
| rule-based | 18/18 + 3/3r | 8/8 + 2/2r | 8/9 + 1/2r, fr 1 | 1/1 + 1/4r, **fa 1** |
| laya base | (14/18 + 1/3r) | (5/8 + 0/2r) | 4/9 + 1/2r, fr 5 | 1/1 + 2/4r, **fa 1** |
| laya ft mixed | 18/18 + 1/3r | 7/8 + 1/2r | **4/9 + 1/2r, fr 5** | **1/1 + 1/4r, fa 1** |

Honest reads:

- **Cross-domain transfer is nil — confirmed twice more.** ft-mixed
  (trained on all 31 browser+macos rows) reproduces the base model's
  numbers on sim and vision *exactly* — same misses, same false_act.
  The head learns the distribution it saw, not "computer use".
- **The false_act is shared across engines**: on
  `ocr-save-evidence-only` both rule-based and laya act on a stray
  enabled element instead of abstaining — OCR text is evidence, not a
  handle. This is the single most valuable dataset row: it catches the
  dangerous direction.
- **LODO confirms it.** Leave-one-domain-out checkpoints
  (`scripts/cross_app_eval.sh`, 25 epochs / 6 perms / 15% holdout):

  | holdout | train rows | held-out result | base result |
  |---|---|---|---|
  | sim | 36 | 4/9 + 1/2r, fr 5, fa 0 | 4/9 + 1/2r, fr 5, fa 0 |
  | vision | 41 | 1/1 + 2/4r, fa 1 | 1/1 + 2/4r, fa 1 |

  Training on three surfaces buys exactly nothing on the fourth —
  the LODO scores equal the base model's to the item. Per-surface
  data is the only thing that moves a surface's numbers. The two
  within-AX-surface holdouts (Finder, TextEdit) were dropped as
  uninformative vs the cross-surface question.

- Trainer note: concurrent `finetune.py` processes stall on MPS
  (Metal device contention serializes to ~0% CPU); use
  `--device cpu` for parallel LODO runs.

## MLX fine-tune (`workers/laya/finetune_mlx.py`, `--provider laya-mlx`)

Same recipe as `finetune.py` ported to MLX for the `laya-mlx` package
(Apple silicon port of the Laya decision models, no torch). The worker
exposes it as `--provider laya-mlx` (same NDJSON protocol, provider
label `laya-mlx`); checkpoints load via `--model <dir|hub-id>
--subfolder root` — `root` maps to None because MLX hub repos are flat
(no `multilingual/` subfolder). Train:

```sh
dexter eval export datasets/*/items.jsonl -o /tmp/rows.jsonl
python3 workers/laya/finetune_mlx.py /tmp/rows.jsonl \
    --out /tmp/ckpt --holdout 0.2 --device cpu
```

Base = `aac6fef/laya-mlx` (English, ModernBERT-large encoder, ~26.5M
trainable head params — encoder stays frozen). `--holdout-app <prefix>`
holds out whole provenance groups for LOAO runs; `--device cpu`
because the paravirt GPU stalls in the Metal allocator during training.

Measured on the frozen datasets (macOS, Apple silicon, MLX 0.32.3,
laya-mlx 0.4.0; argmax act accuracy unless noted):

| engine | browser | macos | sim | vision |
|---|---|---|---|---|
| laya-mlx base | 22/31 + 1/5r, fr 2 | 7/15 + 0/5r, fa 1 | 4/9 + 1/2r, fr 5 | 1/1 + 2/4r, fa 1 |
| laya-mlx ft, **in-sample** | 31/31 + 5/5r | 14/15 + 5/5r | 9/9 + 2/2r | 1/1 + 4/4r |

In-sample the head memorizes the set (~99% train accuracy) — expected
with a frozen encoder on 72 rows; the honest question is held-out.

Held-out splits (each number's split named):

| split | held out | base | ft |
|---|---|---|---|
| random 20% (14 items, all datasets) | 9 acts + 5 routes | 7/9 + 1/5r, fa 1 | **6/9** + 1/5r, fa 1 |
| LOAO `sim/` (whole surface) | sim, 11 items | 4/9 + 1/2r, fr 5 | **8/9** + 0/2r, **fr 1** |
| LOAO `vision/` (whole surface) | vision, 5 items | 1/1 + 2/4r, fa 1 | 1/1 + **1/4r**, fa 1 |
| LOAO `com.apple.calculator` | 4 of 20 macos items | 1/3 + 0/1r | 1/3 + **1/1r** |
| LOAO browser `checkout.html` | 2 of 36 items | 1/2 | **2/2** |

Honest reads:

- **Inconsistent held-out transfer.** Fine-tuning moved sim up hard
  (+4 acts, −4 false routes) and nudged calculator/checkout, but lost
  an act on the random holdout and a route on vision. Within-surface
  LOAO barely transfers (calculator same acts, +1 route); cross-surface
  LOAO is a wash on vision and a real but partial win on sim.
- **The random-holdout regression matters**: ft-indomain trains on 58
  rows yet scores *below* base on unseen items from the same
  distribution — the head overfits row idiosyncrasies, not surface
  patterns. That is the same "learns what it saw" shape as the torch
  LODO above, at smaller scale.
- **τ post-ft does not isolate the false act** (sweep on the 14-item
  holdout: utility flat at 0/0.25, a false route appears at 0.5) —
  per-checkpoint calibration still needed before enabling
  `--min-confidence`.
- **Verdict: not shipped as default.** 72 rows overfit a 26.5M head;
  per-surface data at 10× this volume is the obvious next lever. The
  tooling is the keepable artifact: `finetune_mlx.py`, the `laya-mlx`
  worker provider, and `holdout_app`/`split_rows` in `rows.py` make
  the honest-split protocol one command per holdout.

## Non-goals

- No end-to-end task success in this layer — that's
  `docs/sdd/scenario-eval.md` (`dexter eval scenario`), which replays
  whole tasks through `run_task` and reports success, steps-over-
  optimal, phase latencies, and recovery counts.
