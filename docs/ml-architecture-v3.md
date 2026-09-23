# DEXTER — Master Architecture & MLOps Plan v3

> **Dexter — the hands and eyes of AI agents.**
>
> Local-first, model-agnostic Computer Use Runtime designed to make AI agents faster, cheaper, safer and more reliable.
>
> **Architectural thesis:** Dexter should not be another Computer Use model. It should be the operational nervous system around AI agents.

---

# 0. Executive Summary

Dexter sits between an AI agent and the computer.

Its responsibility is to transform:

```text
Intent
  ↓
Observe
  ↓
Build World State
  ↓
Generate Candidate Actions
  ↓
Rank / Decide
  ↓
Apply Policy
  ↓
Execute
  ↓
Verify
  ↓
Recover
  ↓
Learn
```

The key architectural idea is to separate:

- high-level reasoning;
- computer state representation;
- candidate generation;
- fast operational decisions;
- execution;
- verification;
- recovery;
- policy.

Large multimodal LLMs remain valuable, but they should not be forced into every microdecision.

Dexter should make the expensive model necessary as rarely as possible.

---

# 1. What We Learned From Laya

Laya provides several architectural ideas that should directly influence Dexter.

## 1.1 Dynamic option scoring is more useful than fixed classification

Laya does not simply classify into a fixed set of classes.

It evaluates candidate options present in the current question and scores them comparatively.

Conceptually:

```text
state + task + candidate A
state + task + candidate B
state + task + candidate C
              ↓
        shared encoder
              ↓
       candidate scores
              ↓
            softmax
```

This maps extremely well to Computer Use.

Dexter should not have:

```text
class 0 = click
class 1 = type
class 2 = scroll
...
```

Instead:

```text
candidate_1 = click(confirm)
candidate_2 = click(cancel)
candidate_3 = press(Escape)
candidate_4 = wait(500ms)
```

The model chooses among the candidates generated for the current state.

This makes the action space dynamic.

---

## 1.2 Candidate generation is therefore fundamental

Laya's architecture reinforces an important Dexter principle:

> **The model should not search the entire computer action space.**

Dexter should reduce:

```text
millions of possible coordinates/elements
```

to:

```text
5–15 plausible semantic actions
```

and then ask the decision layer to rank them.

---

## 1.3 Laya uses a large pretrained encoder plus a small decision transformer

Laya uses a ModernBERT-large backbone and adds a small decision-oriented Transformer stage plus specialized heads.

The important lesson is not:

> "Dexter must use ModernBERT-large."

The lesson is:

> **A general pretrained representation can be converted into a specialized decision model by adding a relatively small decision layer.**

Dexter should experiment with much smaller encoders first.

---

## 1.4 Option markers are highly relevant

Laya extracts representations from special markers associated with candidate options.

Dexter can use the same conceptual mechanism:

```text
[STATE]
...

[ACTION_1]
click(confirm)

[ACTION_2]
click(cancel)

[ACTION_3]
press(Escape)

[ACTION_4]
wait(500)
```

The decision core then scores each action marker.

This should be the default architecture for Dexter's first learned Decision Core.

---

## 1.5 Soft labels are more useful than hard labels

Instead of:

```text
correct = action_1
```

Dexter should support distributions:

```text
action_1 = 0.91
action_2 = 0.04
action_3 = 0.02
action_4 = 0.03
```

This is especially useful for:

- teacher distillation;
- ambiguous scenarios;
- uncertainty;
- calibration.

---

## 1.6 Option order must be randomized

Training data must randomize candidate ordering.

Otherwise the model may learn:

```text
option #1 is usually correct
```

instead of:

```text
this semantic action is correct for this state
```

Dexter's synthetic generator must aggressively randomize:

- candidate order;
- element IDs;
- labels;
- layouts;
- names;
- positions;
- application identifiers.

---

## 1.7 Confidence requires calibration

A raw softmax probability is not necessarily a trustworthy confidence estimate.

Dexter should therefore treat:

```text
model confidence
```

and:

```text
calibrated confidence
```

as separate concepts.

Pipeline:

```text
raw logits
  ↓
softmax
  ↓
calibration
  ↓
risk-aware decision
```

Possible calibration methods:

- temperature scaling;
- per-domain calibration;
- calibration by candidate count;
- calibration by action type.

---

## 1.8 Escalation is part of the decision problem

Laya includes a mechanism for deciding whether to act or escalate.

Dexter should make this a first-class decision.

But Dexter's action space is richer:

```text
EXECUTE
WAIT
REOBSERVE
RETRY
ABSTAIN
ESCALATE_LLM
ESCALATE_HUMAN
```

This is more appropriate than a binary:

