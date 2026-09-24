//! Decision seam — who decides *what* to do next, never *how*.
//!
//! Architecture (from the v3 reflex design):
//!
//! ```text
//! Observation ──► CandidateGenerator ──► Vec<CandidateAction>
//!        │                                    │
//!        └──────────► DecisionContext ◄───────┘
//!                            │
//!                     DecisionEngine::decide()
//!                            │
//!               Decision::Act / Decision::Route(...)
//! ```
//!
//! - `CandidateGenerator` reduces thousands of observed elements to a
//!   handful of plausible actions before any model sees them
//!   (rules-based today; learned ranking later).
//! - `DecisionEngine` is the plug point: `RuleBased` ships with the
//!   runtime, `LayaEngine` (sidecar) and LLM agents implement the same
//!   trait. Engines only *propose* — policy still gates every action.
//! - `Route` is richer than act/don't-act: a decision can wait,
//!   re-observe, retry, abstain, or escalate.
//! - `Question`/`Answer` are the typed micro-decision DTOs Q&A-style
//!   engines (Laya) express themselves through; the engine-internal
//!   `decide()` call stays opaque.

use dexter_core::{Action, Element, MouseButton, Observation, Target};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// What the runtime should do next, beyond executing an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Route {
    /// Nothing sensible to do yet — observe again.
    Reobserve,
    /// Wait for the world to change (animations, loads).
    Wait { millis: u64 },
    /// Repeat the last action (e.g. transient failure).
    Retry,
    /// No candidate fits the goal — stop without acting.
    Abstain,
    /// Hand the decision to a larger model.
    EscalateLlm,
    /// Hand the decision to a human.
    EscalateHuman,
}

/// A proposed action with provenance for audit and ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateAction {
    pub action: Action,
    /// Why the generator proposed it (journal-visible).
    pub rationale: String,
    /// Generator prior in [0,1] — *not* a confidence the policy trusts.
    pub prior: f32,
}

/// Everything a decision engine needs for one step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionContext {
    /// The user's goal, verbatim.
    pub goal: String,
    /// World-model text digest of the latest observation.
    pub state_digest: String,
    /// Generated candidates, highest prior first.
    pub candidates: Vec<CandidateAction>,
    /// Last step's error/failure summary, if this is a retry.
    pub last_error: Option<String>,
    /// 1-based step counter within the task.
    pub step: u32,
}

/// The engine's verdict for one step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Decision {
    /// Execute this action — index into `ctx.candidates` when it came
    /// from the generator (`None` = engine invented it). `Action` grew
    /// with the v2 variants; boxing keeps `Decision` small — serde is
    /// transparent to the indirection.
    Act {
        action: Box<Action>,
        candidate_index: Option<usize>,
        /// Journal-visible explanation (never trusted by policy).
        rationale: String,
    },
    /// Don't act this step — take a route instead.
    Route { route: Route, rationale: String },
}

#[derive(Debug, Error)]
pub enum DecisionError {
    #[error("decision engine '{engine}' failed: {message}")]
    Engine { engine: String, message: String },
    #[error("decision engine '{engine}' timed out after {millis}ms")]
    Timeout { engine: String, millis: u64 },
}

/// Liveness self-report for a decision engine. `dexter_status` and
/// `dexter doctor` surface this — agents probe it before trusting
/// `dexter_task` with a goal.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EngineHealth {
    /// Working and responding.
    Ready,
    /// Alive but impaired (e.g. respawn budget partially spent).
    Degraded(String),
    /// Not responding — the detail says why.
    Down(String),
}

/// The plug point for Laya, LLMs and rule engines.
pub trait DecisionEngine: Send + Sync {
    fn name(&self) -> &str;
    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError>;
    /// Liveness probe. Read-only — it must never mutate supervision
    /// state (no respawns). Default: embedded engines are always ready.
    fn health(&self) -> EngineHealth {
        EngineHealth::Ready
    }
}

/// Typed micro-decision DTO for Q&A-style engines (Laya's actual API
/// shape). A `DecisionEngine` may internally render a `DecisionContext`
/// into `Question`s, get `Answer`s from a model, and interpret them.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Question {
    /// Pick exactly one option by index.
    Choice {
        id: String,
        prompt: String,
        options: Vec<String>,
    },
    /// Numeric score in [0,1].
    Score { id: String, prompt: String },
    /// Yes/no gate.
    Bool { id: String, prompt: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        id: String,
        index: usize,
        /// Model's calibrated confidence in its pick, when the engine
        /// reports one (Laya does). Engines may gate acting on it.
        #[serde(default)]
        confidence: Option<f32>,
    },
    Score {
        id: String,
        value: f32,
    },
    Bool {
        id: String,
        value: bool,
    },
}

