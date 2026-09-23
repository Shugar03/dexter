//! `dexter` CLI — thin shell over the runtime. Every action goes through
//! `Engine::run_step` — the same policy/act/verify path MCP and SDKs use.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dexter_core::{
    Action, AppSelector, ElementId, ExpectedState, MouseButton, ObservationScope, ScrollDelta,
    SemanticTarget, Target,
};
use dexter_driver::ComputerDriver;
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_macos::{permissions, MacOsDriver};
use dexter_policy::Policy;
use serde::Deserialize;
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
        /// Print the text digest (decision-engine input) instead of JSON.
        #[arg(long)]
        digest: bool,
        /// Capture a screenshot to this path.
        #[arg(long)]
        screenshot: Option<String>,
    },
    /// Click an element (AXPress; `point:x,y` needs --coords).
    Click {
        #[command(flatten)]
        args: ActionArgs,
        /// Mouse button: left (default), right, middle.
        #[arg(long, default_value = "left")]
        button: String,
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
    /// Permit coordinate-level input.
    #[arg(long)]
    coords: bool,
    /// Approve every policy-required action (you approve the goal).
    #[arg(long)]
    approve_all: bool,
    /// Write the event journal (JSONL) to this path.
    #[arg(long)]
    events: Option<String>,
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

/// Build the selected driver. `browser` spawns `safaridriver` unless
/// `--browser-url` points at an already-running endpoint.
fn build_driver(cli: &Cli) -> Result<Box<dyn ComputerDriver>> {
    match cli.driver.as_str() {
        "macos" => Ok(Box::new(MacOsDriver::new())),
        "browser" => match &cli.browser_url {
            Some(url) => {
                let label = if url.contains("4444") {
                    "firefox"
                } else if url.contains("9515") {
                    "chrome"
                } else {
                    "browser"
                };
                Ok(Box::new(
                    dexter_browser::BrowserDriver::connect_attach(url, label)
                        .map_err(|e| anyhow::anyhow!("browser driver at {url}: {e}"))?,
                ))
            }
            None => Ok(Box::new(
                dexter_browser::BrowserDriver::safari()
                    .map_err(|e| anyhow::anyhow!("safaridriver: {e}"))?,
            )),
        },
        other => anyhow::bail!("unknown driver '{other}' — available: macos, browser"),
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let policy = load_policy(&cli.policy)?;
    let is_browser = cli.driver == "browser";
    let mut engine = Engine::new(build_driver(&cli)?, policy, Duration::from_secs(300));

    match cli.cmd {
        Command::Doctor { request } => doctor(engine.driver(), request, is_browser),
        Command::Navigate { url, args } => run_action(&mut engine, &args, Action::Navigate { url }),
        Command::Windows { app } => windows(engine.driver(), app),
        Command::Observe {
            app,
            max_depth,
            max_elements,
            digest,
            screenshot,
        } => observe(
            engine.driver(),
            app,
            max_depth,
            max_elements,
            digest,
            screenshot,
        ),
        Command::Click { args, button } => {
            let button = match button.as_str() {
                "left" => MouseButton::Left,
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                other => anyhow::bail!("unknown button '{other}' (left|right|middle)"),
            };
            let target = resolve_target(engine.driver(), &args)?;
            run_action(&mut engine, &args, Action::Click { target, button })
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
        Command::Run {
            path,
            coords,
            approve_all,
            events,
        } => run_scenario(&mut engine, &path, coords, approve_all, events),
        Command::Task { goal, args } => run_task(&mut engine, &goal, args),
        Command::Eval { cmd } => match cmd {
            EvalCommand::Run {
                dataset,
                engine: eng,
                engine_path,
                min_confidence,
                out,
            } => eval_run(&dataset, &eng, engine_path, min_confidence, out),
            EvalCommand::Harvest { manifest, out } => {
                eval_harvest(engine.driver(), &manifest, &out)
            }
        },
        Command::Mcp {
            engine: ref eng,
            ref engine_path,
            min_confidence,
        } => run_mcp(&cli, eng, engine_path.clone(), min_confidence),
    }
}

fn run_mcp(
    cli: &Cli,
    engine_name: &str,
    engine_path: Option<String>,
    min_confidence: f32,
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
        .block_on(dexter_mcp::serve_stdio(policy, driver, decider))
}

/// Parse a `--target` flag into a `Target`. `element:N` takes a fresh
/// observation first — element ids are only meaningful against the
/// observation that produced them, and only inside this process.
fn resolve_target(driver: &dyn ComputerDriver, args: &ActionArgs) -> Result<Target> {
    let raw = args
        .target
        .as_deref()
        .context("this command requires --target")?;
    if let Some(rest) = raw.strip_prefix("element:") {
        let n: u64 = rest
            .parse()
            .with_context(|| format!("invalid element id '{rest}'"))?;
        let scope = ObservationScope {
            app: args.app.as_deref().map(AppSelector::parse),
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
        "invalid --target '{raw}' — use a semantic JSON object, `element:N`, `focused` or `point:x,y`"
    )
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
        observe_max_elements: 4_000,
    };
    let step = Step {
        note: None,
        action,
        expect,
        max_attempts: Some(args.attempts),
        app: app.clone(),
    };
    let status = engine.run_step(&step, &cfg);
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
    if let Some(path) = &events_path {
        // Live stream: a presence overlay tails this file mid-run.
        engine
            .set_journal_sink(std::path::Path::new(path))
            .with_context(|| format!("opening events sink '{path}'"))?;
    }
    let cfg = RunConfig {
        app: file.app.as_deref().map(AppSelector::parse),
        max_attempts: file.max_attempts.unwrap_or(3),
        verify_delay: Duration::from_millis(file.verify_delay_ms.unwrap_or(250)),
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
    let decider: Box<dyn dexter_decision::DecisionEngine> = match args.engine.as_str() {
        "rule-based" => Box::new(dexter_decision::RuleBased::default()),
        "laya" => {
            let cmd = args
                .engine_path
                .clone()
                .or_else(|| std::env::var("DEXTER_LAYA_WORKER").ok())
                .unwrap_or_else(|| "python3 workers/laya/worker.py --provider dev".to_string());
            let engine = dexter_laya::LayaEngine::spawn(&cmd, Duration::from_secs(30))
                .with_context(|| format!("spawning laya worker '{cmd}'"))?
                .with_min_confidence(args.min_confidence);
            Box::new(engine)
        }
        other => anyhow::bail!("unknown decision engine '{other}' — available: rule-based, laya"),
    };
    let generator = dexter_decision::HeuristicGenerator::default();
    if args.coords {
        engine.permit_physical();
    }
    if let Some(path) = &args.events {
        engine
            .set_journal_sink(std::path::Path::new(path))
            .with_context(|| format!("opening events sink '{path}'"))?;
    }
    let outcome = engine.run_task(
        goal,
        &generator,
        decider.as_ref(),
        &dexter_engine::TaskConfig {
            run: RunConfig {
                app: args.app.as_deref().map(AppSelector::parse),
                max_attempts: 1,
                verify_delay: Duration::from_millis(250),
                allow_coordinates: args.coords,
                approve_all: args.approve_all,
                observe_max_elements: 4_000,
            },
            max_steps: args.max_steps,
            done_when,
        },
    );

    use dexter_engine::TaskOutcome;
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
    }
}

fn doctor(driver: &dyn ComputerDriver, request: bool, is_browser: bool) -> Result<()> {
    let caps = driver.capabilities();
    println!("driver: {}", caps.name);
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

fn observe(
    driver: &dyn ComputerDriver,
    app: Option<String>,
    max_depth: Option<u32>,
    max_elements: Option<usize>,
    digest: bool,
    screenshot: Option<String>,
) -> Result<()> {
    let mut scope = ObservationScope::default();
    if let Some(a) = app {
        scope.app = Some(AppSelector::parse(&a));
    }
    if let Some(d) = max_depth {
        scope.max_depth = d;
    }
    if let Some(m) = max_elements {
        scope.max_elements = m;
    }
    if let Some(path) = screenshot {
        scope.screenshot = true;
        scope.screenshot_path = Some(path);
    }

    let obs = driver.observe(&scope).context("observe failed")?;
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

    let decider: Box<dyn dexter_decision::DecisionEngine> = match engine_name {
        "rule-based" => Box::new(dexter_decision::RuleBased::default()),
        "laya" => {
            let cmd = engine_path
                .or_else(|| std::env::var("DEXTER_LAYA_WORKER").ok())
                .unwrap_or_else(|| "python3 workers/laya/worker.py --provider dev".to_string());
            Box::new(
                dexter_laya::LayaEngine::spawn(&cmd, Duration::from_secs(30))
                    .with_context(|| format!("spawning laya worker '{cmd}'"))?
                    .with_min_confidence(min_confidence),
            )
        }
        other => anyhow::bail!("unknown engine '{other}' — rule-based, laya"),
    };
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
            let _ = std::process::Command::new("sh").arg("-c").arg(cmd).status();
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