```text
act / don't act
```

---

# 2. Dexter's Central ML Architecture

Dexter should evolve toward a shared **Reflex Layer**.

```text
                 DEXTER REFLEX LAYER

             Shared State Representation
                        │
                        ▼
                Candidate Interaction
                        │
                        ▼
                 Decision Core
                        │
          ┌─────────────┼─────────────┐
          ▼             ▼             ▼
       ACTION         VERIFY         RISK
        HEAD           HEAD          HEAD
          │             │             │
          └─────────────┼─────────────┘
                        ▼
                 ABSTAIN / ROUTE
                        │
              ┌─────────┼─────────┐
              ▼         ▼         ▼
           EXECUTE   RECOVER      LLM
```

The goal is to avoid immediately building three unrelated models.

Instead:

```text
shared encoder
+
small decision transformer
+
multiple specialized heads
```

This should be the main model hypothesis.

---

# 3. World Model

The World Model remains separate from the Reflex Layer.

It builds the normalized computer state.

## Inputs

### Browser

- DOM;
- URL;
- title;
- active element;
- Playwright state.

### Accessibility

- role;
- name;
- value;
- bounds;
- enabled;
- selected;
- focused;
- hierarchy.

### OS

- process;
- window;
- bounds;
- display;
- scaling;
- focus;
- cursor.

### Vision

- screenshot;
- OCR;
- visual regions;
- visual embeddings.

### History

- previous actions;
- previous observations;
- failures;
- verification.

---

# 4. Structured World State

Conceptual representation:

```rust
struct WorldState {
    timestamp: Instant,

    task: TaskContext,

    app: AppState,

    browser: Option<BrowserState>,

    windows: Vec<WindowState>,

    accessibility: AccessibilityTree,

    dom: Option<DomSnapshot>,

    screenshot: Option<ImageRef>,

    cursor: CursorState,

    focused_element: Option<ElementId>,

    recent_actions: Vec<ActionRecord>,

    expected_state: Option<ExpectedState>,
}
```

The World Model should become the canonical source of truth for Dexter.

---

# 5. State Encoder

A key architectural improvement over simply copying Laya:

## Do not necessarily encode the entire World State repeatedly for every candidate.

Instead:

```text
WorldState
    ↓
State Encoder
    ↓
cached State Embedding
```

Then evaluate candidate actions against that representation.

Conceptually:

```text
                  WORLD STATE
                       │
                       ▼
                 STATE ENCODER
                       │
                       ▼
                  STATE VECTOR
                       │
           ┌───────────┼───────────┐
           ▼           ▼           ▼
        Action A    Action B    Action C
           │           │           │
           └───────────┼───────────┘
                       ▼
                 Decision Core
```

This creates an opportunity for:

- lower latency;
- candidate batching;
- state caching;
- fewer redundant encoder passes.

---

# 6. Candidate Generator

The Candidate Generator transforms the raw World Model into a small candidate set.

Example:

```text
World State
     ↓
Candidate Generator

A = click(confirm_button)
B = click(cancel_button)
C = press(Escape)
D = wait(500ms)
E = reobserve
F = escalate
```

Target:

```text
~5–15 candidates
```

rather than hundreds or thousands.

---

# 7. Hierarchical Candidate Selection

If there are too many candidates:

```text
10,000 elements
      ↓
coarse ranking
      ↓
20 relevant elements
      ↓
fine ranking
      ↓
5–15 candidate actions
      ↓
Decision Core
```

This is preferable to giving a decision model an enormous flat action space.

---

# 8. Candidate Representation

Each candidate should have structured information.

Example:

```json
{
  "id": "candidate_17",
  "type": "click",
  "target": {
    "semantic_name": "confirm_reservation",
    "element_id": "ax_9281"
  },
  "source": "accessibility",
  "driver": "macos_ax",
  "risk": 0.20,
  "reversible": true
}
```

The model should receive both:

- natural-language semantics;
- structured metadata.

---

# 9. Dexter Reflex Input

Conceptually:

```text
[TASK]
Confirm today's pending reservation

[WORLD_STATE]
application = hotel_pms
window = reservation_details
reservation_status = pending
modal = confirmation
loading = false

[HISTORY]
open reservation

[CANDIDATE_1]
click confirm_reservation

[CANDIDATE_2]
click cancel_reservation

[CANDIDATE_3]
press Escape

[CANDIDATE_4]
wait 500ms
```

Then the Decision Core produces candidate scores.

---

# 10. Reflex Heads

The initial shared model should support several outputs.

## Action Head

```text
P(action_i | state, task)
```

## Abstain Head

```text
P(no safe action)
```

## Risk Head

```text
risk(action | state)
```

