//! `dexter` CLI — thin shell over the runtime. Every action goes through
//! `Engine::run_step` — the same policy/act/verify path MCP and SDKs use.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dexter_core::{
    Action, AppSelector, ElementId, EventKind, ExpectedState, MouseButton, ObservationScope,
    ScrollDelta, SemanticTarget, Target,
};
use dexter_decision::CandidateGenerator;
use dexter_driver::ComputerDriver;
use dexter_engine::{presence, Engine, RunConfig, Step, StepStatus};
use dexter_macos::{permissions, MacOsDriver};
use dexter_policy::Policy;
use serde::Deserialize;
use serde_json::json;
use std::process::ExitCode;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "dexter",
    version,
    about = "Dexter — agent computer runtime (macOS)",
    long_about = None
)]
struct Cli {
    /// Policy file (TOML). Default: embedded — reads allowed, every
    /// mutation requires approval.
    #[arg(long, global = true)]
    policy: Option<String>,

    /// Driver backend: `macos` (Accessibility API) or `browser`
    /// (W3C WebDriver — Safari via safaridriver, or any endpoint).
    #[arg(long, global = true, default_value = "macos")]
    driver: String,

    /// WebDriver endpoint for --driver browser (e.g.
    /// http://localhost:9515). Default: spawn `safaridriver`.
    #[arg(long, global = true)]
    browser_url: Option<String>,

    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Report driver capabilities and macOS permission state.
    Doctor {
        /// Ask macOS to show the permission prompts.
        #[arg(long)]
        request: bool,
        /// Also probe a decision engine's health (rule-based | laya).
        #[arg(long)]
        engine: Option<String>,
        /// Worker command for --engine laya (or DEXTER_LAYA_WORKER).
        #[arg(long)]
        engine_path: Option<String>,
    },
    /// List windows known to the window server.
    Windows {
        /// Filter by app name (case-insensitive substring), bundle id or pid.
        #[arg(long)]
        app: Option<String>,
    },
    /// Capture one observation: windows + accessibility tree (+ screenshot).
    Observe {
        /// Scope to an application: name, `com.bundle.id` or pid.
        #[arg(long)]
        app: Option<String>,
        /// Max accessibility-tree depth.
        #[arg(long)]
        max_depth: Option<u32>,
        /// Cap on flattened elements.
        #[arg(long)]
        max_elements: Option<usize>,
        /// Narrow the observation to one window id (bounds intersection;
        /// unpositioned elements like menubar items are dropped).
        #[arg(long)]
        window: Option<u32>,
        /// Print the text digest (decision-engine input) instead of JSON.
        #[arg(long)]
        digest: bool,
        /// Capture a screenshot to this path.
        #[arg(long)]
        screenshot: Option<String>,
        /// Opt-in OCR fallback: when the AX tree is thin/empty (or a window
        /// is scoped), recognize text in the window capture and append it as
        /// inert `[ocr]` elements.
        #[arg(long)]
        vision: bool,
    },
    /// Map an app's interface: windows, control clusters, the menubar
    /// verb vocabulary and inferred capabilities — one call answers
    /// "what is this app and what can it do" without hand-authored
    /// per-app knowledge.
    Map {
        /// Scope to an application: name, `com.bundle.id` or pid.
        #[arg(long)]
        app: String,
    },
    /// Click an element (AXPress; `point:x,y` needs --coords).
    Click {
        #[command(flatten)]
        args: ActionArgs,
        /// Mouse button: left (default), right, middle.
        #[arg(long, default_value = "left")]
        button: String,
        /// Click count 1-3. Count ≥2 prefers the element's advertised
        /// `open` action; physical multi-click needs --coords.
        #[arg(long, default_value = "1")]
        count: u8,
    },
    /// Type text into an element (AXValue first; keyboard fallback needs
    /// --coords and the app being frontmost).
    Type {
        #[command(flatten)]
        args: ActionArgs,
        #[arg(long)]
        text: String,
    },
    /// Post a key chord like "cmd+s" (requires --coords; goes to the
    /// frontmost app).
    Key {
        #[command(flatten)]
        args: ActionArgs,
        #[arg(long)]
        chord: String,
    },
    /// Scroll: a target scrolls it into view via AX; without a target it
    /// scrolls at the pointer (requires --coords).
    Scroll {
        #[command(flatten)]
        args: ActionArgs,
        #[arg(long, default_value = "0")]
        dx: f64,
        #[arg(long, default_value = "0")]
        dy: f64,
    },
    /// Focus an element (AXFocused).
    Focus {
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Set an element's value directly (AXValue).
    SetValue {
        #[command(flatten)]
        args: ActionArgs,
        #[arg(long)]
        value: String,
    },
    /// Launch an application (`/usr/bin/open`; normal activation).
    Launch {
        /// App name or `com.bundle.id` — never a pid.
        #[arg(value_name = "APP")]
        name: String,
        /// Don't bring the app to the foreground.
        #[arg(long)]
        background: bool,
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Ask an application to quit normally (NSRunningApplication
    /// terminate — apps with unsaved state may refuse).
    Quit {
        /// App name or `com.bundle.id`.
        #[arg(value_name = "APP")]
        name: String,
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Perform an action the element advertises (`open`, `confirm`,
    /// `cancel`, `pick`...). Discovery: `dexter observe` shows the
    /// element's `actions` list.
    Invoke {
        #[command(flatten)]
        args: ActionArgs,
        /// The advertised action name.
        #[arg(long)]
        action: String,
    },
    /// Window operation: new | focus | raise | close | minimize |
    /// restore | move | resize. Targets the scoped app's frontmost
    /// window unless --window is given.
    WindowOp {
        /// The operation name.
        op: String,
        /// Window id (`dexter windows`).
        #[arg(long)]
        window: Option<u32>,
        /// move/resize geometry.
        #[arg(long)]
        x: Option<f64>,
        #[arg(long)]
        y: Option<f64>,
        #[arg(long)]
        w: Option<f64>,
        #[arg(long)]
        h: Option<f64>,
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Clipboard access — reads return text on stdout; the journal
    /// never records the content (secrets tier).
    Clipboard {
        /// `read` prints the text; `write` sets it.
        op: String,
        /// Text for `write`.
        #[arg(long)]
        text: Option<String>,
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Drag between two elements. Both targets resolve and validate
    /// before the pointer moves — a stale endpoint aborts cleanly.
    Drag {
        /// Start target (same syntax as --target).
        #[arg(long)]
        from: String,
        /// End target.
        #[arg(long)]
        to: String,
        /// Gesture duration in ms.
        #[arg(long, default_value = "300")]
        duration_ms: u64,
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Run a goal in closed loop: observe → candidates → decision engine
    /// → act → re-check, until `--done` verifies or bounds hit.
    Task {
        /// The goal, verbatim ("save the document").
        goal: String,
        #[command(flatten)]
        args: TaskArgs,
    },
    /// Serve MCP over stdio — exposes observe/act/verify/task/journal to
    /// agent clients (Claude Desktop, MCP SDKs). Same engine path as CLI.
    Mcp {
        /// Decider for dexter_task: rule-based | laya.
        #[arg(long, default_value = "rule-based")]
        engine: String,
        /// Worker command for --engine laya.
        #[arg(long)]
        engine_path: Option<String>,
        /// Abstain below this calibrated confidence (laya). 0 = never.
        #[arg(long, default_value = "0")]
        min_confidence: f32,
        /// Operator: treat every RequireApproval as granted for the
        /// whole MCP session (the human approved at launch).
        #[arg(long)]
        approve_all: bool,
        /// Operator: permit physical-tier (coordinate/keyboard) input.
        /// Off by default — agents cannot enable it per call.
        #[arg(long)]
        coords: bool,
        /// Operator: show the presence overlay while tools act — the
        /// cursor flies on every dexter_act/dexter_task the agent runs.
        #[arg(long)]
        overlay: bool,
    },
    /// Open a URL — browser driver navigates its session; macOS hands it
    /// to LaunchServices. Policy-gated like any mutation.
    Navigate {
        /// Absolute URL to open.
        url: String,
        #[command(flatten)]
        args: ActionArgs,
    },
    /// Eval harness: replay labeled decision points against engines,
    /// or harvest new labeled items by observing real pages/apps.
    Eval {
        #[command(subcommand)]
        cmd: EvalCommand,
    },
    /// Run a scenario file (TOML): ordered steps through the full
    /// policy/act/verify loop, stopping at the first failure.
    Run {
        /// Path to the scenario TOML.
        path: String,
        /// Permit coordinate-level input for the whole run.
        #[arg(long)]
        coords: bool,
        /// Approve every policy-required action in the file (you are
        /// approving the whole scenario up front).
        #[arg(long)]
        approve_all: bool,
        /// Write the event journal (JSONL) to this path.
        #[arg(long)]
        events: Option<String>,
        /// Show the presence overlay: auto-spawns `dexter-overlay` on the
        /// journal (a temp file if --events isn't given). On by default
        /// when the terminal is interactive; best-effort — a missing
        /// overlay binary warns, never fails the run.
        #[arg(long)]
        overlay: bool,
        /// Turn the presence overlay off for this run.
        #[arg(long, conflicts_with = "overlay")]
        no_overlay: bool,
    },
}

#[derive(Subcommand)]
enum EvalCommand {
    /// Replay a JSONL dataset of labeled decision points against a
    /// decision engine — offline, deterministic, no machine access.
    Run {
        /// Dataset JSONL (one EvalItem per line).
        dataset: String,
        /// Decision engine under test: rule-based | laya.
        #[arg(long, default_value = "rule-based")]
        engine: String,
        /// Worker command for --engine laya.
        #[arg(long)]
        engine_path: Option<String>,
        /// Abstain below this calibrated confidence (laya). 0 = never.
        #[arg(long, default_value = "0")]
        min_confidence: f32,
        /// Emit per-item verdicts as JSONL to this path.
        #[arg(long)]
        out: Option<String>,
    },
    /// Export frozen items as (state, options, gold) training rows in
    /// the exact format the laya engine renders at inference — same
    /// generator, same digest budget, same option list.
    Export {
        /// Dataset JSONL files (one or more).
        datasets: Vec<String>,
        /// Output JSONL of training rows.
        #[arg(short, long)]
        out: String,
    },
    /// Cross-app matrix: replay every dataset against one engine and
    /// report per-app-group + per-dataset rows — the leave-one-app-out
    /// companion. Rows are grouped by harvest provenance (meta.app →
    /// meta.url → observation app).
    Matrix {
        /// Dataset JSONL files (one or more).
        datasets: Vec<String>,
        /// Decision engine under test: rule-based | laya.
        #[arg(long, default_value = "rule-based")]
        engine: String,
        /// Worker command for --engine laya.
        #[arg(long)]
        engine_path: Option<String>,
        /// Abstain below this calibrated confidence (laya). 0 = never.
        #[arg(long, default_value = "0")]
        min_confidence: f32,
    },
    /// Harvest labeled items: observe each page/app in a TOML manifest,
    /// resolve the declared gold target, emit EvalItem JSONL.
    /// Requires the driver selected by --driver (browser recommended).
    Harvest {
        /// Manifest TOML: [[page]] entries with url/app, goal, gold.
        manifest: String,
        /// Output JSONL dataset.
        #[arg(short, long)]
        out: String,
    },
    /// Task scenarios: run goal-driven tasks end-to-end on programmable
    /// sim worlds and report utility metrics — success rate, steps over
    /// optimal, per-phase latency, recoveries — optionally gated against
    /// a committed baseline (the MLOps layer).
    Scenario {
        /// Directory of scenario TOML files, or a single file.
        path: String,
        /// Decision engine under test: rule-based | laya.
        #[arg(long, default_value = "rule-based")]
        engine: String,
        /// Worker command for --engine laya.
        #[arg(long)]
        engine_path: Option<String>,
        /// Abstain below this calibrated confidence (laya). 0 = never.
        #[arg(long, default_value = "0")]
        min_confidence: f32,
        /// Repetitions per scenario — the flakiness/variance signal.
        #[arg(long, default_value = "1")]
        reps: u32,
        /// Write the metrics report (JSON) to this path.
        #[arg(long)]
        out: Option<String>,
        /// Append a timestamped run record to this JSONL history file.
        #[arg(long)]
        history: Option<String>,
        /// Baseline TOML to check against — nonzero exit on regression.
        #[arg(long)]
        check: Option<String>,
        /// Export successful runs as Laya training rows (JSONL) — same
        /// shape `eval export` emits, labelled by the decision taken.
        #[arg(long)]
        export: Option<String>,
        /// Dump each run's event journal to `<dir>/<id>.jsonl`.
        #[arg(long)]
        journal_out: Option<String>,
        /// Show the presence overlay during live macOS runs — the
        /// cursor should be seen while the suite touches real apps.
        /// On by default when the terminal is interactive.
        #[arg(long)]
        overlay: bool,
        /// Turn the presence overlay off.
        #[arg(long, conflicts_with = "overlay")]
        no_overlay: bool,
    },
}

/// Flags for the closed-loop `task` command.
#[derive(clap::Args)]
struct TaskArgs {
    /// Structural completion check (JSON ExpectedState).
    #[arg(long)]
    done: String,
    /// Decision engine: `rule-based` | `laya` (needs --engine-path or
    /// DEXTER_LAYA_WORKER).
    #[arg(long, default_value = "rule-based")]
    engine: String,
    /// Worker command for --engine laya (NDJSON sidecar). Defaults to
    /// $DEXTER_LAYA_WORKER or the repo's dev worker.
    #[arg(long)]
    engine_path: Option<String>,
    /// For engines reporting calibrated confidence (laya): abstain
    /// instead of acting below this threshold. 0 = never gate.
    #[arg(long, default_value = "0")]
    min_confidence: f32,
    /// Scope to an application.
    #[arg(long)]
    app: Option<String>,
    /// Max decide/act iterations.
    #[arg(long, default_value = "10")]
    max_steps: u32,
    /// Wall-clock budget in seconds (checked per step, alongside
    /// --max-steps).
    #[arg(long)]
    max_secs: Option<u64>,
    /// Permit coordinate-level input.
    #[arg(long)]
    coords: bool,
    /// Approve every policy-required action (you approve the goal).
    #[arg(long)]
    approve_all: bool,
    /// Write the event journal (JSONL) to this path.
    #[arg(long)]
    events: Option<String>,
    /// Show the presence overlay: auto-spawns `dexter-overlay` on the
    /// journal (a temp file if --events isn't given). On by default
    /// when the terminal is interactive; best-effort — a missing
    /// overlay binary warns, never fails the task.
    #[arg(long)]
    overlay: bool,
    /// Turn the presence overlay off for this task.
    #[arg(long, conflicts_with = "overlay")]
    no_overlay: bool,
}

/// Shared flags for single-action commands.
#[derive(clap::Args)]
struct ActionArgs {
    /// Scope to an application: name, `com.bundle.id` or pid.
    #[arg(long)]
    app: Option<String>,
    /// Target: `{"role":"button","name":"Save"}` | `element:N` |
    /// `focused` | `point:x,y`. Required except for `type`/`scroll`
    /// (which default to focused/pointer).
    #[arg(long)]
    target: Option<String>,
    /// Permit coordinate-level input (moves the real cursor).
    #[arg(long)]
    coords: bool,
    /// Human approval for this exact action (when policy requires it).
    #[arg(long)]
    approve: bool,
    /// Post-condition to verify (JSON ExpectedState), retried up to
    /// --attempts.
    #[arg(long)]
    expect: Option<String>,
    /// Attempt bound for verify loops.
    #[arg(long, default_value = "3")]
    attempts: u32,
    /// Write the action's event journal (JSONL) to this path.
    #[arg(long)]
    events: Option<String>,
    /// Show the presence overlay for this action. On by default when
    /// the terminal is interactive — every act should be seen.
    #[arg(long)]
    overlay: bool,
    /// Turn the presence overlay off for this action.
    #[arg(long, conflicts_with = "overlay")]
    no_overlay: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("dexter: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn load_policy(path: &Option<String>) -> Result<Policy> {
    match path {
        Some(p) => {
            Policy::load(std::path::Path::new(p)).with_context(|| format!("loading policy '{p}'"))
        }
        None => Ok(Policy::embedded()),
    }
}

/// Build the decision engine for `task`/`mcp`/`doctor --engine`.
/// `laya` spawns the sidecar worker; a spawn failure is an error,
/// never a silent fallback to rule-based.
fn build_decider(
    engine_name: &str,
    engine_path: &Option<String>,
    min_confidence: f32,
) -> Result<Box<dyn dexter_decision::DecisionEngine>> {
    match engine_name {
        "rule-based" => Ok(Box::new(dexter_decision::RuleBased::default())),
        "laya" => {
            let cmd = engine_path
                .clone()
                .or_else(|| std::env::var("DEXTER_LAYA_WORKER").ok())
                .unwrap_or_else(|| "python3 workers/laya/worker.py --provider dev".to_string());
            let engine = dexter_laya::LayaEngine::spawn(&cmd, Duration::from_secs(30))
                .with_context(|| format!("spawning laya worker '{cmd}'"))?
                .with_min_confidence(min_confidence);
            Ok(Box::new(engine))
        }
        other => {
            anyhow::bail!("unknown decision engine '{other}' — available: rule-based, laya")
        }
    }
}

/// Build the selected driver. `browser` spawns `safaridriver` unless
/// `--browser-url` points at an already-running endpoint.
fn build_driver(cli: &Cli) -> Result<Box<dyn ComputerDriver>> {
    match cli.driver.as_str() {
        "macos" => Ok(Box::new(MacOsDriver::new())),
        "browser" => match &cli.browser_url {
            Some(url) => Ok(Box::new(
                dexter_browser::BrowserDriver::connect_attach(url, browser_label(url))
                    .map_err(|e| anyhow::anyhow!("browser driver at {url}: {e}"))?,
            )),
            None => Ok(Box::new(
                dexter_browser::BrowserDriver::safari()
                    .map_err(|e| anyhow::anyhow!("safaridriver: {e}"))?,
            )),
        },
        other => anyhow::bail!("unknown driver '{other}' — available: macos, browser"),
    }
}

/// Driver label heuristic for a WebDriver endpoint URL.
fn browser_label(url: &str) -> &'static str {
    if url.contains("4444") {
        "firefox"
    } else if url.contains("9515") {
        "chrome"
    } else {
        "browser"
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let policy = load_policy(&cli.policy)?;
    let is_browser = cli.driver == "browser";
    let mut engine = Engine::new(build_driver(&cli)?, policy, Duration::from_secs(300));

    match cli.cmd {
        Command::Doctor {
            request,
            engine: engine_name,
            engine_path,
        } => doctor(
            engine.driver(),
            request,
            is_browser,
            engine_name,
            engine_path,
        ),
        Command::Navigate { url, args } => run_action(&mut engine, &args, Action::Navigate { url }),
        Command::Windows { app } => windows(engine.driver(), app),
        Command::Observe {
            app,
            max_depth,
            max_elements,
            window,
            digest,
            screenshot,
            vision,
        } => {
            let scope = ObservationScope {
                app: app.as_deref().map(AppSelector::parse),
                window,
                max_depth: max_depth.unwrap_or(40),
                max_elements: max_elements.unwrap_or(4_000),
                screenshot: screenshot.is_some(),
                vision,
                screenshot_path: screenshot,
            };
            observe(engine.driver(), &scope, digest)
        }
        Command::Map { app } => {
            let scope = ObservationScope {
                app: Some(AppSelector::parse(&app)),
                ..Default::default()
            };
            let mut obs = engine
                .driver()
                .observe(&scope)
                .context("map observe failed")?;
            // AX exposes window content only while the app is frontmost
            // — if the map is windowless, wake once, re-observe, then
            // hand focus back. Same contract as the live-scenario runner.
            if !obs
                .elements
                .iter()
                .any(|e| e.role.as_deref() == Some("window"))
            {
                let handle = engine
                    .driver()
                    .wake(&AppSelector::parse(&app))
                    .context("map wake failed")?;
                if handle.activated {
                    std::thread::sleep(Duration::from_millis(800));
                    obs = engine
                        .driver()
                        .observe(&scope)
                        .context("map re-observe failed")?;
                    engine.driver().restore(&handle);
                }
            }
            let map = dexter_world_model::app_map(&obs);
            println!("{}", serde_json::to_string_pretty(&map)?);
            Ok(())
        }
        Command::Click {
            args,
            button,
            count,
        } => {
            let button = match button.as_str() {
                "left" => MouseButton::Left,
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                other => anyhow::bail!("unknown button '{other}' (left|right|middle)"),
            };
            let target = resolve_target(engine.driver(), &args)?;
            run_action(
                &mut engine,
                &args,
                Action::Click {
                    target,
                    button,
                    count,
                },
            )
        }
        Command::Type { args, text } => {
            let target = args
                .target
                .as_deref()
                .map(|_| resolve_target(engine.driver(), &args))
                .transpose()?;
            run_action(&mut engine, &args, Action::TypeText { text, target })
        }
        Command::Key { args, chord } => {
            let chord = dexter_core::KeyChord::parse(&chord)?;
            run_action(&mut engine, &args, Action::Key { chord })
        }
        Command::Scroll { args, dx, dy } => {
            let target = args
                .target
                .as_deref()
                .map(|_| resolve_target(engine.driver(), &args))
                .transpose()?;
            run_action(
                &mut engine,
                &args,
                Action::Scroll {
                    delta: ScrollDelta { dx, dy },
                    target,
                },
            )
        }
        Command::Focus { args } => {
            let target = resolve_target(engine.driver(), &args)?;
            run_action(&mut engine, &args, Action::Focus { target })
        }
        Command::SetValue { args, value } => {
            let target = resolve_target(engine.driver(), &args)?;
            run_action(&mut engine, &args, Action::SetValue { target, value })
        }
        Command::Launch {
            name,
            background,
            args,
        } => run_action(
            &mut engine,
            &args,
            Action::LaunchApp {
                app: AppSelector::parse(&name),
                activate: !background,
            },
        ),
        Command::Quit { name, args } => run_action(
            &mut engine,
            &args,
            Action::QuitApp {
                app: AppSelector::parse(&name),
            },
        ),
        Command::Invoke { args, action } => {
            let target = resolve_target(engine.driver(), &args)?;
            run_action(&mut engine, &args, Action::Invoke { target, action })
        }
        Command::WindowOp {
            op,
            window,
            x,
            y,
            w,
            h,
            args,
        } => {
            use dexter_core::WindowOperation as Op;
            let operation = match op.as_str() {
                "new" => Op::New,
                "focus" => Op::Focus,
                "raise" => Op::Raise,
                "close" => Op::Close,
                "minimize" => Op::Minimize,
                "restore" => Op::Restore,
                "move" => Op::Move {
                    x: x.context("--x required for move")?,
                    y: y.context("--y required for move")?,
                },
                "resize" => Op::Resize {
                    width: w.context("--w required for resize")?,
                    height: h.context("--h required for resize")?,
                },
                other => anyhow::bail!(
                    "unknown window op '{other}' (new|focus|raise|close|minimize|restore|move|resize)"
                ),
            };
            run_action(
                &mut engine,
                &args,
                Action::Window {
                    window_id: window,
                    operation,
                },
            )
        }
        Command::Clipboard { op, text, args } => match op.as_str() {
            "read" => run_action(&mut engine, &args, Action::ReadClipboardText),
            "write" => run_action(
                &mut engine,
                &args,
                Action::WriteClipboardText {
                    text: text.context("--text required for clipboard write")?,
                },
            ),
            other => anyhow::bail!("unknown clipboard op '{other}' (read|write)"),
        },
        Command::Drag {
            from,
            to,
            duration_ms,
            args,
        } => {
            let app = args.app.as_deref();
            let from = resolve_raw_target(engine.driver(), app, &from)?;
            let to = resolve_raw_target(engine.driver(), app, &to)?;
            run_action(
                &mut engine,
                &args,
                Action::Drag {
                    from,
                    to,
                    duration_ms,
                },
            )
        }
        Command::Run {
            path,
            coords,
            approve_all,
            events,
            overlay,
            no_overlay,
        } => run_scenario(
            &mut engine,
            &path,
            coords,
            approve_all,
            events,
            presence_wanted(overlay, no_overlay),
        ),
        Command::Task { goal, args } => run_task(&mut engine, &goal, args),
        Command::Eval { cmd } => match cmd {
            EvalCommand::Run {
                dataset,
                engine: eng,
                engine_path,
                min_confidence,
                out,
            } => eval_run(&dataset, &eng, engine_path, min_confidence, out),
            EvalCommand::Export { datasets, out } => eval_export(&datasets, &out),
            EvalCommand::Matrix {
                datasets,
                engine,
                engine_path,
                min_confidence,
            } => eval_matrix(&datasets, &engine, engine_path, min_confidence),
            EvalCommand::Harvest { manifest, out } => {
                eval_harvest(engine.driver(), &manifest, &out)
            }
            EvalCommand::Scenario {
                path,
                engine: eng,
                engine_path,
                min_confidence,
                reps,
                out,
                history,
                check,
                export,
                journal_out,
                overlay,
                no_overlay,
            } => eval_scenario(
                &path,
                &eng,
                engine_path,
                min_confidence,
                reps,
                out,
                history,
                check,
                export,
                journal_out,
                cli.browser_url.clone(),
                presence_wanted(overlay, no_overlay),
            ),
        },
        Command::Mcp {
            engine: ref eng,
            ref engine_path,
            min_confidence,
            approve_all,
            coords,
            overlay,
        } => run_mcp(
            &cli,
            eng,
            engine_path.clone(),
            min_confidence,
            dexter_mcp::ServerConfig {
                approve_all,
                allow_coords: coords,
                presence: overlay,
            },
        ),
    }
}

fn run_mcp(
    cli: &Cli,
    engine_name: &str,
    engine_path: Option<String>,
    min_confidence: f32,
    config: dexter_mcp::ServerConfig,
) -> Result<()> {
    // MCP owns its own engine (persistent session) — the CLI's engine is
    // dropped. Policy and driver come from the global flags.
    let policy = load_policy(&cli.policy)?;
    let driver = build_driver(cli)?;
    // The task decider is spawned once at server start — a laya worker
    // loads its model here rather than per dexter_task call.
    let decider: Option<Box<dyn dexter_decision::DecisionEngine>> = match engine_name {
        "rule-based" => None, // DexterMcp defaults to RuleBased
        "laya" => {
            let cmd = engine_path
                .or_else(|| std::env::var("DEXTER_LAYA_WORKER").ok())
                .unwrap_or_else(|| "python3 workers/laya/worker.py --provider dev".to_string());
            Some(Box::new(
                dexter_laya::LayaEngine::spawn(&cmd, Duration::from_secs(30))
                    .with_context(|| format!("spawning laya worker '{cmd}'"))?
                    .with_min_confidence(min_confidence),
            ))
        }
        other => anyhow::bail!("unknown engine '{other}' — rule-based, laya"),
    };
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("tokio runtime")?
        .block_on(dexter_mcp::serve_stdio(policy, driver, decider, config))
}

/// Parse a `--target` flag into a `Target`. `element:N` takes a fresh
/// observation first — element ids are only meaningful against the
/// observation that produced them, and only inside this process.
fn resolve_target(driver: &dyn ComputerDriver, args: &ActionArgs) -> Result<Target> {
    let raw = args
        .target
        .as_deref()
        .context("this command requires --target")?;
    resolve_raw_target(driver, args.app.as_deref(), raw)
}

/// One raw target string → `Target`. Shared by `--target`, `--from`
/// and `--to` — the syntax is identical everywhere.
fn resolve_raw_target(driver: &dyn ComputerDriver, app: Option<&str>, raw: &str) -> Result<Target> {
    if let Some(rest) = raw.strip_prefix("element:") {
        let n: u64 = rest
            .parse()
            .with_context(|| format!("invalid element id '{rest}'"))?;
        let scope = ObservationScope {
            app: app.map(AppSelector::parse),
            ..Default::default()
        };
        let obs = driver
            .observe(&scope)
            .context("observe for element target")?;
        return Ok(Target::Element {
            observation: obs.id,
            element: ElementId(n),
        });
    }
    if raw == "focused" {
        return Ok(Target::Focused);
    }
    if let Some(rest) = raw.strip_prefix("window:") {
        let n: u32 = rest
            .parse()
            .with_context(|| format!("invalid window id '{rest}'"))?;
        return Ok(Target::Window { window_id: n });
    }
    if let Some(rest) = raw.strip_prefix("point:") {
        let (x, y) = rest
            .split_once(',')
            .with_context(|| format!("invalid point '{rest}' — expected x,y"))?;
        return Ok(Target::Point {
            x: x.parse().with_context(|| format!("invalid x '{x}'"))?,
            y: y.parse().with_context(|| format!("invalid y '{y}'"))?,
        });
    }
    if raw.starts_with('{') {
        let t: SemanticTarget =
            serde_json::from_str(raw).with_context(|| format!("invalid target JSON '{raw}'"))?;
        return Ok(Target::Semantic(t));
    }
    anyhow::bail!(
        "invalid target '{raw}' — use a semantic JSON object, `element:N`, \
         `window:N`, `focused` or `point:x,y`"
    )
}

/// If the scoped app exposes no AX window content, borrow the stage:
/// one bounded activation + settle. The handle restores the previous
/// frontmost app — callers restore when done. `None` when the window
/// layer is already visible or no wake was needed.
fn wake_if_windowless(
    engine: &Engine<Box<dyn ComputerDriver>>,
    app: &Option<AppSelector>,
    settle: Duration,
) -> Option<dexter_driver::WakeHandle> {
    let sel = app.as_ref()?;
    let scope = ObservationScope {
        app: Some(sel.clone()),
        ..Default::default()
    };
    let obs = engine.driver().observe(&scope).ok()?;
    if obs
        .elements
        .iter()
        .any(|e| e.role.as_deref() == Some("window"))
    {
        return None;
    }
    let h = engine.driver().wake(sel).ok()?;
    if h.activated {
        std::thread::sleep(settle);
        Some(h)
    } else {
        None
    }
}

fn run_action(
    engine: &mut Engine<Box<dyn ComputerDriver>>,
    args: &ActionArgs,
    action: Action,
) -> Result<()> {
    let app = args.app.as_deref().map(AppSelector::parse);
    let expect = args
        .expect
        .as_deref()
        .map(serde_json::from_str::<ExpectedState>)
        .transpose()
        .context("invalid --expect JSON")?;
    if args.coords {
        // --coords is the user's physical-input consent: it lifts the
        // policy's implicit deny and permits coordinate mechanisms.
        engine.permit_physical();
    }
    let cfg = RunConfig {
        app: app.clone(),
        max_attempts: args.attempts,
        allow_coordinates: args.coords,
        approve_all: args.approve,
        verify_delay: Duration::from_millis(250),
        post_act_settle: Duration::ZERO,
        observe_max_elements: 4_000,
    };
    let step = Step {
        note: None,
        action,
        expect,
        max_attempts: Some(args.attempts),
        app: app.clone(),
    };
    // Presence: single acts journal to a live sink so the overlay can
    // draw the cursor on the action's target while it happens.
    let presence = presence_wanted(args.overlay, args.no_overlay);
    let events_path = args
        .events
        .clone()
        .or_else(|| presence.then(overlay_journal_path));
    if let Some(path) = &events_path {
        engine
            .set_journal_sink(std::path::Path::new(path))
            .with_context(|| format!("opening events sink '{path}'"))?;
        if presence {
            spawn_overlay(path);
        }
    }
    // Borrow the stage when the target's window layer is hidden —
    // background-first with one bounded wake, then hand focus back.
    let wake = wake_if_windowless(engine, &app, Duration::from_millis(800));
    let status = engine.run_step(&step, &cfg);
    if events_path.is_some() {
        let (kind, data) = match &status {
            StepStatus::Done { .. } => (EventKind::TaskCompleted, json!({"steps": 1})),
            StepStatus::Denied { reason } => (
                EventKind::TaskFailed,
                json!({"outcome": "denied", "reason": reason}),
            ),
            StepStatus::NeedsApproval { reason, .. } => (
                EventKind::TaskFailed,
                json!({"outcome": "escalated", "reason": reason}),
            ),
            StepStatus::Failed { reason, .. } => (
                EventKind::TaskFailed,
                json!({"outcome": "failed", "reason": reason}),
            ),
            StepStatus::Errored { error } => (
                EventKind::TaskFailed,
                json!({"outcome": "failed", "reason": error.to_string()}),
            ),
        };
        engine.emit(kind, data);
    }
    if let Some(h) = wake {
        engine.driver().restore(&h);
    }
    print_status(&status);
    if status.done() {
        Ok(())
    } else {
        anyhow::bail!("step did not complete")
    }
}

fn print_status(status: &StepStatus) {
    match status {
        StepStatus::Done {
            result,
            verification,
            attempts,
        } => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "done",
                    "attempts": attempts,
                    "result": result,
                    "verification": verification,
                })
            );
        }
        StepStatus::Denied { reason } => {
            println!(
                "{}",
                serde_json::json!({"status": "denied", "reason": reason})
            );
        }
        StepStatus::NeedsApproval {
            fingerprint,
            reason,
        } => {
            println!(
                "{}",
                serde_json::json!({
                    "status": "needs_approval",
                    "reason": reason,
                    "fingerprint": fingerprint,
                    "hint": "re-run with --approve, or add the fingerprint to a scenario's grants",
                })
            );
        }
        StepStatus::Failed { reason, attempts } => {
            println!(
                "{}",
                serde_json::json!({"status": "failed", "reason": reason, "attempts": attempts})
            );
        }
        StepStatus::Errored { error } => {
            println!(
                "{}",
                serde_json::json!({"status": "error", "error": error.to_string()})
            );
        }
    }
}

#[derive(Deserialize)]
struct ScenarioFile {
    #[serde(default)]
    app: Option<String>,
    #[serde(default)]
    grants: Vec<String>,
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    verify_delay_ms: Option<u64>,
    #[serde(rename = "step", default)]
    steps: Vec<ScenarioStep>,
}

#[derive(Deserialize)]
struct ScenarioStep {
    #[serde(default)]
    note: Option<String>,
    action: Action,
    #[serde(default)]
    expect: Option<ExpectedState>,
    #[serde(default)]
    max_attempts: Option<u32>,
    /// Per-step app override (name/bundle/pid syntax).
    #[serde(default)]
    app: Option<String>,
}

fn run_scenario(
    engine: &mut Engine<Box<dyn ComputerDriver>>,
    path: &str,
    coords: bool,
    approve_all: bool,
    events_path: Option<String>,
    overlay: bool,
) -> Result<()> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading scenario '{path}'"))?;
    let file: ScenarioFile =
        toml::from_str(&text).with_context(|| format!("parsing scenario '{path}'"))?;
    for fp in &file.grants {
        engine.grant_approval(fp);
    }
    if coords {
        engine.permit_physical();
    }
    let events_path = events_path.or_else(|| overlay.then(overlay_journal_path));
    if let Some(path) = &events_path {
        // Live stream: a presence overlay tails this file mid-run.
        engine
            .set_journal_sink(std::path::Path::new(path))
            .with_context(|| format!("opening events sink '{path}'"))?;
        if overlay {
            spawn_overlay(path);
        }
    }
    let cfg = RunConfig {
        app: file.app.as_deref().map(AppSelector::parse),
        max_attempts: file.max_attempts.unwrap_or(3),
        verify_delay: Duration::from_millis(file.verify_delay_ms.unwrap_or(250)),
        post_act_settle: Duration::ZERO,
        allow_coordinates: coords,
        approve_all,
        observe_max_elements: 4_000,
    };
    let steps: Vec<Step> = file
        .steps
        .into_iter()
        .map(|s| Step {
            note: s.note,
            action: s.action,
            expect: s.expect,
            max_attempts: s.max_attempts,
            app: s.app.as_deref().map(AppSelector::parse),
        })
        .collect();
    let report = engine.run_scenario(&steps, &cfg);

    for (i, status) in &report.steps {
        print!("step {i}: ");
        print_status(status);
    }
    if report.ok() {
        Ok(())
    } else {
        anyhow::bail!("scenario failed")
    }
}

fn run_task(
    engine: &mut Engine<Box<dyn ComputerDriver>>,
    goal: &str,
    args: TaskArgs,
) -> Result<()> {
    let done_when: ExpectedState =
        serde_json::from_str(&args.done).context("invalid --done ExpectedState JSON")?;
    let decider = build_decider(&args.engine, &args.engine_path, args.min_confidence)?;
    let generator = dexter_decision::HeuristicGenerator::default();
    if args.coords {
        engine.permit_physical();
    }
    let presence = presence_wanted(args.overlay, args.no_overlay);
    let events_path = args
        .events
        .clone()
        .or_else(|| presence.then(overlay_journal_path));
    if let Some(path) = &events_path {
        engine
            .set_journal_sink(std::path::Path::new(path))
            .with_context(|| format!("opening events sink '{path}'"))?;
        if presence {
            spawn_overlay(path);
        }
    }
    // Sequential goals: "escribir 'x' y guardar" runs as two subgoals,
    // the --done expectation belonging to the last. Single-intent goals
    // are a one-subgoal plan — identical behavior.
    let parts = dexter_decision::split_goal(goal);
    let last = parts.len() - 1;
    let subgoals: Vec<dexter_engine::Subgoal> = parts
        .iter()
        .enumerate()
        .map(|(i, g)| dexter_engine::Subgoal {
            goal: g.clone(),
            done_when: (i == last).then(|| done_when.clone()),
        })
        .collect();
    let app_sel = args.app.as_deref().map(AppSelector::parse);
    // Same bounded-borrow contract as single actions: wake the app if
    // its window layer is hidden, restore focus when the plan ends.
    let wake = wake_if_windowless(engine, &app_sel, Duration::from_millis(800));
    let outcome = engine.run_plan(
        &subgoals,
        &generator,
        decider.as_ref(),
        &dexter_engine::TaskConfig {
            run: RunConfig {
                app: app_sel.clone(),
                max_attempts: 1,
                verify_delay: Duration::from_millis(250),
                post_act_settle: Duration::ZERO,
                allow_coordinates: args.coords,
                approve_all: args.approve_all,
                observe_max_elements: 4_000,
            },
            max_steps: args.max_steps,
            max_duration: args.max_secs.map(Duration::from_secs),
            cancel: None,
            done_when,
        },
    );
    if let Some(h) = wake {
        engine.driver().restore(&h);
    }

    use dexter_engine::{PlanOutcome, TaskOutcome};
    let outcome = match outcome {
        PlanOutcome::Completed { steps, .. } => TaskOutcome::Completed { steps },
        PlanOutcome::Failed {
            index, goal, inner, ..
        } => {
            eprintln!("subgoal {index} '{goal}' failed");
            *inner
        }
    };
    match outcome {
        TaskOutcome::Completed { steps } => {
            println!(
                "{}",
                serde_json::json!({"status": "completed", "steps": steps})
            );
            Ok(())
        }
        TaskOutcome::Abstained { reason } => {
            println!(
                "{}",
                serde_json::json!({"status": "abstained", "reason": reason})
            );
            anyhow::bail!("task abstained")
        }
        TaskOutcome::Escalated { route, reason } => {
            println!(
                "{}",
                serde_json::json!({"status": "escalated", "route": format!("{route:?}"), "reason": reason})
            );
            anyhow::bail!("task escalated")
        }
        TaskOutcome::NeedsApproval {
            fingerprint,
            reason,
        } => {
            println!(
                "{}",
                serde_json::json!({"status": "needs_approval", "fingerprint": fingerprint, "reason": reason})
            );
            anyhow::bail!("task needs approval")
        }
        TaskOutcome::Denied { reason } => {
            println!(
                "{}",
                serde_json::json!({"status": "denied", "reason": reason})
            );
            anyhow::bail!("task denied by policy")
        }
        TaskOutcome::Failed { reason } => {
            println!(
                "{}",
                serde_json::json!({"status": "failed", "reason": reason})
            );
            anyhow::bail!("task failed")
        }
        TaskOutcome::MaxSteps => {
            println!("{}", serde_json::json!({"status": "max_steps"}));
            anyhow::bail!("task hit step bound without completing")
        }
        TaskOutcome::Cancelled => {
            println!("{}", serde_json::json!({"status": "cancelled"}));
            anyhow::bail!("task cancelled")
        }
        TaskOutcome::TimedOut { elapsed } => {
            println!(
                "{}",
                serde_json::json!({"status": "timed_out", "elapsed_ms": elapsed.as_millis() as u64})
            );
            anyhow::bail!("task exceeded its time budget")
        }
    }
}

fn doctor(
    driver: &dyn ComputerDriver,
    request: bool,
    is_browser: bool,
    engine_name: Option<String>,
    engine_path: Option<String>,
) -> Result<()> {
    let caps = driver.capabilities();
    println!("driver: {}", caps.name);
    if let Some(name) = &engine_name {
        let decider = build_decider(name, &engine_path, 0.0)?;
        use dexter_decision::EngineHealth;
        match decider.health() {
            EngineHealth::Ready => println!("engine '{}': ready", decider.name()),
            EngineHealth::Degraded(d) => println!("engine '{}': DEGRADED — {d}", decider.name()),
            EngineHealth::Down(d) => println!("engine '{}': DOWN — {d}", decider.name()),
        }
    }
    if is_browser {
        println!("element tree: {}", onoff(caps.element_tree));
        println!("screenshots: {}", onoff(caps.screenshots));
        println!("background input: {}", onoff(caps.background_input));
        println!(
            "\nnote: browser driver needs a WebDriver endpoint — `safaridriver`\
             requires Safari Settings > Developer > 'Allow Remote Automation'."
        );
        return Ok(());
    }
    if request {
        permissions::request_accessibility();
        permissions::request_screen_capture();
    }
    let ax = permissions::accessibility_trusted();
    let sc = permissions::screen_capture_allowed();
    println!("accessibility permission: {}", onoff(ax));
    println!("screen recording permission: {}", onoff(sc));
    println!("element tree: {}", onoff(caps.element_tree));
    println!("screenshots: {}", onoff(caps.screenshots));
    println!("background input: {}", onoff(caps.background_input));
    if !ax {
        println!(
            "\nto grant accessibility: System Settings > Privacy & Security > \
             Accessibility, add this terminal/binary. Or run `dexter doctor --request`."
        );
    }
    if !sc {
        println!(
            "to grant screen recording: System Settings > Privacy & Security > \
             Screen Recording. Without it, window titles and screenshots are unavailable."
        );
    }
    println!(
        "\nnote: `observe` reports `ax_limited` when the AX tree comes back \
         degraded — that means the permission applies to the launching \
         terminal, not this binary. Add the dexter binary to Accessibility \
         in System Settings to get full trees."
    );
    Ok(())
}

fn onoff(v: bool) -> &'static str {
    if v {
        "granted"
    } else {
        "missing"
    }
}

fn windows(driver: &dyn ComputerDriver, app: Option<String>) -> Result<()> {
    let mut windows = driver.windows().context("listing windows")?;
    if let Some(filter) = app {
        let sel = AppSelector::parse(&filter);
        match sel {
            AppSelector::Pid(pid) => windows.retain(|w| w.pid == pid),
            AppSelector::BundleId(b) => {
                windows.retain(|w| w.app.to_lowercase().contains(&b.to_lowercase()))
            }
            AppSelector::Name(n) => {
                let needle = n.to_lowercase();
                windows.retain(|w| w.app.to_lowercase().contains(&needle))
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&windows)?);
    Ok(())
}

fn observe(driver: &dyn ComputerDriver, scope: &ObservationScope, digest: bool) -> Result<()> {
    let obs = driver.observe(scope).context("observe failed")?;
    let obs = match scope.window {
        Some(id) => dexter_world_model::scope_to_window(obs, id).map_err(anyhow::Error::msg)?,
        None => obs,
    };
    if digest {
        println!("{}", obs.digest);
    } else {
        if obs.elements_truncated {
            eprintln!(
                "dexter: element list truncated at scope limits — \
                 `not found` results are not definitive"
            );
        }
        if obs.ax_limited {
            eprintln!(
                "dexter: AX tree degraded (ax_limited) — the accessibility \
                 grant likely applies to your terminal, not this binary"
            );
        }
        println!("{}", serde_json::to_string_pretty(&obs)?);
    }
    Ok(())
}

// ---- eval harness ----

#[derive(Deserialize)]
struct HarvestManifest {
    #[serde(default)]
    page: Vec<HarvestPage>,
}

#[derive(Deserialize)]
struct HarvestPage {
    id: String,
    /// Browser: URL to navigate. macOS: omit and use `app` instead.
    url: Option<String>,
    /// Browser: HTML file relative to the manifest's directory.
    path: Option<String>,
    /// macOS: scope the observation to this app.
    app: Option<String>,
    /// Shell command to put the app in the intended state before
    /// observing (e.g. osascript making a new document). Harvest
    /// tooling only — never part of the agent path.
    #[serde(default)]
    prep: Option<String>,
    /// Shell command to restore state after observing (e.g. close the
    /// scratch document without saving).
    #[serde(default)]
    teardown: Option<String>,
    goal: String,
    gold: HarvestGold,
    /// Extra settle time after navigate/scope before observing (ms).
    #[serde(default)]
    settle_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HarvestGold {
    /// The correct move is acting on the element matching `target`.
    Act { target: SemanticTarget },
    /// The correct move is a route (wait/abstain/escalate/...).
    Route { route: dexter_decision::Route },
}

/// Export frozen eval items as laya-format training rows:
/// {id, state, options, n_candidates, gold_index|null, gold_route}.
/// `gold_index` is the absolute option index (candidates first, then the
/// fixed route options). `null` = uncovered act-gold — ambiguous label,
/// training should skip it.
fn eval_export(datasets: &[String], out: &str) -> Result<()> {
    let generator = dexter_decision::HeuristicGenerator::default();
    let mut buf = String::new();
    let mut n_rows = 0usize;
    let mut n_skipped = 0usize;
    for ds in datasets {
        let text =
            std::fs::read_to_string(ds).with_context(|| format!("reading dataset '{ds}'"))?;
        let items = dexter_eval::load_jsonl(&text).with_context(|| format!("parsing '{ds}'"))?;
        for item in &items {
            let obs = &item.observation;
            let candidates =
                generator.generate(obs, &item.goal, &dexter_decision::GenHistory::default());
            let ctx = dexter_decision::DecisionContext {
                goal: item.goal.clone(),
                state_digest: dexter_world_model::digest_budget(obs, 14_000),
                candidates,
                last_error: None,
                step: 1,
            };
            let (state, q) = dexter_laya::build_question(&ctx);
            let options = match &q {
                dexter_decision::Question::Choice { options, .. } => options.clone(),
                _ => unreachable!("build_question always emits Choice"),
            };
            let n_cands = ctx.candidates.len();
            let (gold_index, gold_route) = match &item.gold {
                dexter_eval::Gold::Route { route } => {
                    // Same variant-level mapping the scorer uses — gold
                    // Wait{1000} lands on the wait option.
                    let variant = dexter_eval::route_variant(route);
                    let slot = dexter_laya::ROUTE_VARIANT_ORDER
                        .iter()
                        .position(|v| *v == variant);
                    (slot.map(|s| n_cands + s), Some(variant))
                }
                _ => (
                    dexter_eval::gold_candidate_index(&item.gold, &ctx, obs),
                    None,
                ),
            };
            if gold_index.is_none() {
                n_skipped += 1;
            }
            buf.push_str(&serde_json::to_string(&serde_json::json!({
                "id": item.id,
                // Provenance group — leave-one-app-out training filters
                // rows by this key (same grouping `eval matrix` reports).
                "app": dexter_eval::app_key(item),
                "state": state,
                "options": options,
                "n_candidates": n_cands,
                "gold_index": gold_index,
                "gold_route": gold_route,
            }))?);
            buf.push('\n');
            n_rows += 1;
        }
    }
    std::fs::write(out, &buf).with_context(|| format!("writing '{out}'"))?;
    println!("{n_rows} rows exported to {out} ({n_skipped} uncovered/ambiguous golds)");
    Ok(())
}

/// One matrix row: label → eval report. Shared formatting with
/// `eval run`'s summary line.
fn matrix_row(label: &str, report: &dexter_eval::EvalReport) {
    let act_items = report.covered.saturating_sub(report.route_items);
    let act_acc = if act_items > 0 {
        report.correct as f64 / act_items as f64
    } else {
        0.0
    };
    println!(
        "{label:<38} {:>3} items | cov {:>3.0}% | act {}/{} ({:>3.0}%) | routes {}/{} | fa {} | fr {}",
        report.items,
        report.coverage() * 100.0,
        report.correct,
        act_items,
        act_acc * 100.0,
        report.routes_correct,
        report.route_items,
        report.false_acts,
        report.false_routes,
    );
}

fn eval_matrix(
    datasets: &[String],
    engine_name: &str,
    engine_path: Option<String>,
    min_confidence: f32,
) -> Result<()> {
    let decider = build_decider(engine_name, &engine_path, min_confidence)?;
    let generator = dexter_decision::HeuristicGenerator::default();

    for ds in datasets {
        let text =
            std::fs::read_to_string(ds).with_context(|| format!("reading dataset '{ds}'"))?;
        let items = dexter_eval::load_jsonl(&text).with_context(|| format!("parsing '{ds}'"))?;
        println!("{ds}");
        let groups = dexter_eval::split_by_app(&items);
        for (app, group) in &groups {
            let report = dexter_eval::run_eval(group, &generator, decider.as_ref());
            matrix_row(&format!("  {app}"), &report);
        }
        if groups.len() > 1 {
            let report = dexter_eval::run_eval(&items, &generator, decider.as_ref());
            matrix_row("  (all)", &report);
        }
    }
    Ok(())
}

fn eval_run(
    dataset: &str,
    engine_name: &str,
    engine_path: Option<String>,
    min_confidence: f32,
    out: Option<String>,
) -> Result<()> {
    let text =
        std::fs::read_to_string(dataset).with_context(|| format!("reading dataset '{dataset}'"))?;
    let items =
        dexter_eval::load_jsonl(&text).with_context(|| format!("parsing dataset '{dataset}'"))?;

    let decider = build_decider(engine_name, &engine_path, min_confidence)?;
    let generator = dexter_decision::HeuristicGenerator::default();
    let report = dexter_eval::run_eval(&items, &generator, decider.as_ref());

    if let Some(path) = out {
        let mut buf = String::new();
        for v in &report.verdicts {
            buf.push_str(&serde_json::to_string(&serde_json::json!({
                "item": v.item_id,
                "covered": v.covered,
                "correct": v.correct,
                "decision": v.decision,
                "note": v.note,
            }))?);
            buf.push('\n');
        }
        std::fs::write(&path, buf).with_context(|| format!("writing '{path}'"))?;
    }

    for v in &report.verdicts {
        let mark = match v.correct {
            Some(true) => "ok",
            Some(false) => "MISS",
            None => "err",
        };
        println!(
            "{mark:>4}  {}  (covered={}) {}",
            v.item_id, v.covered, v.note
        );
    }
    let act_items = report.covered.saturating_sub(report.route_items);
    let act_acc = if act_items > 0 {
        report.correct as f64 / act_items as f64
    } else {
        0.0
    };
    println!(
        "\n{} items | coverage {:.0}% | act-accuracy {}/{} ({:.0}%) | routes {}/{} | false_acts {} | false_routes {}",
        report.items,
        report.coverage() * 100.0,
        report.correct,
        act_items,
        act_acc * 100.0,
        report.routes_correct,
        report.route_items,
        report.false_acts,
        report.false_routes,
    );
    Ok(())
}

fn eval_harvest(driver: &dyn ComputerDriver, manifest_path: &str, out: &str) -> Result<()> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("reading manifest '{manifest_path}'"))?;
    let manifest: HarvestManifest =
        toml::from_str(&text).with_context(|| format!("parsing manifest '{manifest_path}'"))?;

    let manifest_dir = std::path::Path::new(manifest_path)
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .canonicalize()
        .with_context(|| format!("resolving manifest dir '{manifest_path}'"))?;
    let mut buf = String::new();
    for page in &manifest.page {
        let url = page.url.clone().or_else(|| {
            page.path
                .as_ref()
                .map(|rel| format!("file://{}", manifest_dir.join(rel).display()))
        });
        if let Some(url) = &url {
            driver
                .act(
                    &Action::Navigate { url: url.clone() },
                    &dexter_driver::ActContext::default(),
                )
                .map_err(|e| anyhow::anyhow!("{}: navigate: {e}", page.id))?;
        }
        if let Some(cmd) = &page.prep {
            let status = std::process::Command::new("sh")
                .env("DEXTER_OVERLAY", "0")
                .arg("-c")
                .arg(cmd)
                .status()
                .with_context(|| format!("{}: prep spawn", page.id))?;
            if !status.success() {
                eprintln!("{}: prep exited {status} — continuing", page.id);
            }
        }
        std::thread::sleep(Duration::from_millis(page.settle_ms.unwrap_or(600)));

        let scope = ObservationScope {
            app: page.app.as_deref().map(AppSelector::parse),
            ..Default::default()
        };
        let obs = driver
            .observe(&scope)
            .map_err(|e| anyhow::anyhow!("{}: observe: {e}", page.id))?;
        if let Some(cmd) = &page.teardown {
            let _ = std::process::Command::new("sh")
                .env("DEXTER_OVERLAY", "0")
                .arg("-c")
                .arg(cmd)
                .status();
        }

        let gold = match &page.gold {
            HarvestGold::Act { target } => {
                let el =
                    dexter_world_model::resolve_element(&obs, &Target::Semantic(target.clone()))
                        .map_err(|e| {
                            anyhow::anyhow!(
                                "{}: gold target did not resolve uniquely: {e}",
                                page.id
                            )
                        })?;
                dexter_eval::Gold::Act {
                    target: target.clone(),
                    element: el.id,
                }
            }
            HarvestGold::Route { route } => dexter_eval::Gold::Route { route: *route },
        };

        let item = dexter_eval::EvalItem {
            id: page.id.clone(),
            goal: page.goal.clone(),
            observation: obs,
            gold,
            source: "harvest".into(),
            meta: serde_json::json!({"url": url, "app": page.app}),
        };
        buf.push_str(&serde_json::to_string(&item)?);
        buf.push('\n');
        eprintln!("harvested {}", page.id);
    }
    std::fs::write(out, buf).with_context(|| format!("writing '{out}'"))?;
    println!("wrote {} items to {out}", manifest.page.len());
    Ok(())
}

/// `eval scenario` — task-level utility metrics over the sim suite.
/// Loads every `*.toml` in `path` (or a single file), runs each spec
/// `reps` times through `run_task`, aggregates journal-derived metrics,
/// and — with `--check` — gates the suite against a committed baseline.
#[allow(clippy::too_many_arguments)]
fn eval_scenario(
    path: &str,
    engine_name: &str,
    engine_path: Option<String>,
    min_confidence: f32,
    reps: u32,
    out: Option<String>,
    history: Option<String>,
    check: Option<String>,
    export: Option<String>,
    journal_out: Option<String>,
    browser_url: Option<String>,
    presence: bool,
) -> Result<()> {
    use dexter_eval::scenario::*;

    // Load specs: a directory of TOML files or one file.
    let root = std::path::Path::new(path);
    let mut files: Vec<std::path::PathBuf> = if root.is_dir() {
        std::fs::read_dir(root)
            .with_context(|| format!("reading '{path}'"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            // Baseline is a config file, not a scenario.
            .filter(|p| p.file_name().is_some_and(|n| n != "baseline.toml"))
            .collect()
    } else {
        vec![root.to_path_buf()]
    };
    files.sort();
    if files.is_empty() {
        anyhow::bail!("no scenario TOML files in '{path}'");
    }

    let decider = build_decider(engine_name, &engine_path, min_confidence)?;
    let generator = dexter_decision::HeuristicGenerator::default();

    if let Some(dir) = &journal_out {
        std::fs::create_dir_all(dir).with_context(|| format!("creating '{dir}'"))?;
    }
    let mut export_rows: Vec<String> = Vec::new();
    let mut export_seen = std::collections::HashSet::new();
    let mut export_skipped = 0usize;

    let mut all: Vec<ScenarioMetrics> = Vec::new();
    let mut last_overlay: Option<std::process::Child> = None;
    for file in &files {
        let text =
            std::fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        let spec: ScenarioSpec =
            toml::from_str(&text).with_context(|| format!("parsing {}", file.display()))?;

        // Browser scenarios need a live endpoint — without one they are
        // skipped (never failed), keeping the suite hermetic.
        let browser = match spec.driver() {
            "browser" => {
                let Some(endpoint) = &browser_url else {
                    println!(
                        "{:<24} skipped — driver=browser needs --browser-url",
                        spec.scenario.id
                    );
                    continue;
                };
                let bspec = spec.browser.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("{}: [browser] section required", file.display())
                })?;
                let url = match (&bspec.page, &bspec.url) {
                    (Some(page), _) => {
                        let abs = std::fs::canonicalize(file.parent().unwrap_or(root).join(page))
                            .with_context(|| format!("resolving page '{page}'"))?;
                        format!("file://{}", abs.display())
                    }
                    (None, Some(u)) => u.clone(),
                    (None, None) => {
                        anyhow::bail!("{}: [browser] needs page = or url =", file.display())
                    }
                };
                Some((endpoint.clone(), url, bspec.settle_ms))
            }
            _ => None,
        };

        // Live macOS scenarios resolve their [live] spec up front — the
        // observe probe runs after the first prep below, since prep is
        // what launches the app. We also remember who was frontmost: a
        // lazy app may need one bounded activation to render its window,
        // and the suite hands focus back when it finishes.
        let macos =
            if spec.driver() == "macos" {
                Some(spec.live.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("{}: [live] section required", file.display())
                })?)
            } else {
                None
            };
        let mut wake_handle = None;

        let mut runs: Vec<ScenarioRun> = Vec::new();
        for rep in 0..reps.max(1) {
            let run = if let Some((endpoint, url, settle_ms)) = &browser {
                // Fresh session per rep — `connect` opens a new
                // window (closed on drop), so state is deterministic
                // and the user's live session is never hijacked.
                let driver =
                    dexter_browser::BrowserDriver::connect(endpoint, browser_label(endpoint))
                        .map_err(|e| anyhow::anyhow!("browser driver at {endpoint}: {e}"))?;
                driver
                    .navigate(url)
                    .map_err(|e| anyhow::anyhow!("navigate {url}: {e}"))?;
                std::thread::sleep(Duration::from_millis(*settle_ms));
                run_scenario_with(&spec, driver, &generator, decider.as_ref(), None)
            } else if let Some(lspec) = macos {
                // Per rep: prep the fixture, settle, run, teardown —
                // teardown always runs so a failed rep leaves no state.
                if let Some(prep) = &lspec.prep {
                    let st = std::process::Command::new("sh")
                        .env("DEXTER_OVERLAY", "0")
                        .arg("-c")
                        .arg(prep)
                        .status()
                        .with_context(|| format!("running prep '{prep}'"))?;
                    if !st.success() {
                        println!("{:<24} skipped — prep failed ({st})", spec.scenario.id);
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(lspec.settle_ms));
                // Probe once after the first prep — a hard observe error
                // (no AX permission, app missing) skips the scenario so
                // CI runners and permission-less terminals never flake.
                // A `open -g` background launch can leave the app with no
                // rendered window — then AX reports a menubar-only tree.
                // Wake once via activation, re-settle, re-probe.
                if rep == 0 {
                    let scope = dexter_core::ObservationScope {
                        app: Some(dexter_core::AppSelector::parse(&lspec.app)),
                        ..Default::default()
                    };
                    let probe = MacOsDriver::new().observe(&scope);
                    let no_window = |o: &dexter_core::Observation| {
                        !o.elements
                            .iter()
                            .any(|e| e.role.as_deref() == Some("window"))
                    };
                    let probe = match probe {
                        Ok(obs) if no_window(&obs) => {
                            // AX only exposes window content while the
                            // app is frontmost — bounded wake: activate
                            // once, re-settle, re-probe. Frontmost is
                            // restored after all reps via the handle.
                            let drv = MacOsDriver::new();
                            if let Ok(h) = drv.wake(&dexter_core::AppSelector::parse(&lspec.app)) {
                                if h.activated {
                                    wake_handle = Some(h);
                                    std::thread::sleep(Duration::from_millis(lspec.settle_ms));
                                }
                            }
                            drv.observe(&scope)
                        }
                        other => other,
                    };
                    match probe {
                        Err(e) => {
                            println!(
                                "{:<24} skipped — macos observe failed: {e}",
                                spec.scenario.id
                            );
                            if let Some(teardown) = &lspec.teardown {
                                let _ = std::process::Command::new("sh")
                                    .env("DEXTER_OVERLAY", "0")
                                    .arg("-c")
                                    .arg(teardown)
                                    .status();
                            }
                            break;
                        }
                        Ok(obs) if no_window(&obs) => {
                            println!(
                                "{:<24} skipped — {} exposes no AX window",
                                spec.scenario.id, lspec.app
                            );
                            if let Some(teardown) = &lspec.teardown {
                                let _ = std::process::Command::new("sh")
                                    .env("DEXTER_OVERLAY", "0")
                                    .arg("-c")
                                    .arg(teardown)
                                    .status();
                            }
                            break;
                        }
                        _ => {}
                    }
                }
                // Presence on live reps: a per-rep journal feeds the
                // overlay — the cursor flies while the scenario works.
                let presence_journal = presence.then(|| {
                    std::env::temp_dir().join(format!(
                        "dexter-{}-{}-rep{}.jsonl",
                        std::process::id(),
                        spec.scenario.id,
                        rep + 1
                    ))
                });
                // One overlay on screen at a time: the previous rep's is
                // retired when the next starts; the last one lingers on
                // its own terminal timer so the outcome stays visible.
                if let Some(mut c) = last_overlay.take() {
                    let _ = c.kill();
                }
                last_overlay = presence_journal
                    .as_deref()
                    .and_then(dexter_engine::presence::spawn_overlay);
                let run = run_scenario_with(
                    &spec,
                    MacOsDriver::new(),
                    &generator,
                    decider.as_ref(),
                    presence_journal.as_deref(),
                );
                if let Some(teardown) = &lspec.teardown {
                    let _ = std::process::Command::new("sh")
                        .env("DEXTER_OVERLAY", "0")
                        .arg("-c")
                        .arg(teardown)
                        .status();
                }
                run
            } else {
                run_scenario(&spec, &generator, decider.as_ref())
            };
            if let Some(dir) = &journal_out {
                let name = if reps > 1 {
                    format!("{}-rep{}.jsonl", spec.scenario.id, rep + 1)
                } else {
                    format!("{}.jsonl", spec.scenario.id)
                };
                let mut buf = String::new();
                for ev in &run.events {
                    buf.push_str(&serde_json::to_string(ev)?);
                    buf.push('\n');
                }
                std::fs::write(std::path::Path::new(dir).join(name), buf)
                    .with_context(|| format!("writing journal to '{dir}'"))?;
            }
            if export.is_some() && run.success {
                let (rows, skipped) = rows_from_events(
                    &run.events,
                    &spec.scenario.id,
                    spec.scenario.app.as_deref().unwrap_or(spec.driver()),
                );
                export_skipped += skipped;
                for row in rows {
                    // Deterministic sim reps emit identical rows — dedup.
                    let line = serde_json::to_string(&row)?;
                    if export_seen.insert(line.clone()) {
                        export_rows.push(line);
                    }
                }
            }
            runs.push(run);
        }
        // Hand focus back to whoever owned it before a wake fired —
        // background-first means borrow the stage, then return it.
        if let Some(h) = wake_handle.take() {
            MacOsDriver::new().restore(&h);
        }
        if runs.is_empty() {
            // Prep failed on rep 0 — nothing was measured.
            continue;
        }
        let m = aggregate(&spec.scenario.id, spec.scenario.optimal_steps, runs);
        println!(
            "{:<24} ok {}/{}  steps {:>4.1} (opt {})  decide p50/p95 {:>3}/{}ms  rec {}  fails {}  appr {}  phys {}  {}",
            m.id,
            m.succeeded,
            m.reps,
            m.mean_steps,
            spec.scenario
                .optimal_steps
                .map(|o| o.to_string())
                .unwrap_or_else(|| "-".into()),
            m.decide_p50_ms,
            m.decide_p95_ms,
            m.recoveries,
            m.verify_fails + m.action_failures,
            m.approvals,
            m.physical_acts,
            m.outcomes
                .iter()
                .map(|(k, n)| format!("{k}×{n}"))
                .collect::<Vec<_>>()
                .join(" "),
        );
        all.push(m);
    }
    let roll = suite_rollup(&all);
    println!(
        "\n{} scenarios | {} reps | success {:.0}% | decide p95 {}ms | physical acts {}",
        roll.scenarios,
        roll.reps,
        roll.success_rate * 100.0,
        roll.decide_p95_ms,
        roll.physical_acts,
    );

    if let Some(p) = &export {
        let mut buf = export_rows.join("\n");
        if !buf.is_empty() {
            buf.push('\n');
        }
        std::fs::write(p, buf).with_context(|| format!("writing '{p}'"))?;
        println!(
            "{} rows exported to {p} ({export_skipped} unlabelable)",
            export_rows.len()
        );
    }
    if let Some(p) = &out {
        let doc = serde_json::json!({
            "engine": engine_name,
            "scenarios": &all,
            "suite": &roll,
        });
        std::fs::write(p, serde_json::to_string_pretty(&doc)?)
            .with_context(|| format!("writing '{p}'"))?;
    }
    if let Some(p) = &history {
        let sha = std::process::Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());
        let rec = serde_json::json!({
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            "git_sha": sha,
            "engine": engine_name,
            "reps": reps,
            "suite": &roll,
            "scenarios": &all,
        });
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)
            .with_context(|| format!("opening history '{p}'"))?;
        writeln!(f, "{}", serde_json::to_string(&rec)?)?;
    }
    if let Some(p) = &check {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading baseline '{p}'"))?;
        let base: Baseline =
            toml::from_str(&text).with_context(|| format!("parsing baseline '{p}'"))?;
        let violations = check_baseline(&all, &base);
        if !violations.is_empty() {
            for v in &violations {
                eprintln!("REGRESSION {}: {}", v.scenario, v.message);
            }
            anyhow::bail!("{} baseline violation(s)", violations.len());
        }
        println!("baseline check: ok");
    }
    Ok(())
}