/// Task history available to the generator: what was already tried and
/// what the world looked like before. Repeat penalties and observation
/// deltas live here.
#[derive(Debug, Default)]
pub struct GenHistory {
    /// Actions already executed in this task (in order).
    pub attempts: Vec<Action>,
    /// Display label each attempted action resolved to at decision time
    /// (parallel to `attempts`) — element targets carry no name, so the
    /// engine records the label it resolved when the world was fresh.
    pub attempt_names: Vec<Option<String>>,
    /// Last step's failure summary, if any.
    pub last_error: Option<String>,
    /// Previous observation, for delta signals (new elements get a
    /// small bonus — a dialog that just appeared is often the point).
    pub prev: Option<Observation>,
}

/// Reduces an observation to plausible next actions for a goal.
pub trait CandidateGenerator: Send + Sync {
    fn generate(&self, obs: &Observation, goal: &str, hist: &GenHistory) -> Vec<CandidateAction>;
}

/// Default generator: goal is parsed into verbs + object terms, elements
/// are scored by term coverage (no saturation), verb→affordance
/// alignment, focus state and history. Emits action variety (click /
/// focus / set_value / type_text) so engines pick *what* to do, not
/// just *where*. Deterministic, no model — that's the point of the seam.
pub struct HeuristicGenerator {
    /// Max candidates emitted per observation.
    pub max_candidates: usize,
}

impl Default for HeuristicGenerator {
    fn default() -> Self {
        Self { max_candidates: 12 }
    }
}

const STOPWORDS: &[&str] = &[
    "the", "a", "an", "my", "your", "our", "to", "for", "of", "in", "on", "and", "or", "it",
    "this", "that", "with", "from", "into", "me", "we", "i", "you", "is", "are", "be", "do",
    "does", "can", "could", "should", "would", "will", "please", "now", "any", "some", "up", "out",
    "then", "than", "so", "such", "no", "not", "only", "get", "each", "more", "most", "other",
    "much", "many",
    // Spanish — real machines run localized UIs. Quantifiers ("todo",
    // "todas", "all") are NOT stopwords: "cerrar" vs "cerrar todo" are
    // different actions.
    "una", "un", "el", "la", "los", "las", "del", "al", "para", "por", "con", "en", "de", "y", "o",
    "que", "se", "su", "sus", "mi", "tu", "es", "son", "hay", "muy", "este", "esta", "estos",
    "estas", "ese", "esa", "eso", "como", "cuando", "donde", "cada", "entre", "sobre", "desde",
    "hasta", "ser", "estar", "hacer", "donde", "ir", "voy", "ve",
];

/// Multi-word verbs checked before single words ("log in" beats "log").
const PHRASE_VERBS: &[&str] = &[
    "log in",
    "sign in",
    "sign up",
    "log out",
    "check out",
    "go back",
    "proceed to checkout",
];

/// Verbs that imply an editing action (type into a field).
const EDIT_VERBS: &[&str] = &[
    "type",
    "write",
    "enter",
    "fill",
    "input",
    "set",
    "paste",
    // Spanish
    "escribir",
    "teclear",
    "ingresar",
    "rellenar",
    "completar",
];

/// Verbs that imply a press action (click a control).
const PRESS_VERBS: &[&str] = &[
    "click",
    "press",
    "tap",
    "pay",
    "submit",
    "confirm",
    "cancel",
    "delete",
    "remove",
    "close",
    "dismiss",
    "open",
    "select",
    "choose",
    "check",
    "uncheck",
    "accept",
    "reject",
    "save",
    "continue",
    "next",
    "back",
    "find",
    "buy",
    "order",
    "add",
    "create",
    "sign",
    "log",
    "login",
    "logout",
    "go",
    "navigate",
    "launch",
    "start",
    "stop",
    "apply",
    "ok",
    "agree",
    "claim",
    "proceed",
    "enable",
    "toggle",
    "search",
    "send",
    // Spanish
    "abrir",
    "cerrar",
    "guardar",
    "borrar",
    "eliminar",
    "quitar",
    "pulsar",
    "clic",
    "aceptar",
    "continuar",
    "siguiente",
    "volver",
    "buscar",
    "enviar",
    "pagar",
    "comprar",
    "crear",
    "iniciar",
    "elegir",
    "seleccionar",
    "confirmar",
    "cancelar",
    "descartar",
    "activar",
    "desactivar",
    "imprimir",
    "compartir",
];