## Success Head

```text
P(expected outcome | state, action)
```

## Route Head

```text
EXECUTE
WAIT
REOBSERVE
RETRY
ESCALATE_LLM
ESCALATE_HUMAN
```

## Optional Progress Head

```text
P(task_progress)
```

---

# 11. Why This Is Better Than One Generic Classifier

Computer Use requires several distinct questions:

```text
What should I do?
Will it probably work?
Is it safe?
Did it work?
Should I abstain?
Should I ask a larger model?
```

A shared representation with multiple heads lets Dexter answer these without requiring a separate foundation model for each question.

---

# 12. Model Size Strategy

Do not begin at 400M+ parameters.

Experiment:

```text
50–100M
100–200M
200–300M
```

Only increase size if benchmark performance justifies it.

The objective is not model size.

The objective is:

```text
task success
+
reliability
+
calibration
+
low latency
+
low memory
```

---

# 13. Bootstrap Strategy

The first versions should not train a custom model.

## V0

Rules.

## V1

Existing decision model such as Laya.

## V2

Synthetic dataset.

## V3

Small Dexter Reflex prototype.

## V4

Real traces and human corrections.

## V5

Fine-tuned/refined Dexter Reflex.

---

# 14. Synthetic Dataset Strategy

Synthetic data is now a first-class part of Dexter's MLOps architecture.

The goal is to bootstrap useful models without waiting for millions of real user interactions.

Computer Use contains many controllable scenarios:

```text
modal
button
form
loading
disabled
popup
overlay
navigation
focus
error
network failure
delayed transition
```

The simulator knows ground truth.

Therefore it can produce labels cheaply.

---

# 15. Synthetic Environment

Do NOT build a perfect desktop simulator initially.

Start with a lightweight structured environment.

Example:

```json
{
  "task": "confirm_reservation",

  "state": {
    "modal_open": true,
    "loading": false,
    "confirm_enabled": true,
    "reservation_status": "pending"
  },

  "candidates": [
    "click_confirm",
    "click_cancel",
    "escape",
    "wait"
  ],

  "transition": {
    "click_confirm": "confirmed",
    "click_cancel": "cancelled",
    "escape": "pending",
    "wait": "pending"
  }
}
```

This alone is enough to train a first decision model.

---

# 16. Synthetic Dataset Types

## 16.1 Action Selection

```text
state + candidates
→ action distribution
```

## 16.2 Ranking

```text
state + A + B
→ A preferred over B
```

## 16.3 Abstention

```text
state + candidates
→ no safe action
```

## 16.4 Risk

```text
state + action
→ risk
```

## 16.5 Verification

```text
before + action + after
→ SUCCESS / FAILURE / UNCERTAIN
```

## 16.6 Recovery

```text
failure + recovery candidates
→ best recovery
```

## 16.7 Next-State Prediction

```text
state + action
→ expected next state
```

---

# 17. Synthetic Data Must Include Soft Labels

Prefer:

```json
{
  "action_a": 0.91,
  "action_b": 0.04,
  "action_c": 0.02,
  "action_d": 0.03
}
```

when multiple actions are plausible.

Use hard labels only where the environment genuinely provides a single correct answer.

Soft labels support:

- distillation;
- uncertainty;
- calibration;
- ambiguity modeling.

---

# 18. Synthetic Data Must Randomize Candidate Order

Every generated scenario should randomize:

- candidate ordering;
- IDs;
- element names;
- UI labels;
- layouts;
- coordinates;
- application identifiers.

The model must learn semantics and state relationships, not positional artifacts.

---

# 19. Synthetic Difficulty Curriculum

## Level 1

One obvious candidate.

## Level 2

Multiple candidates.

## Level 3

Distractors.

## Level 4

Ambiguous labels.

## Level 5

Disabled/hidden elements.

## Level 6

Loading/delay.

## Level 7

Failures.

## Level 8

Recovery.

## Level 9

Multi-step tasks.

## Level 10

Adversarial cases.

---

# 20. Adversarial Synthetic Generation

Once a model exists:

```text
Model
 ↓
benchmark
 ↓
find failure modes
 ↓
generate targeted synthetic cases
 ↓
retrain
 ↓
benchmark
```

Examples:

If the model confuses identical labels:

```text
generate thousands of identical-label scenarios
```

If it ignores disabled state:

```text
generate disabled-state scenarios
```

If it overuses click:

```text
generate cases where WAIT/REOBSERVE is optimal
```

This should be a continuous data-generation loop.

---

# 21. Synthetic-to-Real Gap

Synthetic data is not proof of real-world reliability.

Real interfaces contain:

