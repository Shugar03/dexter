# Eval numbers

Published, reproducible numbers for the task-level suite — the
OSWorld-style denominator: did the task complete, honestly and
efficiently, not just "did the model answer".

## Sim suite (rule-based engine, 5 reps)

Regenerate with:

```sh
cargo run -p dexter-eval --example suite_report 5
```

Measured on Linux @ `main` (2026-09-30):

| scenario | outcome | steps | over-opt | decide p50/p95 ms | recoveries | phys |
|---|---|---|---|---|---|---|
| admin-absent | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| cancel-polarity | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| download-wait | completed | 1.0 | 0.0 | 0/0 | 0 | 0 |
| field-disabled | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| files-open-dialog | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| form-fill | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| modal-confirm | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| ocr-canvas | completed | 1.0 | 0.0 | 0/0 | 0 | 0 |
| ocr-label-only | abstained | 0.0 | 0.0 | 0/0 | 0 | 0 |
| tab-reveal | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |
| wizard-install | completed | 2.0 | 0.0 | 0/0 | 0 | 0 |

**55/55 runs — success rate 100%, worst decide p95 0ms, physical acts
0.** `steps` is the rep mean; `over-opt` is mean steps over the
declared optimum on successful reps (only where `optimal_steps` is
set). `cancel-polarity` abstains by design since the polarity veto —
see its spec header.

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