/// Split a goal into ordered sub-intents on sequencing language:
/// "escribir 'x' y guardar" → ["escribir 'x'", "guardar"]. Conservative
/// by design — a bare "y"/"and"/"e" only splits when the right side
/// starts with a verb, and quoted literals are never split inside.
/// Returns the goal unchanged when there is no sequence.
pub fn split_goal(goal: &str) -> Vec<String> {
    let lower = goal.to_lowercase();
    // Byte ranges covered by quoted literals — protected from splitting.
    let mut protected: Vec<(usize, usize)> = Vec::new();
    for q in ['\'', '"'] {
        let mut from = 0;
        while let Some(a) = lower[from..].find(q) {
            let a = from + a;
            match lower[a + 1..].find(q) {
                Some(b) => {
                    protected.push((a, a + 2 + b));
                    from = a + 2 + b;
                }
                None => break,
            }
        }
    }
    protected.sort_unstable();

    // Words with their byte spans.
    let words: Vec<(usize, usize, &str)> = {
        let mut v = Vec::new();
        let mut start = None;
        for (i, c) in lower.char_indices() {
            if c.is_alphanumeric() || c == '-' || c == '\'' {
                if start.is_none() {
                    start = Some(i);
                }
            } else if let Some(s) = start.take() {
                v.push((s, i, &lower[s..i]));
            }
        }
        if let Some(s) = start {
            v.push((s, lower.len(), &lower[s..]));
        }
        v
    };

    // Sequencing words that always split when they begin a clause
    // boundary; "after"/"next" pair with a following word.
    let hard = ["luego", "despues", "después", "then", "next"];
    let mut clauses: Vec<String> = Vec::new();
    let mut clause_start = 0usize;
    let mut i = 0;
    while i < words.len() {
        let (ws, _we, w) = words[i];
        let in_quotes = protected.iter().any(|(a, b)| ws >= *a && ws < *b);
        if !in_quotes && clause_start < ws {
            let next = words.get(i + 1).map(|w| w.2);
            let is_hard = hard.contains(&w)
                || (w == "after" && next == Some("that"))
                || (w == "y" && hard.contains(&next.unwrap_or("")))
                || (w == "e" && hard.contains(&next.unwrap_or("")))
                || (w == "and" && hard.contains(&next.unwrap_or("")));
            // Guarded conjunction: "y"/"e"/"and" split only before a verb.
            let is_guarded = (w == "y" || w == "e" || w == "and") && next.is_some_and(is_seq_verb);
            if is_hard || is_guarded {
                let clause = goal[clause_start..ws]
                    .trim()
                    .trim_end_matches([',', ';'])
                    .trim();
                if !clause.is_empty() {
                    clauses.push(clause.to_string());
                }
                // Consume the delimiter word; two-word delimiters eat next too.
                let mut skip = i + 1;
                if (w == "after" && next == Some("that"))
                    || ((w == "y" || w == "e" || w == "and") && hard.contains(&next.unwrap_or("")))
                {
                    skip += 1;
                }
                clause_start = words[skip - 1].1;
                i = skip;
                continue;
            }
        }
        i += 1;
    }
    let tail = goal[clause_start..]
        .trim()
        .trim_end_matches([',', ';'])
        .trim();
    if !tail.is_empty() {
        clauses.push(tail.to_string());
    }
    if clauses.is_empty() {
        clauses.push(goal.trim().to_string());
    }
    clauses
}

/// Verbs that can start a subgoal clause — the guard for bare
/// conjunctions. Union of the press/edit lexicons plus task verbs.
fn is_seq_verb(w: &str) -> bool {
    PRESS_VERBS.contains(&w)
        || EDIT_VERBS.contains(&w)
        || matches!(
            w,
            "calcular" | "calculate" | "sumar" | "restar" | "contar" | "count" | "esperar" | "wait"
        )
}

