# SDD: scenario eval — task-level utility metrics

## Contract

`eval run` measures **decision quality on frozen single steps**.
`eval scenario` measures **utility on live tasks**: a declared world, a
goal, and a `done_when`, driven end-to-end by `Engine::run_task` — the
same closed loop production runs (observe → candidates → decide → act →
re-check). The default surface is `SimDriver` — hermetic, CI-safe;
`driver = "browser"` scenarios run the same loop on a live DOM over
WebDriver (opt-in via `--browser-url`); `driver = "macos"` + `[live]`
runs it on a real app's AX tree (opt-in by environment).

A task scenario is *not* a `dexter run` scenario: that one replays a
fixed action list; this one gives an engine a goal and watches what it
does — including whether it stops.

```
ScenarioSpec { scenario: {id, goal, optimal_steps, app, driver?},
               task: {done_when, expected, max_steps, max_secs, grants},
               world: {element[], rule[](on_press), tick[](on_observe)},
               browser?: {page | url, settle_ms},
               live?: {app, prep?, teardown?, settle_ms} }
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

## macOS-live scenarios

`driver = "macos"` + a `[live]` section run the identical loop against
a real app's Accessibility tree:

```toml
[scenario]
driver = "macos"

[live]
app = "com.apple.calculator"   # RunConfig.app scope — same selector
                               # syntax as --app (name | bundle | pid)