- broken DOM;
- incomplete accessibility;
- weird labels;
- canvas;
- shadow DOM;
- iframes;
- scaling;
- timing issues;
- popups;
- legacy software;
- unexpected state;
- inconsistent semantics.

Therefore:

```text
Synthetic
   ↓
bootstrap
   ↓
Real
   ↓
hard cases
   ↓
synthetic adversarial
   ↓
real evaluation
```

must be continuous.

---

# 22. Real Trace Dataset

Every production task should optionally produce:

```text
Task
WorldState
CandidateSet
Decision
Action
Outcome
Verification
Recovery
HumanCorrection
```

This becomes the real Dexter dataset.

---

# 23. Human Corrections

Example:

```text
Dexter:
choose A

Human:
choose B

Dexter:
execute B
→ verify
```

Store:

```text
state
candidates
Dexter choice
human choice
outcome
```

Human corrections are high-value supervised examples.

---

# 24. Teacher Distillation

Use strong models strategically.

```text
WorldState
+
Candidates
        ↓
Large Teacher
        ↓
ranking
+
confidence
+
optional reasoning
        ↓
training distribution
        ↓
Dexter Reflex
```

Teachers can be:

- GPT-class models;
- Claude-class models;
- Qwen-class models;
- other strong local/remote models.

The production Reflex model does not need to generate reasoning text.

---

# 25. Laya as Bootstrap and Reference

Laya should be treated as:

```text
baseline
+
architectural reference
+
teacher candidate
```

not as an architectural dependency.

Dexter must remain model-agnostic.

Possible engines:

```text
Rules
Laya
Dexter Reflex
Gemma
Qwen
Remote LLM
```

---

# 26. Calibration

Laya's behavior demonstrates that raw softmax confidence can be poorly calibrated.

Dexter should therefore include:

```text
raw logits
 ↓
softmax
 ↓
calibration
 ↓
risk-aware routing
```

Evaluate:

- Expected Calibration Error;
- Brier score;
- reliability curves;
- false-confidence rate.

Calibration should be evaluated by:

- task domain;
- action type;
- candidate count;
- driver;
- application.

---

# 27. Risk-Aware Decisions

The probability of being correct is not enough.

Dexter must consider the cost of being wrong.

For example:

```text
safe click
confidence = 0.92
→ execute may be acceptable
```

but:

```text
purchase
confidence = 0.92
→ may still require confirmation
```

Therefore:

```text
decision =
probability
+
risk
+
policy
+
action reversibility
```

---

# 28. Decision Utility

Future training should optimize expected utility rather than accuracy alone.

Conceptually:

```text
utility =
correct_action_value
-
wrong_action_cost
-
unnecessary_escalation_cost
-
latency_cost
```

For sensitive actions:

```text
wrong_action_cost >> LLM_call_cost
```

Therefore the model should learn to escalate when appropriate.

---

# 29. RL / Proper Scoring Rules

Laya's RLCD approach demonstrates the value of optimizing calibrated probability distributions rather than only hard accuracy.

Dexter should investigate:

- log score;
- Brier-style objectives;
- proper scoring rules;
- policy optimization.

But NOT in the first iteration.

Order:

```text
supervised
→ calibration
→ distillation
→ real outcome data
→ proper scoring / RL
```

RL should only happen after the environment and reward are trustworthy.

---

# 30. State Caching

A major opportunity beyond Laya:

```text
WorldState
 ↓
State Encoder
 ↓
cached representation
```

Candidate evaluation should reuse this representation where possible.

Potential architecture:

```text
State Embedding
       │
       ├── Candidate A
       ├── Candidate B
       ├── Candidate C
       └── Candidate N
              │
              ▼
        batched scoring
```

This can reduce latency significantly.

---

# 31. Vision Architecture

Vision should be conditional.

Do not send every screenshot to a large VLM.

Preferred:

```text
structured state
   ↓
can we solve?
   ↓ yes → no vision

   ↓ no
region selection
   ↓
small vision encoder
   ↓
visual embedding
   ↓
Reflex Layer
```

---

# 32. Multimodal Reflex

Future:

```text
             DOM
              │
             AX
              │
             OS
              │
             Task
              │
           History
              │
        Visual Embedding
              │
              ▼
        Fusion Encoder
              │
              ▼
        Decision Core
              │
       ┌──────┼──────┐
       ▼      ▼      ▼
     Action  Risk  Verify
```

This should be added only after the structured version is strong.

---

# 33. Verification

Verification is first-class.

Output:

```text
VERIFIED
FAILED
UNCERTAIN
```

Never:

```text
UNCERTAIN → SUCCESS
```

without evidence.

---

# 34. Predictive Verification

Future:

```text
state + action
       ↓
predicted next state
```