/// Arithmetic expression parsed from a goal, as ordered tokens —
/// operands (digit strings) and operators, always ending in "=".
/// `None` when the goal isn't arithmetic (needs ≥2 operands, ≥1 op).
fn expr_tokens(goal: &str) -> Option<Vec<String>> {
    const OP_WORDS: &[(&str, &str)] = &[
        ("multiplicado", "*"),
        ("multiplicar", "*"),
        ("dividido", "/"),
        ("mas", "+"),
        ("más", "+"),
        ("plus", "+"),
        ("menos", "-"),
        ("minus", "-"),
        ("por", "*"),
        ("times", "*"),
        ("entre", "/"),
    ];
    let lower = goal.to_lowercase();
    let chars: Vec<(usize, char)> = lower.char_indices().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut operands = 0;
    let mut ops = 0;
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if c.is_ascii_digit() {
            // Consume the full operand (digits + one decimal point).
            let start = pos;
            let mut end = pos + c.len_utf8();
            let mut dots = 0;
            while i + 1 < chars.len() {
                let (np, nc) = chars[i + 1];
                if nc.is_ascii_digit() || (nc == '.' && dots == 0) {
                    if nc == '.' {
                        dots += 1;
                    }
                    i += 1;
                    end = np + nc.len_utf8();
                } else {
                    break;
                }
            }
            tokens.push(lower[start..end].to_string());
            operands += 1;
        } else {
            // Symbol ops only in infix position (right after an operand
            // token) — "wi-fi" is not subtraction.
            if matches!(c, '+' | '-' | '*' | '/' | '×' | '÷')
                && tokens
                    .last()
                    .is_some_and(|t| t.chars().next().is_some_and(|t| t.is_ascii_digit()))
            {
                let op = match c {
                    '+' => "+",
                    '-' => "-",
                    '*' | '×' => "*",
                    '/' | '÷' => "/",
                    _ => unreachable!(),
                };
                tokens.push(op.into());
                ops += 1;
                i += 1;
                continue;
            }
            // 'x' as multiplication — only between operands.
            if c == 'x'
                && tokens
                    .last()
                    .is_some_and(|t| t.chars().next().is_some_and(|t| t.is_ascii_digit()))
                && chars
                    .get(i + 1)
                    .is_some_and(|(_, n)| n.is_ascii_digit() || n.is_whitespace())
            {
                tokens.push("*".into());
                ops += 1;
                i += 1;
                continue;
            }
            // Word ops at word boundary, infix only.
            if c.is_alphabetic() {
                let rest = &lower[pos..];
                if let Some((word, op)) =
                    OP_WORDS
                        .iter()
                        .find(|(w, _)| rest.starts_with(w))
                        .filter(|(w, _)| {
                            rest[w.len()..]
                                .chars()
                                .next()
                                .is_none_or(|n| !n.is_alphanumeric())
                                && tokens.last().is_some_and(|t| {
                                    t.chars().next().is_some_and(|t| t.is_ascii_digit())
                                })
                        })
                {
                    tokens.push((*op).into());
                    ops += 1;
                    i += word.chars().count();
                    continue;
                }
            }
        }
        i += 1;
    }
    if operands >= 2 && ops >= 1 {
        tokens.push("=".into());
        Some(tokens)
    } else {
        None
    }
}

/// Is the goal's destination control already in its target state?
/// Used by auto-completing subgoals: "ir a cronómetro" when the
/// Cronómetro tab is already selected must not press it again — the
/// intent is satisfied and the press would be an unverifiable no-op.
/// Conservative: only toggleable/selectable roles carrying a selected
/// value count; a matching enabled button is not "already done".
pub fn goal_already_satisfied(obs: &Observation, goal: &str) -> bool {
    let gp = parse_goal(goal);
    let terms: Vec<&str> = if gp.objects.is_empty() {
        return false;
    } else {
        gp.objects.iter().map(|s| s.as_str()).collect()
    };
    obs.elements.iter().any(|e| {
        let selectable = matches!(
            e.role.as_deref(),
            Some("radio_button" | "tab" | "check_box" | "toggle")
        );
        let selected = e
            .value
            .as_deref()
            .is_some_and(|v| matches!(v, "1" | "true" | "on" | "selected" | "checked"));
        if !(selectable && selected) {
            return false;
        }
        let label = e.label().unwrap_or("").to_lowercase();
        terms.iter().any(|t| term_matches(&label, t))
    })
}

