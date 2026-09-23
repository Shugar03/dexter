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
        Some(p) => Policy::load(std::path::Path::new(p))
            .with_context(|| format!("loading policy '{p}'")),
        None => Ok(Policy::embedded()),
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let policy = load_policy(&cli.policy)?;
    let mut engine = Engine::new(MacOsDriver::new(), policy, Duration::from_secs(300));

    match cli.cmd {
        Command::Doctor { request } => doctor(engine.driver(), request),
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
            run_action(
                &mut engine,
                &args,
                Action::Click { target, button },
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
        Command::Run {
            path,
            coords,
            approve_all,
            events,
        } => run_scenario(&mut engine, &path, coords, approve_all, events),
    }
}

/// Parse a `--target` flag into a `Target`. `element:N` takes a fresh
/// observation first — element ids are only meaningful against the
/// observation that produced them, and only inside this process.
fn resolve_target(driver: &MacOsDriver, args: &ActionArgs) -> Result<Target> {
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

fn run_action(engine: &mut Engine<MacOsDriver>, args: &ActionArgs, action: Action) -> Result<()> {
    let app = args.app.as_deref().map(AppSelector::parse);
    let expect = args
        .expect
        .as_deref()
        .map(serde_json::from_str::<ExpectedState>)
        .transpose()
        .context("invalid --expect JSON")?;
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
            println!("{}", serde_json::json!({"status": "denied", "reason": reason}));
        }
        StepStatus::NeedsApproval { fingerprint, reason } => {
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
            println!("{}", serde_json::json!({"status": "error", "error": error.to_string()}));
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
    engine: &mut Engine<MacOsDriver>,
    path: &str,
    coords: bool,
    approve_all: bool,
    events_path: Option<String>,
) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading scenario '{path}'"))?;
    let file: ScenarioFile =
        toml::from_str(&text).with_context(|| format!("parsing scenario '{path}'"))?;
    for fp in &file.grants {
        engine.grant_approval(fp);
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

    if let Some(path) = events_path {
        let mut out = String::new();
        for e in engine.events() {
            out.push_str(&serde_json::to_string(e)?);
            out.push('\n');
        }
        std::fs::write(&path, out).with_context(|| format!("writing events '{path}'"))?;
    }

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

fn doctor(driver: &MacOsDriver, request: bool) -> Result<()> {
    if request {
        permissions::request_accessibility();
        permissions::request_screen_capture();
    }
    let caps = driver.capabilities();
    let ax = permissions::accessibility_trusted();
    let sc = permissions::screen_capture_allowed();
    println!("driver: {}", caps.name);
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

fn windows(driver: &MacOsDriver, app: Option<String>) -> Result<()> {
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
    driver: &MacOsDriver,
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
