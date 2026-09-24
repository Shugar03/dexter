# SDD: Recovery v2

## Purpose

Recovery must improve reliability without duplicating side effects. A failed
or delayed verification is not evidence that an action did not happen.

## State machine

```text
OBSERVE
  -> PLAN
  -> AUTHORIZE
  -> EXECUTE (at most once)
  -> VERIFY_POLL
       -> VERIFIED
       -> REOBSERVE
       -> REPLAN
       -> NEEDS_APPROVAL
       -> DENIED
       -> ESCALATE
       -> FAILED
```

## Act-once invariant

One call to `Engine::run_step` executes at most one mutating route. After a
successful execute, `max_attempts` bounds fresh observation and verification
polls only. It never calls execute again.

`max_attempts`'s v2 meaning is polling, not action replay.

A second mutation is legal only when:

- a task decision explicitly returns `Route::Retry`, or
- recovery selects a different planned route after a reported unsupported
  primary route.

Both cases re-plan, re-run policy and consume a new approval.

## Verification polling

```rust
pub struct RunConfig {
    // Per-step bound on act + verify cycles.
    pub max_attempts: u32,
    // Settle time before re-observing for verification.
    pub verify_delay: Duration,
    // Settle after a completed act before the next observe.
    pub post_act_settle: Duration,
    // existing app/coords/approval/observe/scope fields
}
```

- `max_attempts >= 1`.
- Polls honor the task cancel token between attempts; the attempt bound
  caps them. The wall-clock budget is enforced by the task loop between
  steps, not inside a poll — a single observe+verify is atomic.
- `FAILED` and `UNCERTAIN` remain distinct in events, but neither is success.
- If execute times out or returns an unknown-effect failure and an expectation
  exists, Engine verifies before considering another action because the side
  effect may have landed.
- A step without an expectation reports the driver's honest result and does
  not invent verification. An element token bound to a foreign observation
  is unresolvable for expectation derivation — the step runs unverified
  rather than binding whichever element now sits at that id (or failing on
  a secure field's redacted value).

## Deterministic recovery classifier

Recovery is an internal pure function, not a public `RecoveryEngine` trait
until a second implementation exists.

- `NeedsApproval` -> task returns `NeedsApproval` immediately.
- Policy deny -> task returns `Denied` immediately.
- Stale/not-found/ambiguous target -> fresh observe and candidate re-plan in
  task mode; single-step calls return the error.
- Unsupported route -> consider the next planned route, authorize it
  independently and execute at most once.
- Foreground required/windowless -> policy-gated wake once, settle, re-plan.
- Permission denied -> terminal failure with onboarding hint.
- Timeout after possible side effect -> verify first.
- Exhausted polls/routes -> escalate or fail; never flail.

## Approval and resume

`TaskOutcome` adds:

```rust
NeedsApproval {
    fingerprint: String,
    reason: String,
    // The redacted action summary the operator approves — payloads stay
    // digest tokens.
    action: serde_json::Value,
}
Denied {
    reason: String,
}
```

`PlanOutcome` preserves subgoal index and completed count around these
outcomes. MCP returns the fingerprint, clears the running-task slot and
releases the engine lock. The human calls `dexter_grant`, then the agent
re-invokes the task. The canonical approval key excludes observation ids, so
re-observing the same stable target consumes the grant; a changed target does
not.

## Wake/restore

Engine owns wake/restore for all callers — there is no mode flag. Wake fires
only for acts that stage a pre-action observation (`needs_stage`: the
element-targeted acts), only when the scoped observation really showed no
window content, and only for the concrete route about to execute that
declares `requires_foreground` — a background route (a menu AXPress) never
steals focus. Read-only observes never wake.

Wake is a visual operation and goes through policy before activation — the
borrow is evaluated as a launch-or-activate route, so a `deny` on
`launch_app` refuses it and an unapproved one surfaces its own fingerprint.
A granted stage approval is session-scoped: retries re-borrow the same app
without re-asking (execute approvals stay single-use). After activation the
engine re-observes, re-resolves descriptors and re-authorizes the concrete
route on the woken world before executing. Restore runs after success,
error, denial, approval request, cancellation and timeout. CLI/MCP/eval do
not implement private wake helpers.

## Events

- `RecoveryStarted { strategy, trigger, attempt }`
- `StateChanged { added, removed, changed, focus_changed, windows_changed }`
- `RecoveryCompleted { strategy, outcome }`

`ActionExecuted` counts physical executes, not verification polls. Scenario
metrics separately count `execute_count`, `verify_polls`, recovery strategy,
recovery outcome and duplicate effects.

## TDD contract

1. A delayed expected state verifies after multiple polls with one execute.
2. Timeout-after-effect verifies before any retry.
3. Single-use approval cannot cover two execute calls.
4. Task approval returns immediately and can be granted/re-invoked.
5. Denied routes do not consume task steps in a loop.
6. Stale candidates reobserve and select an observation-bound alternative.
7. Unsupported semantic route cannot enter a physical route without coords and
   policy.
8. Wake restores frontmost state on every terminal path.
9. Cancellation interrupts verify delay and clears MCP task state.
10. A denied or unapproved stage borrow never activates the app, and a
    route authorized on a windowless world is re-authorized after the wake
    before it may execute.
