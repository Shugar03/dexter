# SDD: scenario eval — task-level utility metrics

## Contract

`eval run` measures **decision quality on frozen single steps**.
`eval scenario` measures **utility on live tasks**: a declared world, a
goal, and a `done_when`, driven end-to-end by `Engine::run_task` — the
same closed loop production runs (observe → candidates → decide → act →
re-check). The default surface is `SimDriver` — hermetic, CI-safe;
`driver = "browser"` scenarios run the same loop on a live DOM over
WebDriver, opt-in via `--browser-url`.

A task scenario is *not* a `dexter run` scenario: that one replays a
fixed action list; this one gives an engine a goal and watches what it
does — including whether it stops.

```
ScenarioSpec { scenario: {id, goal, optimal_steps, app, driver?},
               task: {done_when, expected, max_steps, max_secs, grants},
               world: {element[], rule[](on_press), tick[](on_observe)},
               browser?: {page | url, settle_ms} }
```

## What it measures

Per scenario, per rep, derived from the journal the engine already
writes (`engine.events()`) — this layer aggregates, it does not
instrument:

- **outcome** — `TaskOutcome` as a string: `completed`, `abstained`,
  `escalated`, `failed`, `max_steps`, `cancelled`, `timed_out`.
- **success** — outcome == `task.expected` (default `completed`; use
  `abstained` for worlds where the right answer is not to act).
- **steps / steps_over_optimal** — iterations used vs the declared
  optimum. The efficiency axis: an engine that completes in 8 steps
  what an ideal run does in 2 is measurably worse, not wrong.
- **phase latency** — journal `ts` deltas per step:
  `CandidatesGenerated→DecisionMade` = **decide_ms** (engine latency —
  the Laya question), `ObservationCreated→CandidatesGenerated` =
  gen_ms (verify + generation), `DecisionMade→ActionExecuted` = act_ms.
- **recoveries / verify_fails / action_failures** — `RecoveryStarted`,
  `VerificationFailed`, `ActionFailed` counts.
- **approvals** — `HumanApprovalRequired` count. Suite runs with
  `approve_all` (the author is the operator), so approvals still get
  journaled and counted without blocking the run.
- **physical_acts** — `ActionExecuted` with `mechanism=Coordinates`.
  On sim that means the engine reached for physical input where
  semantics should have sufficed — a coverage failure signal.

## Browser-live scenarios

`driver = "browser"` + a `[browser]` section run the identical loop
against a real DOM:

```toml
[scenario]
driver = "browser"

[browser]
page = "pages/web-login.html"   # resolved next to the spec → file://
settle_ms = 500                 # post-navigate settle
```

- Needs `--browser-url <endpoint>` (any W3C WebDriver: chromedriver,
  safaridriver, a grid). Without one the scenario prints `skipped` —
  never failed — so the suite stays hermetic and the CI gate untouched.
- Each rep opens a **fresh session** (`connect`, not `connect_attach`)
  and re-navigates: deterministic state, and the user's live browser
  session is never hijacked.
- `page` files live in `datasets/scenarios/pages/` — hand-authored,
  minimal, hidden sections revealed by interaction (the walker filters
  `display:none`, so reveals behave like real apps).
- Browser scenarios are local/nightly only — driver enablement is
  flaky on CI runners, and a metric suite must not flake the gate.
- A scenario named in `baseline.toml` but absent from results (skipped
  or deleted) **is a violation** — a skip cannot hide a regression.

## Limits honesty

- Latency on sim = engine + runtime overhead only — no OS driver cost.
  `decide_ms` answers "is the *decision* fast enough"; browser runs add
  real WebDriver round-trips to `gen_ms`/`act_ms`, which is the point —
  keep the two surfaces' numbers separate when comparing.
- `--reps` matters for non-deterministic engines (laya) and real
  drivers; rule-based on sim is deterministic, so reps > 1 mostly
  exercises the aggregation math.

## MLOps surface

```
dexter eval scenario datasets/scenarios [--engine laya] [--reps 5]
    [--out report.json] [--history runs.jsonl] [--check baseline.toml]
    [--export rows.jsonl] [--journal-out dir/] [--browser-url URL]
```

- `baseline.toml` — committed bounds: per-scenario `min_success`,
  `max_mean_steps`, `max_decide_p95_ms`, `max_physical_acts`, plus
  suite-level bounds. `--check` exits nonzero on regression — wired
  into CI (`.github/workflows/ci.yml`, "Scenario eval gate"). Bounds
  are authored against the **rule-based** engine (deterministic gate);
  laya comparisons go through `--history`/`--out`.
