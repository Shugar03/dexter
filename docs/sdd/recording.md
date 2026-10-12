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
