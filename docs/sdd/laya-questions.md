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
   `true` → `Route::EscalateHuman` naming the modal (dismissing a dialog
   the goal didn't mention is a human call); `false` → act.

## Invariants

- **Never an invented act** — the model still only picks generated
  candidates; disambiguation picks among generated twins.
- **Ambiguous targets fail closed** — no tier guesses among identical
  labels without a typed answer.
- **Protocol errors are errors** — a missing answer for an asked
  question, a wrong answer type or an out-of-range index is
  `DecisionError::Engine`, never a silent default.
- **Policy stays outside** — whatever Laya picks is still gated.

## Out of scope

The sim driver does not reject presses behind a modal (real-OS
semantics vary by toolkit); scenario outcomes rest on the decision
layer's verdicts. Modal dismissal as a generated candidate is a later
slice.

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
  one step, modal escalates.
