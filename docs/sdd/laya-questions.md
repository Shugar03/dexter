# Laya typed questions — `docs/sdd/laya-questions.md`

Slice: the first concrete Laya uses beyond "pick a candidate"
(ROADMAP.md Etapa 3 — usos concretos): **blocking-modal detection** and
**ambiguous-target resolution**, asked as typed `Question`s through the
existing `DecisionEngine` seam.

## Contract

Structure is detected deterministically; judgment is asked of the
model.

Generator (`HeuristicGenerator`, has the observation):

- **Modal containers** are elements with role `dialog` or `sheet`. When
  at least one is present, every candidate whose element lies outside
  every modal subtree (parent chain) carries
  `CandidateAction.behind_modal = Some(<modal label>)`, a `× 0.5` prior
  penalty and a `behind modal "<label>"` rationale suffix. Candidates
  inside the modal are untouched.
- **Twin context**: when a target needs `SemanticTarget.index` (two or
  more elements share role + name), the rationale gains
  `occurrence k of n in "<nearest labeled ancestor>"` — the context the
  model needs to tell the twins apart.

Decision layer (`dexter_decision::questions`):

- `ambiguous_group(candidates)` — indices of the candidates tied with
  the top one (same prior) whose actions are identical except for the
  target's `index`. `< 2` members = no ambiguity.
- `typed_questions(ctx)` — the extra questions a Q&A engine asks:
  - `Bool { id: "blocking_modal" }` when any candidate is behind a modal.
  - `Choice { id: "disambiguate" }` over the ambiguous group (one option
    per member, in group order) plus a trailing `none` option.

`RuleBased` (no model) fails closed on both:

- ambiguous group → `Abstain` ("ambiguous target … not guessing");
- top candidate behind a modal → `Abstain`.

Under `--engine cascade` both abstentions escalate to Laya.

`LayaEngine::decide` sends `[pick, …typed_questions(ctx)]` in one
request (Laya batches them in one forward pass) and matches answers by
question id:

1. `pick` as before (routes, confidence gate).
2. Pick lands in the ambiguous group → the `disambiguate` answer
   chooses the member; `none` → `Abstain`; its confidence is gated by
   `min_confidence` too.
3. The chosen candidate is behind a modal → `blocking_modal` decides:
   `true` → act on the sole matching modal-dismissal candidate, if there
   is exactly one; otherwise `Route::EscalateHuman` naming the modal.
   Repeating the last successful action with `action_effect = true`
   still escalates rather than duplicating it. `false` → act on the
   chosen candidate.

## Invariants

- **Never an invented act** — the model still only picks generated
  candidates; disambiguation picks among generated twins.
- **Ambiguous targets fail closed** — no tier guesses among identical
  labels without a typed answer.
- **Protocol errors are errors** — a missing answer for an asked
  question, a wrong answer type or an out-of-range index is
  `DecisionError::Engine`, never a silent default.
- **Policy stays outside** — whatever Laya picks is still gated.

## Modal dismissal

Dismissal candidates are structural, generated only when there is
exactly one `dialog` or `sheet`, at least one ordinary candidate behind
it, and exactly one matching button that is enabled or has no enabled
state in that modal's descendant tree. Zero or multiple matches fail closed. If the generator already
offers that element for the goal, it is not tagged or duplicated. The
synthetic candidate uses the normal button target builder, has prior
`0.2`, and records the modal label in `dismisses_modal`.

The exact-label allowlist is `más tarde`, `ahora no`, `cerrar`,
`no, gracias`, `recordármelo más tarde`, `later`, `not now`, `close`,
`no thanks`, and `remind me later`, after lowercasing, trimming, and
stripping trailing `.`, `!`, or `…`. Matching is exact, never substring.
`cancelar`/`cancel` may abort the user's in-flight operation; `no` may
discard changes; `descartar`/`discard` and `omitir`/`skip` are not
safe dismissals; `aceptar`/`ok`/`sí` express consent.