Compare:

```text
predicted
    vs
observed
```

This can improve:

- action ranking;
- early failure detection;
- recovery;
- planning.

---

# 35. Recovery

Recovery order:

```text
1. retry
2. re-observe
3. wait
4. alternative semantic target
5. alternative driver
6. vision
7. rollback
8. cached procedure
9. LLM recovery
10. human
```

---

# 36. Recovery Model

Eventually:

```text
failure state
+
candidate recoveries
→
recovery distribution
```

Train initially on synthetic failure scenarios.

Fine-tune using real failures.

---

# 37. Policy

Policy must remain independent of ML.

Example:

```yaml
permissions:
  read_screen: true
  click: true
  type: true

dangerous_actions:
  delete: require_confirmation
  purchase: require_confirmation
  send_external_message: require_confirmation
  change_password: deny
```

Models propose.

Policy authorizes.

---

# 38. Action Router

Interaction hierarchy:

```text
API
 ↓
DOM / Playwright
 ↓
Accessibility
 ↓
Native OS
 ↓
Vision
 ↓
Coordinates
```

Use the highest semantic level available.

---

# 39. Driver Architecture

```rust
trait ComputerDriver {
    fn observe(&self) -> Result<Observation>;

    fn click(&self, target: Target) -> Result<ActionResult>;

    fn type_text(&self, target: Target, text: &str) -> Result<ActionResult>;

    fn keypress(&self, key: Key) -> Result<ActionResult>;

    fn scroll(&self, target: Target, direction: Direction) -> Result<ActionResult>;
}
```

Drivers:

```text
ApiDriver
BrowserDriver
MacOSAccessibilityDriver
WindowsUIDriver
LinuxAccessibilityDriver
VisionDriver
CoordinateDriver
```

---

# 40. Platform Strategy

## macOS

MVP:

- AXUIElement;
- native screenshot;
- keyboard/mouse;
- process/window APIs.

## Browser

- Playwright.

## Windows

- UI Automation;
- MSAA;
- Win32;
- native input.

## Linux

Initially:

- AT-SPI;
- X11 where practical.

Wayland becomes a dedicated later effort.

---

# 41. Runtime Stack

## Core

Rust.

## Async

Tokio.

## UI

Tauri 2 + TypeScript.

## Browser

Playwright worker.

## IPC

- Unix sockets;
- Named Pipes on Windows;
- Protobuf or equivalent.

## Storage

SQLite.

## Observability

- tracing;
- structured logs;
- metrics;
- OpenTelemetry where useful.

---

# 42. Process Architecture

Avoid unnecessary microservices.

Start with a modular local daemon:

```text
dexter-daemon
 ├── task engine
 ├── world model
 ├── candidate generator
 ├── decision router
 ├── policy
 ├── verifier
 ├── recovery
 ├── memory
 ├── trace store
 └── model runtime
```

Workers:

```text
browser worker
native driver
vision worker
model worker
```

Only split processes where isolation, platform constraints or resource management justify it.

---

# 43. Persistent Browser Sessions

Support:

```text
Dexter
 ↓
Playwright context
 ↓
persistent profile
 ↓
authenticated session
```

Avoid extracting credentials.

---

# 44. Background Automation

Classify automation:

```text
BACKGROUND_SAFE
FOREGROUND_REQUIRED
UNSUPPORTED_BACKGROUND
```

Do not claim that every native UI operation can run without stealing focus.

---

# 45. Security

Default:

- no password persistence;
- no cookie persistence;
- secret redaction;
- no clipboard logging by default;
- OS credential stores;
- audit sensitive actions;
- policy outside models;
- least privilege.

---

# 46. Memory

## Short-term

Current task.

## Operational

Environment-specific knowledge.

## Failure

Known failure patterns.

## Procedure

Successful workflows.

## Policy

Organization-specific rules.

---

# 47. Trace Architecture

Every action should optionally produce:

```text
observation
candidate set
decision
policy
action
result
verification
recovery
outcome
```

The same trace can power:

- debugging;
- replay;
- benchmarking;
- memory;
- training.

---

# 48. Synthetic Environment Architecture

The synthetic system should be its own module:

```text
ml/synthetic/
 ├── state_generator
 ├── ui_generator
 ├── task_generator
 ├── candidate_generator
 ├── transition_engine
 ├── failure_generator
 ├── adversarial_generator
 └── dataset_exporter
```

It should produce:

```text
JSONL / Parquet / Arrow
```

or another efficient dataset representation.

---

# 49. Synthetic Transition Engine

Represent the UI as a state graph:

```text
State A
 ├── action 1 → State B
 ├── action 2 → State C
 └── action 3 → Failure
```

