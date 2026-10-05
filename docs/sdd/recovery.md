# Recovery ladder — `docs/sdd/recovery.md`

Slice: rung 3 of the recovery ladder (ROADMAP.md Etapa 1 — retry →
refresh observation → alternative semantic target). Rungs 1 and 2
already existed: `Route::Retry` replays the last action, and every
`run_goal` step observes fresh before deciding. What was missing is
the bound — a decider that kept asking for the same failing action
(`Retry`, or `Act` on the same candidate) made the engine replay it
until `max_steps`.

## Contract

The ladder lives in `Engine::run_goal`, outside any model, and runs
the same for every `DecisionEngine` (rule-based, Laya, OpenAI,
cascade) — CLI, MCP and eval all go through it.

State per goal (reset per subgoal, like `GenHistory`):

- `failing: Option<(Action, failures)>` — the last executed action
  and how many times *in a row* it ended in anything but
  `StepStatus::Done`. A different action or a success resets it.
- `dead: Vec<Action>` — every action that failed in this goal.

When a decision resolves to executing action `a` (either
`Decision::Act { action: a }` or `Route::Retry` replaying `a`):

- `failures(a) < 2` → rung 1: execute `a` as decided (the first retry
  is legitimate — transient failures exist). Rung 2 is implicit: the
  step after a failure decides on a fresh observation.
- `failures(a) >= 2` → rung 3: do **not** execute `a` again. Take the
  next-best *generated* candidate of this step — highest prior, not
  behind a modal, action not in `dead` — and execute that instead,
  journaled as `RecoveryStarted { rung: 3, failed, failures,
  alternative, prior }`. The alternative is then subject to the same
  ladder if it fails too.
- No such candidate → the ladder is exhausted: `TaskOutcome::Escalated
  { route: EscalateHuman }` with a reason naming the failed action and
  count, journaled as `TaskFailed { outcome: "escalated", ladder:
  "exhausted" }`. Never a silent spin to `max_steps`.

## Invariants

- **Never an invented act.** The alternative is always one of the
  candidates the generator emitted for *this* observation — the same
  set the decider saw. Policy (`run_step_inner`) still gates it;
  approvals are fingerprint-bound to the alternative, not the failed
  act.
- **Fail closed.** A candidate `behind_modal` is never promoted by the
  ladder; an exhausted ladder escalates rather than guessing.
- **Deciders keep their thresholds.** The ladder only substitutes when
  the decider *insisted* on an already-twice-failed action. A decider
  that moves on by itself (rule-based does — the generator's
  already-tried penalty demotes the failed element) never sees rung 3.
- **Audit.** `RecoveryStarted` events already count as `recoveries`
  in eval reports; rung-3 events add `"rung": 3` so they are
  distinguishable from in-step verify retries (`"attempt": n`).

## Sim support

`dexter_sim::Effect::Fail(detail)` — a press rule whose element
refuses the action: the driver reports `ActionStatus::Failed` with
`detail`, records no press and applies no other effect. Models a
control the a11y tree exposes as pressable but the app rejects (the
honest `Failed` the macOS/Windows drivers already return). Spec form:
`effect = { type = "fail", detail = "…" }`.

## Tests

- `crates/engine/tests/e2e.rs`:
  `recovery_rung3_tries_next_best_candidate` (insisting decider: the
  failing top candidate is tried exactly twice, then the alternative
  completes the task; `RecoveryStarted { rung: 3 }` journaled),
  `recovery_ladder_exhausted_escalates` (no alternative → `Escalated
  { EscalateHuman }`, not `MaxSteps`),
  `insisting_act_on_dead_candidate_is_substituted` (`Decision::Act`
  on the dead action is treated like `Retry`).
- `drivers/sim/tests/effects.rs`: `fail_rule_reports_failed_without_pressing`.
- `datasets/scenarios/dead-end-save.toml`: first-ranked target
  refuses the press; the suite completes via the alternative in 2
  steps under rule-based.
