# SDD: scenario eval — task-level utility metrics

## Contract

`eval run` measures **decision quality on frozen single steps**.
`eval scenario` measures **utility on live tasks**: a declared world, a
goal, and a `done_when`, driven end-to-end by `Engine::run_task` — the
same closed loop production runs (observe → candidates → decide → act →
re-check). No machine access: scenarios run on `SimDriver`, so the suite
is hermetic and CI-safe.

A task scenario is *not* a `dexter run` scenario: that one replays a
fixed action list; this one gives an engine a goal and watches what it
does — including whether it stops.

```
ScenarioSpec { scenario: {id, goal, optimal_steps, app},
               task: {done_when, expected, max_steps, max_secs, grants},
               world: {element[], rule[](on_press), tick[](on_observe)} }
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

## Limits honesty

- Latency on sim = engine + runtime overhead only — no OS driver cost.
  `decide_ms` answers "is the *decision* fast enough"; driver latency
  belongs to live runs, not this suite.
- `--reps` matters for non-deterministic engines (laya) and real
  drivers; rule-based on sim is deterministic, so reps > 1 mostly
  exercises the aggregation math.

## MLOps surface

```
dexter eval scenario datasets/scenarios [--engine laya] [--reps 5]
    [--out report.json] [--history runs.jsonl] [--check baseline.toml]
```

- `baseline.toml` — committed bounds: per-scenario `min_success`,
  `max_mean_steps`, `max_decide_p95_ms`, `max_physical_acts`, plus
  suite-level bounds. `--check` exits nonzero on regression — wired
  into CI (`.github/workflows/ci.yml`, "Scenario eval gate"). Bounds
  are authored against the **rule-based** engine (deterministic gate);
  laya comparisons go through `--history`/`--out`.
- `--history` appends a `{ts, git_sha, engine, metrics…}` record per
  run — longitudinal utility tracking across commits and checkpoints.

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