- `--history` appends a `{ts, git_sha, engine, metrics…}` record per
  run — longitudinal utility tracking across commits and checkpoints.
- `--journal-out <dir>` dumps each run's journal JSONL —
  `<id>.jsonl` (or `<id>-repN.jsonl` under `--reps`). This is how "why
  did the engine abstain on step 2" gets answered.
- `--export rows.jsonl` turns **successful** runs into Laya training
  rows — same shape as `eval export` (`state`/`options` rendered by
  `build_question`, so the row is inference-identical), labelled by the
  decision the engine actually took, plus `source: "scenario"` and
  `step`. Act decisions label `gold_index`; route decisions label the
  route slot. Invented actions (`candidate_index: None`) are counted as
  unlabelable, never guessed. Deterministic reps dedupe to one row.

  Only successful runs export — a failed trajectory is not a gold
  label. The intended teacher is `--engine rule-based --export`:
  on authored worlds its picks are correct by construction; exporting
  a laya run is self-distillation and says so.

## The fitness loop (measured once)

```
dexter eval scenario datasets/scenarios --export scenario-rows.jsonl
python3 workers/laya/finetune.py frozen+scenario.jsonl \
    --out ckpt --device cpu
dexter eval scenario datasets/scenarios --engine laya \
    --engine-path "python3 workers/laya/worker.py \
      --provider laya --model ckpt --subfolder root --device cpu"
```

First measurement (47 frozen + 8 scenario rows, 25 epochs):
suite **40%** — identical to the base model's headline, with a
*different* failure distribution: `files-open-dialog` and
`download-wait` now complete, but `wizard-install` abstains at step 0
and `admin-absent` loops to `max_steps` despite the abstain gold row
being in the training set. Honest read: 8 sequential rows don't move
task-level behaviour yet — the plumbing is the deliverable, the dataset
needs depth before the loop produces a real delta.

## SimDriver capabilities added for scenarios

- `Effect::SetEnabledOf(target, bool)` — enable chains ("the checkbox
  unlocks Continue").
- `Effect::CycleValueOf(target, values)` — advancing progress per
  observe, then holding the last value.
- `Effect::Remove(target)` — the world deleting an element mid-task.
- `SimDriver::on_tick(effect)` — applied on every `observe()` before
  the snapshot: worlds that evolve while the agent looks.

## Initial suite (`datasets/scenarios/`)

| scenario | exercises |
|---|---|
| `wizard-install` | enable chain — 2-step, disabled-until-checkbox |
| `files-open-dialog` | reveal dialog — row → confirm |
| `tab-reveal` | tab → newly spawned goal element (delta signal) |
| `download-wait` | wait correctly; pressing "Cancelar" is the fail |
| `admin-absent` | unsatisfiable goal — success = `abstained` |
| `web-login` | browser: fill→submit on a real DOM (needs `--browser-url`) |
| `web-checkout` | browser: reveal → pay — hidden-until-acted sections |

## Measured (first runs)

| scenario | rule-based | laya dev (heuristic) | laya model |
|---|---|---|---|
| wizard-install | completed, 2 steps | max_steps | abstained step 1 |
| files-open-dialog | completed, 2 | completed, 2 | abstained step 1 |
| tab-reveal | completed, 2 | completed, 2 | escalated step 1 |
| download-wait | completed, 1 | completed, 1 | completed, 1 |
| admin-absent | abstained (pass) | **max_steps — never stopped** | abstained (pass) |
| suite | 100% | 60% | 40% |

`decide_ms` on the real model: p50 ~50ms per step, but p95 **8.4s** on
the first decision — cold encoder load is visible in the percentile,
exactly what per-step latency is for. Tuning: worker warmup moves the
p95; the metric reports it either way.

Three distinct failure shapes at task level, none visible in
decision-level eval: the dev heuristic never *stops* on an
unsatisfiable goal (4 max_steps), while the real model *abstains or
escalates on step 1* of multi-step tasks — cautious in the wrong
direction. Task-level eval separates "can't chain" from "won't stop"
from "won't start".

## Found by the browser surface

`web-login` exposed a real generator flaw in its first run: under an
edit goal (`escribir "demo" … y entrar`), the `want_edit` penalty halved
the prior of *every* non-editable element permanently — so after the
field was filled, the submit control could never cross the act
threshold and the run abstained mid-form. Fixed in
`HeuristicGenerator`: the non-editable penalty applies only until an
edit action has actually been attempted (`GenHistory.attempts`).
Regression-locked in `edit_penalty_lifts_once_the_field_was_filled`.
Both browser scenarios now complete in optimal steps on Chrome 154 via
chromedriver, stable across `--reps 2`.