Ground truth is deterministic.

This allows automatic labels for:

- action;
- outcome;
- expected state;
- verification;
- recovery.

---

# 50. Synthetic Visual Layer

Do not build first.

Add later:

```text
structured state
 ↓
renderer
 ↓
synthetic screenshot
```

The renderer can vary:

- fonts;
- themes;
- layouts;
- spacing;
- colors;
- scaling;
- window sizes.

The semantic state remains known.

---

# 51. Dataset Storage

Store examples with:

```text
task_id
environment_id
state
candidates
labels
teacher_distribution
risk
expected_state
outcome
verification
recovery
metadata
```

Metadata should include:

```text
synthetic / real
generator version
environment version
model version
label source
```

This is critical for reproducibility.

---

# 52. Dataset Versioning

Every dataset must have:

```text
dataset_version
generator_version
schema_version
label_version
```

Never silently regenerate a dataset and overwrite the previous one.

---

# 53. Data Quality Gates

Before training:

```text
schema validation
duplicate detection
label validation
candidate-order balance
class/action balance
difficulty distribution
synthetic/real ratio
```

Synthetic datasets can contain systematic bugs.

The generator itself must be tested.

---

# 54. Evaluation Splits

Do NOT randomly split only individual examples.

Create harder splits:

## Random split

Basic sanity.

## Template split

Hold out templates.

## UI split

Hold out UI layouts.

## Application split

Hold out applications.

## Task split

Hold out task types.

## Adversarial split

Hold out generated failure modes.

## Real-world split

Entirely real environments never seen during training.

This measures actual generalization.

---

# 55. Benchmark Harness

Metrics:

## Action

- accuracy;
- ranking;
- top-k.

## Abstention

- false action rate;
- safe abstention;
- coverage.

## Calibration

- ECE;
- Brier;
- reliability.

## Verification

- success precision;
- failure precision;
- uncertainty accuracy.

## Recovery

- recovery success;
- recovery latency.

## Runtime

- latency;
- throughput;
- RAM;
- CPU.

## Agent

- task success;
- actions/task;
- LLM calls/task;
- VLM calls/task;
- human interventions.

---

# 56. Model Evaluation Policy

Never ask:

> "Is Dexter's model smarter?"

Ask:

> "Does it improve the complete Computer Use system?"

A model is promoted only if it improves one or more of:

```text
success
reliability
latency
cost
memory
calibration
```

without unacceptable regression elsewhere.

---

# 57. Repository

```text
dexter/
│
├── crates/
│   ├── dexter-core/
│   ├── dexter-task/
│   ├── dexter-world/
│   ├── dexter-candidates/
│   ├── dexter-reflex/
│   ├── dexter-policy/
│   ├── dexter-verifier/
│   ├── dexter-recovery/
│   ├── dexter-memory/
│   ├── dexter-drivers/
│   │   ├── browser/
│   │   ├── macos/
│   │   ├── windows/
│   │   ├── linux/
│   │   └── vision/
│   ├── dexter-models/
│   ├── dexter-mcp/
│   └── dexter-cli/
│
├── apps/
│   └── dexter-ui/
│
├── ml/
│   ├── synthetic/
│   ├── datasets/
│   ├── training/
│   ├── evaluation/
│   ├── export/
│   └── configs/
│
├── models/
│   ├── configs/
│   └── evaluation/
│
├── tests/
│   ├── unit/
│   ├── integration/
│   ├── e2e/
│   └── failure/
│
├── docs/
└── Cargo.toml
```

---

# 58. Core Interfaces

```rust
trait ComputerDriver {}

trait ObservationProvider {}

trait CandidateGenerator {}

trait DecisionEngine {}

trait PolicyEngine {}

trait Verifier {}

trait RecoveryEngine {}

trait MemoryStore {}

trait ModelProvider {}
```

Reflex-specific:

```rust
trait ReflexModel {
    fn decide(
        &self,
        state: &EncodedState,
        candidates: &[CandidateAction],
        context: &DecisionContext,
    ) -> Result<ReflexDecision>;
}
```

---

# 59. Model Provider Abstraction

Possible implementations:

```text
RuleEngine
Laya
ONNX
Candle
Burn
Local LLM
Remote LLM
Dexter Reflex
```

The rest of Dexter must not depend on the provider.

---

# 60. MCP

Expose:

```text
computer.observe
computer.click
computer.type
computer.keypress
computer.scroll
computer.wait
computer.read
computer.execute_task
computer.get_state
computer.request_human
```

The external agent communicates with Dexter semantically.

---

# 61. UI

Tauri + TypeScript.

Show:

- current task;
- World State;
- screenshot;
- candidates;
- selected action;
- confidence;
- policy;
- verification;
- recovery;
- model usage;
- latency;
- trace.

The daemon remains headless.

---

# 62. Development Roadmap

## Phase 1 — Runtime Foundation

Build:

- Rust workspace;
- core types;
- driver traits;
- macOS driver;
- screenshots;
- keyboard/mouse;
- Accessibility;
- World Model.

No custom ML.

---

## Phase 2 — Browser

Build:

- Playwright;
- DOM;
- semantic targeting;
- browser sessions;
- Candidate Generator.

---

## Phase 3 — Reliability

Build:

- Verifier;
- Recovery;
- Policy;
- tracing;
- replay;
- benchmark harness.

Dexter should already be useful.

---

## Phase 4 — Existing Intelligence

Add:

- Laya adapter;
- small vision adapter;
- LLM fallback.

Measure everything.

---

## Phase 5 — Synthetic MLOps

Build:

- state generator;
- task generator;
- candidate generator;
- transition engine;
- failure generator;
- dataset exporter;
- benchmark suite.

Create the first synthetic datasets.

---

## Phase 6 — First Dexter Reflex

Train a small model:

```text
100–200M parameters
```

with:

```text
synthetic data
+
teacher distributions
```

Benchmark against:

```text
Rules
Laya
Gemma
Qwen
LLM
```

---

## Phase 7 — Real Data

Enable structured trace collection.

Collect:

- successful tasks;
- failures;
- human corrections;
- recoveries;
- verification outcomes.

---

## Phase 8 — Mixed Training

Train on:

```text
synthetic
+
real
+
teacher
+
human corrections
```

Evaluate separately on:

```text
synthetic
real
unseen applications
adversarial
```

---

## Phase 9 — Reflex Expansion

Add heads:

```text
action
risk
abstain
success
route
```

---

## Phase 10 — Multimodal

Add visual encoder only after structured Reflex reaches useful performance.

---

## Phase 11 — Advanced Optimization

Investigate:

- state caching;
- candidate batching;
- quantization;
- proper scoring;
- RLCD-like optimization;
- next-state prediction.

---

## Phase 12 — Platform Expansion

Add:

- Windows;
- Linux;
- enterprise deployment;
- fleet management;
- policy management.

---

# 63. What NOT to Build First

Do not begin with:

- foundation model from scratch;
- 1B+ custom model;
- perfect desktop simulator;
- giant VLM;
- RL;
- multi-agent system;
- distributed inference;
- cloud fleet;
- Wayland-first Linux;
- advanced CV research;
- 8-hour autonomous agents;
- huge vector database.

---

# 64. First Vertical Slice

Goal:

```text
"Submit this browser form."
```

Pipeline:

```text
Task
 ↓
World Model
 ↓
Candidate Generator
 ↓
Rules / Laya
 ↓
Policy
 ↓
Playwright
 ↓
Verifier
 ↓
Continue
 ↓
Final verification
```

Failure:

```text
Recovery
```

Trace:

```text
stored
```

That is the first meaningful Dexter.

---

# 65. MVP Definition

MVP succeeds when:

- external agent can issue a task;
- Dexter observes;
- Dexter builds World State;
- Dexter generates semantic candidates;
- Dexter decides;
- Dexter executes;
- Dexter verifies;
- Dexter recovers;
- policy blocks dangerous actions;
- traces are stored;
- browser works;
- macOS works;
- headless daemon works.

Custom ML is not required for MVP.

---

# 66. Model Promotion Criteria

A model replaces an existing engine only when:

```text
reliability >= baseline
AND
latency <= baseline
AND
cost <= baseline
AND
memory <= acceptable limit
```

with no unacceptable safety regression.

---

# 67. Moat

The model architecture itself is not the primary moat.

The moat should become:

```text
real traces
+
synthetic generator
+
adversarial generator
+
verification data
+
recovery data
+
human corrections
+
application knowledge
+
procedures
+
policy
+
driver reliability
```

The data loop is therefore a product feature, not merely an ML infrastructure concern.

---

# 68. Continuous Learning Flywheel

```text
More usage
    ↓
More traces
    ↓
More failures discovered
    ↓
Better failure taxonomy
    ↓
Targeted synthetic generation
    ↓
Better datasets
    ↓
Better Reflex models
    ↓
Better verification/recovery
    ↓
Fewer LLM calls
    ↓
Lower cost
    ↓
Higher reliability
    ↓
More usage
```

---

# 69. Final Architecture

