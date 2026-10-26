# SDD: scenario recording and replay

## Contract

A recording is the executed-action trace of a scenario run plus the loop
observation count before each action and after the last action. It records
what the engine proposed and the terminal result, not why a generator or
decision engine chose it.

Replay re-executes each recorded action through `Engine::run_step`.
Policy and approval checks are evaluated again against the replay world;
the recording never supplies or bypasses a policy decision. Decision
contexts and Laya/rule-based choices are deliberately not replayed: this
surface pins world-and-action determinism, while `eval run` pins decisions.

Each `ActionProposed` event records the driver's capability name.
`Recording.driver` is that identity when present; legacy journals without
it load with `None`. Present identities must agree across the journal.
Replay refuses a different driver before running any step. Legacy
recordings remain usable with Sim, but live replay requires a driver
journal with an identity.

Every recorded step retains its proposal fingerprint. `granted` is true
only when that step has an approved `PolicyChecked` fingerprint and a
proposal fingerprint, and the same step was not approved through
`approve_all`. Replay never honors these grants by default. The explicit
`--honor-recorded-grants` option checks the replayed action and app against
the recorded fingerprint, grants immediately before that one step, and
revokes the grant afterward if it was not consumed. A mismatch stops before
the action; grants cannot leak to later steps. Operator-supplied
`--grant` fingerprints remain available.

Coordinate actions remain disabled unless `--coords` is explicitly set.
This opt-in permits physical policy defaults for the invocation but does
not override an explicit policy denial or approval requirement.

Replay reports the first divergence as a structured value containing the
step index, recorded and replayed result, and a reason. Reasons preserve
failed/denied details, approval fingerprints, driver errors, and
pre-step/final observation errors. Final `done_when` verification remains
separate from step divergence; `diverged_at` mirrors the structured
divergence index.

## Terminal mapping

| Journal event | Recorded result |
|---|---|
| `PolicyChecked` with `decision = "deny"` | `Denied` |
| `HumanApprovalRequired` without a `granted` key | `NeedsApproval` |
| `ActionFailed` | `Errored` |
| `ActionExecuted` with `status = "Success"` | `Done` |
| `ActionExecuted` with any other status | `Failed` |

An approval granted by `approve_all` is not terminal: its event has a
`granted` key and execution continues to the policy and action result.

## Tick fidelity

Sim worlds may advance on every `observe()`. Each recorded step therefore
stores the count of loop `ObservationCreated` events carrying a `step`
field since the preceding terminal event. Verification observations carry
`attempt` instead and are excluded. Replay performs the recorded observes
before each action and preserves loop observes after the final action, so
tick-driven worlds such as `download-wait` advance on the same schedule.

## Fail-closed recording

Recording refuses an open action proposal, a malformed proposal or
terminal trace, and a journal that reports dropped events. A truncated
JSONL line or any other non-empty malformed line is rejected; blank lines
are ignored. Partial evidence must not become a replay fixture.

## Faithfulness

A replay is faithful only if every recorded step is reached with the same
result and the final `done_when` verification agrees with the outcome:
`completed` requires `Verified`; every other outcome requires a
non-`Verified` result. `Uncertain` is never success.

Fixtures live in `datasets/recordings/<scenario>.json` and are regenerated
with:

```sh
cargo run -p dexter-eval --example record
```

## CLI

`dexter record <journal.jsonl> -o <recording.json>` applies the same
fail-closed contract from the command line: the journal is parsed
strictly, the scenario id comes from `--scenario`, `--spec`, or the
journal file stem, the goal from `--goal`, `--spec`, or the journal's
`SubgoalStarted` events, and the outcome from the last terminal task
event (`outcome_from_events`) unless `--outcome` overrides it — a
journal with no terminal event is refused rather than recorded as a
finished run. The final-state check is never fabricated: it comes from
`--done` (an `ExpectedState` JSON) or the spec's `task.done_when`, and
`record` fails when neither supplies one.

`dexter replay <recording.json>` re-executes through `replay_with` and
prints the `ReplayReport` as JSON, exiting nonzero whenever
`faithful()` is false. The CLI output includes the recorded driver,
structured divergence, and each step's recorded grant and fingerprint.
`--driver sim` rebuilds the world from the
scenario TOML (`--spec`, else `<recording>/../scenarios/<id>.toml`,
else `datasets/scenarios/<id>.toml`) and mirrors the recorded run:
`approve_all` is on and the spec's authored `grants` apply — the sim is
the reference world the fixture was captured against. A real driver
replays live under the operator's `--policy` with `approve_all` off:
nothing is approved implicitly, approval-requiring steps report
`NeedsApproval` with the `(action, app)` fingerprint a `--grant` flag
would approve, and coordinates stay off unless `--coords` is passed.

The Linux-capable twin supports the same JSON report and opt-ins:

```sh
cargo run -p dexter-eval --example replay -- recording.json \
  --driver linux|browser|sim [--browser-url URL] [--policy FILE] \
  [--grant FP]... [--honor-recorded-grants] [--coords]
```