prep = "open -a Calculator"    # shell, per rep — normalises the world
teardown = "osascript -e '...'"# shell, per rep — always runs
settle_ms = 1200               # post-prep AND post-act settle
```

- The observe probe runs **after** the first prep (prep is what
  launches the app); a hard observe error — no AX permission, app
  missing — prints `skipped`, never `failed`. Same contract as
  browser: local-only, never CI-gated.
- `settle_ms` is also applied as `RunConfig.post_act_settle` — a real
  app propagates AX state asynchronously, so without it the post-act
  observation races the state change (measured below).
- `prep`/`teardown` may call `dexter` itself (on PATH post-install) —
  teardowns in the suite use `dexter click` to restore app state.
- **Background operation** — launch with `open -g` when the task only
  needs the menubar or the app already has a rendered window. macOS
  exposes window content over AX **only while the app is frontmost**:
  a background or lazy-launched app reports a menubar-only tree even
  when its CG window is on-screen. The runner detects that (no `window`
  elements in the probe), pays one bounded activation through the
  driver's `wake`/`restore` seam to force the render, and after the
  scenario finishes **restores the previously frontmost app** — the
  stage is borrowed once and handed back.
- **Activation is a privilege of the frontmost** — macOS coalesces or
  denies `activate` requests issued by non-frontmost processes. When
  the operator is working in another app, Dexter cannot summon windows
  at all; scenarios then report `skipped` and `app_map` answers
  `ax_limited: true` with the menubar-only vocabulary. That is the
  desired trade: background-capable when the user's focus allows it,
  honest degradation instead of focus theft when it doesn't. Menubar
  acts (menu_item presses) work fully in background regardless.
- `ComputerDriver::wake`/`restore` is the platform seam: the macOS
  driver implements bounded activation + frontmost restore via
  `NSRunningApplication` (no Apple Events, no Automation grant), other
  drivers default to no-op. `dexter map` and MCP `dexter_map` reuse it.

### What the first live run caught

Four real-app failure modes, all invisible to sim and frozen eval:

1. **Vacuous completion** — Calculator/Clock persist state across
   quit (scientific mode, a running stopwatch), so `done_when` was
   already true at step 0. `prep` must normalise inherited state, not
   just launch the app.
2. **Async AX propagation** — after a successful act the next observe
   raced the retitle/relabel (`window Wi‑Fi`, `Iniciar → Detener`);
   `done_when` missed, the repeated-attempt penalty sank the next
   candidate, and the task *abstained after succeeding*. Fixed by
   `RunConfig.post_act_settle`.
3. **U+2011** — the real Settings label is `Wi‑Fi` with a
   non-breaking hyphen, not ASCII `-`. Targets authored by guessing
   fail; labels must come from a live `observe`.
4. **Focus stealing** — TextEdit restores previous documents, and a
   restored window's `text_area` stole focus from the fixture's.
   `prep` closes restored documents; the focused-editable fallback
   needs the caret on *our* field.

Also surfaced a generator gap fixed in the same pass: a focused,
*unnamed* editable field with a quoted literal in the goal now gets a
`TypeText` candidate (previously only a no-op `Focus`). And a second
live pass surfaced the Space constraint above: `open -g` launches
without stealing focus but lazy apps then expose no window content —
the suite now wakes them once and restores frontmost afterwards.

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
## Sequential goals

`goal` strings are split into ordered sub-intents by
`decision::split_goal` before execution — "escribir 'x' y guardar"
runs as two subgoals. The split is deliberately conservative:

- Hard delimiters always split: `luego`, `después`, `then`, `next`,
  `after that`.
- Bare conjunctions (`y`, `e`, `and`) split only when the right side
  starts with a known verb — "black and white" stays one intent.
- Quoted literals are never split inside: `'pan y vino'` survives.

Each subgoal runs the same closed loop with **fresh history** (repeat
penalties must not leak across intents) inside `Engine::run_plan`;
`SubgoalStarted/Completed/Failed` events carry `index`/`of` so partial
progress is journaled and failures are attributable
(`PlanOutcome::Failed { index, goal, completed }`).

Completion per subgoal:

- `done_when` provided → the structural `ExpectedState` check, same as
  `run_task`.
- `done_when: None` → **auto-completion**: the subgoal finishes after
  the first mutating act (click/set/type/key/navigate) that verifiably
  changed the world — the next observation's signature (roles + names
  + values + enabled + focused + window titles, ids excluded since AX
  regenerates them) must differ. Act success alone never completes a
  subgoal: a no-op press loops until abstain, honestly.
- **Already-satisfied intents** — if the destination control is
  selected on entry (`goal_already_satisfied`: radio/tab/checkbox
  carrying a truthy value and matching the goal's object terms), the
  subgoal completes without acting. Found live: Clock reopened on
  Cronómetro, the press was a no-op, and auto-completion correctly
  refused to count it — the fix is checking, not pressing harder.

`run_task` is a one-subgoal plan — CLI `task`, MCP `dexter_task` and
`eval scenario` all route through `run_plan`, so sequential goals work
identically on every surface. The task-level `done_when` always belongs
to the last subgoal. `wizard-install` ("aceptar los términos y
continuar") now runs as two subgoals and still completes in optimal
steps — same behavior, now attributable.

## Expression goals

`calcular 134 más 89` is one subgoal whose steps come from the
expression itself: `expr_tokens` parses operands + operators from the
goal (digits, `+-*/×÷`, `x` between operands, word ops `más/menos/por/
entre/plus/minus/times/dividido/...`, ≥2 operands + ≥1 op required) and
`expr_next_candidate` emits the next keypad press — progress is tracked
through `GenHistory.attempts` (which labels were already pressed; a
failed last attempt doesn't consume a step), and operator labels match
a localized synonym table (`+` → `Sumar`/`Add`/`+`/...), never assumed
present. No keypad or no matching label → the path stays silent and
generic rules run; a stalled press decays (0.4) so an abstain can take
over. `calc-add` is the live spec: 7 optimal presses, `done_when`
reads `223` off the display.

## Application maps

`dexter_world_model::app_map` summarizes one observation into an
`AppMap` — windows, per-role counts, menubar verbs, named controls,
editable fields, navigation surfaces and evidence-tagged capability
inferences (calculator, document editor, menu-driven). It answers "what
is this app and what can it do" in one call — heuristic and honest, no
per-app hand-authoring, no model. Surfaces: `dexter map --app <sel>`
(pretty JSON) and MCP `dexter_map` (`app` required; `wake` default true
does the bounded foreground borrow described above). `ax_limited: true`
marks the menubar-only degradation — an agent reading the map knows the
window layer is missing rather than absent.

## The e_223 collision

`calc-add`'s first spec used `text_present "223"` and completed in **0
steps**: element ids render in the digest as `e_223`, so the predicate
matched a handle, not the display. `done_when` on live apps should
target `element_value` (or an exact element name), never a bare digest
substring — `text_present` is for authored sim worlds where ids are
small and stable.

## Wake is part of the standard flow

`dexter click`/`type`/`task`/`map` and the scenario runner all run the
same bounded-borrow: scoped observe → no `window` elements →
`driver.wake` (one activation) → settle → act → `restore`. Prep and
teardown scripts therefore self-heal — a `dexter click` inside a prep
wakes the app itself instead of failing `target not found` against a
windowless menubar tree.
