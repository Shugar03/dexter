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