/// Localized label synonyms for calculator operators — matched against
/// element names, never assumed present.
const OP_LABELS: &[(&str, &[&str])] = &[
    ("+", &["sumar", "add", "+", "plus", "más", "mas", "suma"]),
    ("-", &["restar", "subtract", "-", "minus", "menos", "resta"]),
    (
        "*",
        &["multiplicar", "multiply", "×", "*", "por", "times", "x"],
    ),
    ("/", &["dividir", "divide", "÷", "/", "entre", "between"]),
    (
        "=",
        &[
            "es igual a",
            "equals",
            "=",
            "igual",
            "equal",
            "resultado",
            "result",
        ],
    ),
];

/// One step in the expression press-plan.
enum ExprStep {
    Digit(char),
    Op(&'static [&'static str]),
}

fn expr_step_matches(step: &ExprStep, label: &str) -> bool {
    let name = label.trim().to_lowercase();
    match step {
        ExprStep::Digit(c) => name == c.to_string(),
        ExprStep::Op(syns) => syns.contains(&name.as_str()),
    }
}

/// The next keypad press for an arithmetic goal. Progress is tracked
/// through `hist.attempts` (which labels were already pressed); a failed
/// last attempt doesn't consume a plan step.
fn expr_next_candidate(
    obs: &Observation,
    tokens: &[String],
    hist: &GenHistory,
) -> Option<CandidateAction> {
    // Flatten tokens to a press plan.
    let mut plan: Vec<ExprStep> = Vec::new();
    for t in tokens {
        if let Some((_, syns)) = OP_LABELS.iter().find(|(op, _)| *op == t.as_str()) {
            plan.push(ExprStep::Op(syns));
        } else {
            for c in t.chars() {
                if c.is_ascii_digit() {
                    plan.push(ExprStep::Digit(c));
                }
            }
        }
    }
    // Labels already pressed, in order — minus a failed last attempt.
    let mut pressed: Vec<String> = hist
        .attempts
        .iter()
        .zip(hist.attempt_names.iter())
        .filter_map(|(a, n)| match a {
            Action::Click { .. } => n.clone(),
            _ => None,
        })
        .collect();
    if hist.last_error.is_some() {
        pressed.pop();
    }
    // Longest matched prefix → next step.
    let mut si = 0;
    let mut pi = 0;
    while si < plan.len() && pi < pressed.len() {
        if expr_step_matches(&plan[si], &pressed[pi]) {
            si += 1;
            pi += 1;
        } else {
            break;
        }
    }
    let step = plan.get(si)?;
    // The element for this step must actually exist — a keypad without
    // the label means the expr path stays silent and generic rules run.
    let el = obs.elements.iter().find(|e| {
        is_pressable(e)
            && e.enabled != Some(false)
            && e.name
                .as_deref()
                .is_some_and(|n| expr_step_matches(step, n))
    })?;
    // A stalled step (pressed but world didn't move) decays so the
    // generic path or an abstain can take over.
    let stalled = pressed.last().is_some_and(|p| expr_step_matches(step, p))
        || (hist.last_error.is_some()
            && matches!(hist.attempts.last(), Some(Action::Click { .. }))
            && hist
                .attempt_names
                .last()
                .and_then(|n| n.clone())
                .is_some_and(|n| expr_step_matches(step, &n)));
    Some(CandidateAction {
        action: Action::Click {
            target: element_target(obs, el),
            button: MouseButton::Left,
            count: 1,
        },
        rationale: format!(
            "expression sequence: next keypad step {} of {}",
            si + 1,
            plan.len()
        ),
        prior: if stalled { 0.4 } else { 0.95 },
    })
}

/// Parsed goal: verbs (what to do), object terms (what to do it to),
/// and any literal text to type (quoted or after a colon).
#[derive(Debug, Default)]
struct GoalParse {
    verbs: Vec<String>,
    objects: Vec<String>,
    quoted: Option<String>,
}