`RuleBased` abstains when the top candidate is a dismissal candidate,
and the recovery ladder excludes dismissal candidates. Only Laya may
choose one after `blocking_modal = true`; it acts only if exactly one
candidate names the same modal, otherwise it escalates. The last-act
repeat guard applies to the selected dismissal too: when it repeats an
unchanged/unknown prior action and `action_effect = true`, Laya
escalates. The shared `Engine::run_step` still applies policy to every
act; dismissal does not bypass approval or policy checks.

The sim driver removes a dismissed modal together with its descendants,
but real-OS modal behavior still varies by toolkit. Scenario outcomes
pin decision behavior, not universal native dialog semantics.

## Action effect and goal completion

`DecisionContext.last_act` carries the previous successful action and
its structural world delta. `WorldDelta::Changed` means the complete
observation's world signature changed; `Unchanged` means it did not.
If either observation is incomplete (`elements_truncated`, `ax_limited`,
or `collection_errors > 0`), the delta is `Unknown`. Unknown is treated
like no observable change by the repeat guard.

The questions use the same conservative convention: `true` means stop
acting. `action_effect` is asked after `Unchanged` or `Unknown` and asks
whether the successful act took effect invisibly, so repeating it would
duplicate the effect. If the chosen candidate repeats that act and the
answer is true, Laya escalates to a human; false permits a retry. The
deterministic `RuleBased` engine abstains rather than blindly repeating
the same action in either delta state. A changed world asks
`goal_complete`; true escalates when the structural `done_when` still
does not verify, while false permits the decision to continue.

The model can never complete a goal: only the engine's existing
structural or first-verified-act checks do that. These questions can
only veto further acting.

Tests: `crates/decision/tests/questions.rs` pins delta questions and the
repeat guard; engine tests pin `None`, `Unchanged`, `Changed`, and
incomplete-perception `Unknown`; `crates/laya/tests/contract.rs` pins
typed answers and protocol failures. The `no-effect-submit` and
`goal-complete-mismatch` sim scenarios pin rule-based and Laya behavior;
`modal-blocking` and `modal-no-dismiss` pin safe dismissal and
fail-closed escalation.

## Tests

- `crates/decision/tests/questions.rs` — group detection, typed
  questions, `RuleBased` abstentions, generator `behind_modal` and twin
  context.
- `crates/laya/tests/contract.rs` — blocking/non-blocking modal,
  disambiguation member and `none`, via `tests/fixtures/typed_worker.py`
  and the dev worker.
- `datasets/scenarios/modal-blocking.toml`,
  `datasets/scenarios/ambiguous-twin.toml` — rule-based abstains on
  both (suite gate); `crates/eval/tests/scenario.rs` runs them through
  `Cascade[rule-based, laya(dev)]`: twin completes on the right row in
  one step, modal dismisses then saves in two acts. `modal-no-dismiss`
  pins escalation when no safe control is available.

## Measuring per-type quality

Each eval item may set `expect_questions` to `Some([...])`, the gold
question IDs the structural detector should raise; `Some([])` means no
typed question is expected. `None` means the item is unlabeled for
question detection. `last_act` supplies the prior action and world delta
needed to detect `action_effect` or `goal_complete`. Replay records the
sorted, deduplicated IDs returned by `typed_questions`, independently of
which decision engine answers.

`QuestionStats` reports, per question ID:

- `labeled`: labeled items whose `expect_questions` contains this ID.
- `detected`: labeled items expecting this ID where it was asked.
- `spurious`: asked items labeled with `Some(...)` that does not contain
  this ID.
- `asked`: all items where the detector asked this ID, including
  unlabeled items.
- `correct`: asked items whose decision matches the gold answer.
- `false_acts`: asked items with a gold route where the engine acted.
- `false_routes`: asked items with a gold action (`Act` or `AnyOf`)
  where the engine routed.

Detector recall is `detected / labeled`; decision accuracy is
`correct / asked`. Either is zero when its denominator is zero.
Unlabeled items count toward `asked` and its decision outcomes, but are
excluded from `labeled`, `detected`, and `spurious`: without a detector
label, an asked question cannot be classified as expected or spurious.

The initial question dataset has only eleven teacher-authored synthetic
items, so its per-type measurements have tiny `n` and are descriptive,
not quality gates.