/// Presence contract: an explicit `--overlay` always wins, an explicit
/// `--no-overlay` always loses, and otherwise the cursor shows whenever
/// a human is watching (interactive terminal). Piped/CI runs stay
/// headless unless asked — every act should be seen, not invisible.
fn presence_wanted(overlay: bool, no_overlay: bool) -> bool {
    use std::io::IsTerminal;
    // DEXTER_OVERLAY=0 silences nested invocations (scenario prep and
    // teardown scripts) so their overlays don't stack over the run's.
    let muted = std::env::var("DEXTER_OVERLAY").is_ok_and(|v| v == "0");
    !no_overlay && (overlay || (!muted && std::io::stderr().is_terminal()))
}

/// Temp journal path for presence runs without an explicit `--events`.
fn overlay_journal_path() -> String {
    presence::overlay_journal_path()
        .to_string_lossy()
        .into_owned()
}

/// Spawn the presence overlay on this run's journal. Best-effort —
/// a missing binary warns, never fails the run.
fn spawn_overlay(events_path: &str) {
    match presence::spawn_overlay(std::path::Path::new(events_path)) {
        Some(_) => eprintln!("overlay: presence on screen — tailing {events_path}"),
        None => eprintln!("overlay: dexter-overlay unavailable — continuing without presence"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_journal_path_is_per_process_and_jsonl() {
        let p = overlay_journal_path();
        assert!(p.contains("dexter-"));
        assert!(p.ends_with(".jsonl"));
    }

    #[test]
    fn presence_flags_resolve() {
        assert!(presence_wanted(true, false));
        assert!(!presence_wanted(true, true));
        assert!(!presence_wanted(false, true));
        // Default depends on whether stderr is a terminal — under the
        // test harness it isn't, so interactive default is off here.
    }
}