fn parse_goal(goal: &str) -> GoalParse {
    let mut gp = GoalParse::default();
    // Literal text: 'quoted', "quoted" or trailing `: value`.
    let mut cleaned = goal.to_string();
    for (open, close) in [('\'', '\''), ('"', '"')] {
        if let Some(a) = cleaned.find(open) {
            if let Some(b) = cleaned[a + 1..].find(close) {
                gp.quoted = Some(cleaned[a + 1..a + 1 + b].to_string());
                cleaned = format!("{}{}", &cleaned[..a], &cleaned[a + 2 + b..]);
                break;
            }
        }
    }
    if gp.quoted.is_none() {
        if let Some(c) = cleaned.find(": ") {
            let v = cleaned[c + 2..].trim();
            if !v.is_empty() && v.len() < goal.len() / 2 + 20 {
                gp.quoted = Some(v.to_string());
                cleaned = cleaned[..c].to_string();
            }
        }
    }

    let lower = cleaned.to_lowercase();
    let mut consumed = vec![false; lower.len()];
    for phrase in PHRASE_VERBS {
        if let Some(pos) = lower.find(phrase) {
            gp.verbs.push(phrase.to_string());
            for (i, c) in consumed.iter_mut().enumerate().skip(pos) {
                if i >= pos + phrase.len() {
                    break;
                }
                *c = true;
            }
        }
    }
    for w in lower.split(|c: char| !c.is_alphanumeric()) {
        if w.is_empty() {
            continue;
        }
        let pos = lower.find(w).unwrap_or(0);
        if consumed[pos] {
            continue;
        }
        if PRESS_VERBS.contains(&w) || EDIT_VERBS.contains(&w) {
            gp.verbs.push(w.to_string());
        } else if w.len() >= 2 && !STOPWORDS.contains(&w) {
            gp.objects.push(w.to_string());
        }
    }
    gp
}

/// Prefix-style match: whole token, or a label token starting with the
/// term (covers plurals/inflections: "destinations" ⊃ "destination").
/// Mid-word substring is NOT a match — "reescribir" is not "escribir".
fn term_matches(label: &str, term: &str) -> bool {
    if label.is_empty() || term.is_empty() {
        return false;
    }
    // Phrase terms ("log in", "proceed to checkout") match by substring —
    // they were already validated as whole phrases in the goal.
    if term.contains(' ') {
        return label.contains(term);
    }
    label
        .split(|c: char| !c.is_alphanumeric())
        // Empty tokens and label-side stopwords never match — otherwise
        // "Guardar como…" stem-matches "comprar".
        .filter(|tok| !tok.is_empty() && !STOPWORDS.contains(tok))
        .any(|tok| {
            if tok == term {
                return true;
            }
            // Pure digits are identifiers, not words — "1041" is not
            // "1042". No stemming.
            if tok.chars().all(|c| c.is_ascii_digit()) || term.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            // Common-prefix stem: covers plurals ("destinations" ~
            // "destination") and gender inflection ("todas" ~ "todo",
            // which differ mid-string — a plain prefix check misses it).
            // Requires min length 4 so tiny words can't fuse; and the
            // shared prefix must be within one char of the shorter word.
            let min_len = tok.len().min(term.len());
            if min_len < 4 {
                return false;
            }
            let common = tok
                .chars()
                .zip(term.chars())
                .take_while(|(a, b)| a == b)
                .count();
            common >= min_len - 1
        })
}

fn is_editable(el: &Element) -> bool {
    el.actions.iter().any(|a| a == "set_value" || a == "focus")
        || matches!(
            el.role.as_deref(),
            Some("text_field" | "text_area" | "combo_box" | "slider" | "search_field")
        )
}

fn is_pressable(el: &Element) -> bool {
    el.actions.iter().any(|a| a == "press" || a == "show_menu")
        || matches!(
            el.role.as_deref(),
            Some(
                "button"
                    | "link"
                    | "check_box"
                    | "radio_button"
                    | "menu_item"
                    | "tab"
                    | "pop_up_button"
            )
        )
}

/// The target Dexter would pass to an action on this element.
fn element_target(obs: &Observation, el: &Element) -> Target {
    Target::Element {
        observation: obs.id,
        element: el.id,
    }
}

/// Does `attempted` point at `el`? Element targets match by id; semantic
/// targets (attempts recorded by external engines) match role+name.
fn same_element_target(attempted: &Target, el: &Element) -> bool {
    match attempted {
        Target::Element { element, .. } => element == &el.id,
        Target::Semantic(st) => st.role == el.role && st.name == el.name,
        _ => false,
    }
}

/// Does `attempts` already contain an action on this same element?
fn already_tried(el: &Element, attempts: &[Action]) -> bool {
    attempts.iter().any(|a| match a {
        Action::Click { target, .. }
        | Action::Focus { target }
        | Action::SetValue { target, .. } => same_element_target(target, el),
        Action::TypeText { target, .. } => {
            target.as_ref().is_some_and(|t| same_element_target(t, el))
        }
        _ => false,
    })
}