```text
                         EXTERNAL AGENT
                               │
                               ▼
                         TASK ENGINE
                               │
                               ▼
                         WORLD MODEL
                               │
                    ┌──────────┴──────────┐
                    │                     │
              STRUCTURED               VISION
                    │                     │
                    └──────────┬──────────┘
                               ▼
                    CANDIDATE GENERATOR
                               │
                               ▼
                     DECISION CASCADE
                               │
               ┌───────────────┼───────────────┐
               │               │               │
             RULES        DEXTER REFLEX        LLM
               │               │               │
               │      ┌────────┼────────┐      │
               │      ▼        ▼        ▼      │
               │   ACTION    VERIFY    RISK    │
               │      HEAD     HEAD     HEAD   │
               │        └──────┼────────┘      │
               │               ▼               │
               └──────────► ROUTER ◄───────────┘
                               │
                               ▼
                            POLICY
                               │
                               ▼
                            DRIVER
                               │
                               ▼
                           COMPUTER
                               │
                               ▼
                           VERIFIER
                               │
                               ▼
                           RECOVERY
                               │
                               ▼
                            MEMORY
                               │
                               ▼
                             TRACE
                               │
              ┌────────────────┴────────────────┐
              ▼                                 ▼
        SYNTHETIC DATA                     REAL DATA
              │                                 │
              └────────────────┬────────────────┘
                               ▼
                         TRAIN / DISTILL
                               │
                               ▼
                         CALIBRATE
                               │
                               ▼
                          EVALUATE
                               │
                               ▼
                           DEPLOY
```

---

# 70. Master Development Sequence

```text
01. Rust workspace
02. Core types
03. Driver abstraction
04. macOS driver
05. Screenshot
06. Keyboard/mouse
07. Accessibility
08. World Model
09. Browser / Playwright
10. Candidate Generator
11. Semantic actions
12. Task Engine
13. Verifier
14. Policy
15. Recovery
16. Tracing
17. Replay
18. Benchmark harness
19. Laya adapter
20. Small vision adapter
21. LLM fallback
22. Synthetic state generator
23. Synthetic transition engine
24. Synthetic datasets
25. First Dexter Reflex prototype
26. Real trace collection
27. Human correction pipeline
28. Hard-case generator
29. Calibration
30. Dexter Reflex expansion
31. Verifier/Risk/Route heads
32. State caching
33. Candidate batching
34. Multimodal fusion
35. Proper scoring / RL experiments
36. Windows
37. Linux
38. Enterprise deployment
```

---

# 71. Non-Negotiable Principles

## 1. Deterministic first

> Do not solve with an LLM what deterministic logic can solve.

## 2. Small model first

> Do not solve with a large model what a small model can solve.

## 3. Existing models first

> Do not train a model when an existing model is sufficient.

## 4. Measure before training

> Do not build a model before measuring the actual bottleneck.

## 5. Synthetic data accelerates, real data validates

> Synthetic data is for bootstrapping and targeted edge cases. Real environments are the final judge.

## 6. Candidate generation matters

> The Decision Model should rank plausible actions, not search the entire computer.

## 7. Confidence must be calibrated

> Raw softmax confidence is not sufficient for safe automation.

## 8. Abstention is a feature

> A model that knows when not to act is more useful than one that always acts.

## 9. Verification is mandatory

> An executed action is not equivalent to a successful outcome.

## 10. Policy is outside the model

> Models propose. Policy authorizes.

## 11. Traces are product infrastructure

> Every interaction can become data for reliability, debugging, evaluation and learning.

## 12. Model agnosticism

> Laya, Gemma, Qwen, remote LLMs and Dexter models must be interchangeable behind stable interfaces.

---

# 72. Final Thesis

The most important lesson from Laya is not:

> "Use ModernBERT."

It is:

> **Turn decisions into structured candidate scoring instead of forcing a generative model to produce the action.**

Dexter takes that idea further.

Laya provides the conceptual **reflex**.

Dexter adds:

```text
World Model
+
Candidate Generator
+
Structured state
+
Vision
+
Policy
+
Drivers
+
Verification
+
Recovery
+
Memory
+
Synthetic data
+
Real-world traces
```

The resulting architecture is:

> **LLM for strategy, Dexter for operations, small models for reflexes, deterministic systems for everything that does not require intelligence.**

The ultimate goal is not to build the largest Computer Use model.

It is to make Computer Use require **less expensive reasoning per action while becoming more reliable**.

The optimization target is:

```text
minimum expensive reasoning
+
maximum structured state
+
fast local decisions
+
strong verification
+
fast recovery
+
continuous learning
```

And the long-term flywheel is:

```text
Synthetic
→ Bootstrap
→ Real usage
→ Traces
→ Hard cases
→ Adversarial synthetic data
→ Better models
→ Better runtime
→ Lower cost
→ More usage
```

That is the Dexter architecture.
