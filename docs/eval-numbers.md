# Eval numbers

Published, reproducible numbers for the task-level suite — the
OSWorld-style denominator: did the task complete, honestly and
efficiently, not just "did the model answer".

## Sim suite (rule-based engine, 5 reps)

Regenerate with:

```sh
cargo run -p dexter-eval --example suite_report 5
```

Measured on Linux @ `main` (2026-10-05):

| scenario | outcome | steps | over-opt | decide p50/p95 ms | recoveries | phys |
|---|---|---|---|---|---|---|
| admin-absent | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| ambiguous-twin | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| cancel-polarity | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| dead-end-save | completed | 2.0 | 1.0 | 0/0 | 0 | 0 |
| download-wait | completed | 1.0 | 0.0 | 0/0 | 0 | 0 |
| field-disabled | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| files-open-dialog | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| form-fill | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| modal-blocking | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| modal-confirm | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| negate-discard | completed | 1.0 | 0.0 | 0/0 | 0 | 0 |
| ocr-canvas | completed | 1.0 | 0.0 | 0/0 | 0 | 0 |
| ocr-label-only | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| tab-reveal | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| wizard-install | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |

**75/75 runs — success rate 100%, worst decide p95 0ms, physical acts
0.** `steps` is the rep mean; `over-opt` is mean steps over the
declared optimum on successful reps (only where `optimal_steps` is
set). `cancel-polarity` abstains by design since the polarity veto —
see its spec header. `ambiguous-twin` and `modal-blocking` abstain
under rule-based by design; under `Cascade[rule-based, laya]` the
typed questions resolve them (docs/sdd/laya-questions.md).
`dead-end-save` completes one step over optimum by design: the
first-ranked target refuses the press and the next-best semantic
target is tried (docs/sdd/recovery.md) — under rule-based the
generator's already-tried penalty moves on after one failure, so the
run shows 0 `recoveries`; the engine's rung-3 substitution only fires
for deciders that insist on the failed action.

## Live suite (macOS gate)

The live specs (`calc-add`, `calc-scientific`, `clock-timer`,
`settings-wifi`, `textedit-write`) run on the CI macOS runner via

```sh
dexter eval scenario datasets/scenarios --check datasets/scenarios/baseline.toml
```

Current gate: suite success rate 100% (latest `main` CI run —
`test` job log shows per-scenario outcomes and step counts;
`clock-timer` completes in its optimal 2 steps under the pinned
es-ES locale). Browser specs (`web-login`, `web-checkout`) need a
W3C WebDriver endpoint (`--browser-url`) and report `skipped`
otherwise — they keep the suite hermetic rather than pretending.

## Harvest datasets (decision-point replay)

`eval run` replays each frozen item (goal + full observation +
teacher gold) through the rule-based engine; `eval matrix` splits the
same run by provenance app. Regenerate with:

```sh
# browser — needs a W3C WebDriver endpoint:
chromedriver --port=9515 &
dexter --driver browser --browser-url http://localhost:9515 \
  eval harvest datasets/browser/manifest.toml -o datasets/browser/items.jsonl

# macos — needs a Mac with AX permission and `dexter` on PATH; the
# manifest's preps pin each app to es-ES:
dexter --driver macos eval harvest datasets/macos/manifest.toml \
  -o datasets/macos/items.jsonl
```

Measured on macOS 26.5 (Apple Silicon) @ this branch (2026-10-02):

| dataset | items | coverage | act accuracy | routes | false acts | false routes |
|---|---|---|---|---|---|---|
| browser | 21 | 100% (18/18 act) | 18/18 (100%) | 3/3 | 0 | 0 |
| macos | 20 | 100% (15/15 act) | 12/15 (80%) | 4/5 | 1 | 0 |

macOS per app (`dexter eval matrix`):

| app | items | act | routes |
|---|---|---|---|
| com.apple.TextEdit | 6 | 4/5 | 1/1 |
| com.apple.finder | 4 | 3/3 | 1/1 |
| com.apple.calculator | 4 | 2/3 | 1/1 |
| com.apple.clock | 4 | 2/3 | 0/1 |
| com.apple.systempreferences | 2 | 1/1 | 1/1 |

The four misses are generator/engine weaknesses the dataset now
measures — every gold resolved against the live AX tree at harvest:

- `textedit-close-all` — "cerrar todas las ventanas" picks
  Formato > Tipo de letra > Ligaduras > "Todas" (a bare quantifier
  label) over Archivo > "Cerrar todo".
- `calc-scientific` — "cambiar a la calculadora científica" picks the
  "Calculadora" menu bar item (the app menu, which changes no mode)
  over Visualización > "Científica".
- `clock-start` — "iniciar el cronómetro" on the Cronómetro tab
  re-presses the already-selected "Cronómetro" radio (a no-op)
  instead of "Iniciar".
- `clock-lap-disabled` — "marcar una vuelta" with the timer stopped
  presses the "Vuelta" column header — a pressable static_text that
  marks nothing — instead of abstaining. The one false act.

## Threshold calibration (rule-based act threshold)

Sweep of `RuleBased.act_threshold` over all four frozen datasets
(browser 21, macos 20, sim 11, vision 5 = 57 items). Regenerate with:

```sh
cargo run -p dexter-eval --example calibrate        # any host
dexter eval matrix datasets/*/items.jsonl --act-threshold 0.4,0.5,0.6,0.65,0.7,0.8,0.9,1.0
```

Measured on Linux @ this branch (2026-10-02), all datasets pooled:

| act_threshold | act | routes | false acts | false routes | score | utility |
|---|---|---|---|---|---|---|
| 0.40–0.50 | 40/43 (93%) | 8/14 | 3 | 0 | 48 | 42 |
| 0.55–0.60 | 39/43 (91%) | 8/14 | 3 | 1 | 47 | 41 |
| **0.65 (default)** | **39/43 (91%)** | **9/14** | **2** | **1** | **48** | **44** |
| 0.70 | 38/43 (88%) | 9/14 | 2 | 2 | 47 | 43 |
| 0.75 | 30/43 (70%) | 9/14 | 2 | 10 | 39 | 35 |
| 0.80 | 29/43 (67%) | 9/14 | 2 | 11 | 38 | 34 |
| 0.85 | 21/43 (49%) | 9/14 | 2 | 19 | 30 | 26 |
| 0.90 | 20/43 (47%) | 9/14 | 2 | 20 | 29 | 25 |
| 0.95 | 9/43 (21%) | 9/14 | 2 | 33 | 18 | 14 |
| 1.00 | 4/43 (9%) | 11/14 | 0 | 39 | 15 | 15 |

Coverage is 100% (43/43 act-golds) at every value — the threshold
only moves the engine, never the generator. `utility = score − 2 ·
false acts`; `calibrate::pick` (max utility → fewest false acts →
highest value, `docs/sdd/calibration.md`) selects **0.65**.

**Before/after: 0.65 → 0.65 — the shipped default is already the
calibrated point, so it does not move.** Per item: lowering to 0.50
buys `sim` `files-search` (top prior 0.55, a false route at 0.65) but
re-opens a false act on `macos` `finder-open-disabled` (prior 0.63 —
a weak partial match acted on while every "Abrir*" item is disabled
and gold is abstain); raising it only adds false routes (`sim` from
0.70, `browser` from 0.75). The two false acts
left at 0.65 — `macos` `clock-lap-disabled` and `vision`
`ocr-save-evidence-only` — carry top priors of 0.995: no threshold
below 1.0 removes them; they are generator weaknesses, not
calibration.

**Laya `min_confidence` (τ)**: wired into the same sweep
(`--engine laya --min-confidence 0,0.25,0.5`) but not re-measured
here — the builder host has no Laya checkpoint (no torch), and the
`dev` worker emits no confidence, so τ is a no-op on it by design. The
last measured τ numbers (root checkpoint, τ=0.25 catches the one
false act) live in `docs/sdd/eval.md`; τ stays 0 until a per-checkpoint
sweep justifies a value.

## Reading the numbers

- **Outcome ≠ success**: `abstained` is a pass where the world
  offers no honest act (`admin-absent`, `field-disabled`,
  `ocr-label-only`, `cancel-polarity` — only the wrong-polarity label
  exists, so abstaining is the pass). The suite gate checks `outcome
  == expected`, not "did it click something".
- **physical acts** counts coordinate-mechanism acts only — every
  act in this suite is semantic, as designed.
- **decide p95** is the engine-latency bound that matters for real
  models (the rule-based engine is ~0ms by construction); the
  baseline caps it per-scenario and suite-wide.