impl CandidateGenerator for HeuristicGenerator {
    fn generate(&self, obs: &Observation, goal: &str, hist: &GenHistory) -> Vec<CandidateAction> {
        let gp = parse_goal(goal);

        // Terms to match: objects carry the load; when a goal is verb-only
        // ("pay") the verbs themselves become the terms.
        let terms: Vec<&str> = if gp.objects.is_empty() {
            gp.verbs.iter().map(|s| s.as_str()).collect()
        } else {
            gp.objects
                .iter()
                .chain(gp.verbs.iter())
                .map(|s| s.as_str())
                .collect()
        };
        let want_edit = gp.verbs.iter().any(|v| EDIT_VERBS.contains(&v.as_str()));

        let mut out: Vec<CandidateAction> = Vec::new();
        for el in &obs.elements {
            if el.enabled == Some(false) {
                continue;
            }
            let editable = is_editable(el);
            let pressable = is_pressable(el);
            if !editable && !pressable {
                continue;
            }
            let label = el.label().unwrap_or("").to_lowercase();
            let matched = terms.iter().filter(|t| term_matches(&label, t)).count();
            if terms.is_empty() {
                continue;
            }
            if matched == 0 {
                // Focused-editable fallback: the goal wants to type and a
                // field already holds focus — the caret is the evidence,
                // no label match needed. With a literal in the goal the
                // honest offer is typing it; without one, only focus.
                if want_edit && editable && el.focused {
                    let action = match &gp.quoted {
                        Some(text) => Action::TypeText {
                            text: text.clone(),
                            target: Some(element_target(obs, el)),
                        },
                        None => Action::Focus {
                            target: element_target(obs, el),
                        },
                    };
                    out.push(CandidateAction {
                        action,
                        rationale: format!(
                            "{} already focused; edit goal needs no label match",
                            el.role.as_deref().unwrap_or("?"),
                        ),
                        prior: 0.7,
                    });
                }
                continue;
            }
            let coverage = matched as f32 / terms.len() as f32;
            // Whole label equals a goal term — the strongest signal.
            // Only object terms and phrase verbs count: a bare verb label
            // ("Cerrar") must not outrank a more specific match
            // ("Cerrar todo") when the goal names an object.
            let exact_label = terms.iter().any(|t| {
                label == *t
                    && (gp.objects.is_empty()
                        || t.contains(' ')
                        || gp.objects.iter().any(|o| o == t))
            });
            let verb_aligned = (want_edit && editable) || (!want_edit && pressable);
            let mut prior = 0.25
                + 0.55 * coverage
                + if verb_aligned { 0.2 } else { 0.0 }
                + if exact_label { 0.15 } else { 0.0 };
            // Label fully covered by goal terms ("Search" ⊂ {search,...})
            // beats a partial match ("Search destinations").
            let label_fully_covered = label
                .split(|c: char| !c.is_alphanumeric())
                .filter(|t| !t.is_empty())
                .all(|t| terms.contains(&t) || STOPWORDS.contains(&t));
            if label_fully_covered && !label.is_empty() {
                prior += 0.12;
            }
            // Clicking an editable field is *focus*, not action — penalize
            // it when the goal wants a press.
            if editable && pressable && !want_edit {
                prior *= 0.8;
            }
            if editable && el.focused && want_edit {
                prior += 0.1;
            }
            // Edit goal against a non-editable element is weak evidence —
            // pressing the "Documento" menu won't type anything. Once an
            // edit was actually attempted, though, the goal's remaining
            // intent is the press — the penalty no longer applies.
            let edit_done = hist
                .attempts
                .iter()
                .any(|a| matches!(a, Action::SetValue { .. } | Action::TypeText { .. }));
            if want_edit && !editable && !edit_done {
                prior *= 0.5;
            }
            // Delta bonus: element not present in the previous observation.
            if let Some(prev) = &hist.prev {
                let seen_before = prev
                    .elements
                    .iter()
                    .any(|p| p.role == el.role && p.name == el.name && p.name.is_some());
                if !seen_before {
                    prior += 0.15;
                }
            }
            if already_tried(el, &hist.attempts) {
                prior *= 0.35;
            }
            let prior = prior.min(1.0);
            let target = element_target(obs, el);
            let role = el.role.as_deref().unwrap_or("?");
            let name = el.label().unwrap_or("?");
            let base = format!(
                "{role} \"{name}\" matches {matched}/{} goal terms (prior {prior:.2})",
                terms.len()
            );
            if editable && want_edit {
                if let Some(text) = &gp.quoted {
                    out.push(CandidateAction {
                        action: Action::SetValue {
                            target: target.clone(),
                            value: text.clone(),
                        },
                        rationale: format!("{base}; editable + literal text in goal"),
                        prior,
                    });
                    out.push(CandidateAction {
                        action: Action::TypeText {
                            text: text.clone(),
                            target: Some(target.clone()),
                        },
                        rationale: format!("{base}; editable + literal text in goal"),
                        prior: prior * 0.92,
                    });
                } else {
                    out.push(CandidateAction {
                        action: Action::Focus { target },
                        rationale: format!("{base}; editable field, focus to prepare input"),
                        prior,
                    });
                }
            } else if pressable {
                // v2: an element advertising `open` under an open-goal
                // gets the semantic invoke — the proven affordance, not
                // just a generic press.
                let openable = el.actions.iter().any(|a| a == "open") && terms.contains(&"open");
                if openable {
                    out.push(CandidateAction {
                        action: Action::Invoke {
                            target: target.clone(),
                            action: "open".into(),
                        },
                        rationale: format!("{base}; element advertises 'open'"),
                        prior: (prior * 1.05).min(1.0),
                    });
                }
                out.push(CandidateAction {
                    action: Action::Click {
                        target,
                        button: MouseButton::Left,
                        count: 1,
                    },
                    rationale: base,
                    prior,
                });
            } else if editable {
                // Editable element matching a press-y goal ("submit the
                // search") — focus is still a plausible move.
                out.push(CandidateAction {
                    action: Action::Focus { target },
                    rationale: format!("{base}; editable fallback"),
                    prior: prior * 0.85,
                });
            }
        }
        // Arithmetic goals on a keypad world: the next press comes from
        // the expression, not from label-goal matching.
        if let Some(tokens) = expr_tokens(goal) {
            if let Some(c) = expr_next_candidate(obs, &tokens, hist) {
                out.push(c);
            }
        }
        out.sort_by(|a, b| b.prior.total_cmp(&a.prior));
        out.truncate(self.max_candidates);
        out
    }
}

/// Deterministic baseline: execute the top candidate; when nothing is
/// plausible, wait if the world looks busy, abstain otherwise; retry once
/// after an error. Ships with the runtime so `run_task` works with zero
/// model dependencies.
pub struct RuleBased {
    /// Escalate after this many consecutive steps with no candidates.
    pub max_empty_steps: u32,
    /// Minimum prior for the top candidate to be executed.
    pub act_threshold: f32,
}

impl Default for RuleBased {
    fn default() -> Self {
        Self {
            max_empty_steps: 3,
            act_threshold: 0.65,
        }
    }
}

impl DecisionEngine for RuleBased {
    fn name(&self) -> &str {
        "rule-based"
    }

    fn decide(&self, ctx: &DecisionContext) -> Result<Decision, DecisionError> {
        if let Some(first) = ctx.candidates.first() {
            // Weak evidence is not a mandate: below the act threshold the
            // honest move is to abstain, not to click the best bad guess.
            if first.prior >= self.act_threshold {
                return Ok(Decision::Act {
                    action: Box::new(first.action.clone()),
                    candidate_index: Some(0),
                    rationale: format!("top candidate: {}", first.rationale),
                });
            }
            return Ok(Decision::Route {
                route: Route::Abstain,
                rationale: format!(
                    "top candidate prior {:.2} below act threshold {:.2}",
                    first.prior, self.act_threshold
                ),
            });
        }
        if ctx.last_error.is_some() && ctx.step > 1 {
            return Ok(Decision::Route {
                route: Route::Retry,
                rationale: "no candidates; retrying after error".into(),
            });
        }
        let digest = ctx.state_digest.to_lowercase();
        let busy = [
            "loading",
            "processing",
            "please wait",
            "spinner",
            "progress",
        ]
        .iter()
        .any(|hint| digest.contains(hint));
        if busy {
            return Ok(Decision::Route {
                route: Route::Wait { millis: 500 },
                rationale: "no candidates and the world looks busy — wait".into(),
            });
        }
        Ok(Decision::Route {
            route: Route::Abstain,
            rationale: "no candidate matches the goal — abstaining".into(),
        })
    }
}
