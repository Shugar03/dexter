//! `dexter` CLI — thin shell over the runtime. Every command goes through
//! the same driver path; there are no privileged shortcuts.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dexter_core::{
    Action, AppSelector, ElementId, MouseButton, ObservationScope, ScrollDelta, SemanticTarget,
    Target,
};
use dexter_driver::{ActContext, ComputerDriver};
use dexter_macos::{permissions, MacOsDriver};
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "dexter",
    version,
    about = "Dexter — agent computer runtime (macOS)",
    long_about = None
)]
struct Cli {
    /// Emit machine-readable JSON (default for most commands).
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
    /// Click an element: semantic target (AXPress), element id from a fresh
    /// observation, `focused`, or `point:x,y` (requires --coords).
    Click {
        #[arg(long)]
        app: Option<String>,
        /// Target: `{"role":"button","name":"Save"}` | `element:N` |
        /// `focused` | `point:x,y`.
        #[arg(long)]
        target: String,
        /// Mouse button: left (default), right, middle.
        #[arg(long, default_value = "left")]
        button: String,
        /// Permit coordinate-level input (moves the real cursor).
        #[arg(long)]
        coords: bool,
    },
    /// Type text into an element (AXValue first; keyboard fallback needs
    /// --coords and the app being frontmost).
    Type {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        text: String,
        #[arg(long)]
        target: Option<String>,
        #[arg(long)]
        coords: bool,
    },
    /// Post a key chord like "cmd+s" (requires --coords; goes to the
    /// frontmost app).
    Key {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        chord: String,
        #[arg(long)]
        coords: bool,
    },
    /// Scroll: a target scrolls it into view via AX; without a target it
    /// scrolls at the pointer (requires --coords).
    Scroll {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        target: Option<String>,
        #[arg(long, default_value = "0")]
        dx: f64,
        #[arg(long, default_value = "0")]
        dy: f64,
        #[arg(long)]
        coords: bool,
    },
    /// Focus an element (AXFocused).
    Focus {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        target: String,
    },
    /// Set an element's value directly (AXValue).
    SetValue {
        #[arg(long)]
        app: Option<String>,
        #[arg(long)]
        target: String,
        #[arg(long)]
        value: String,
    },
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

fn run() -> Result<()> {
    let cli = Cli::parse();
    let driver = MacOsDriver::new();

    match cli.cmd {
        Command::Doctor { request } => doctor(&driver, request),
        Command::Windows { app } => windows(&driver, app),
        Command::Observe {
            app,
            max_depth,
            max_elements,
            digest,
            screenshot,
        } => observe(&driver, app, max_depth, max_elements, digest, screenshot),
        Command::Click {
            app,
            target,
            button,
            coords,
        } => {
            let button = match button.as_str() {
                "left" => MouseButton::Left,
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                other => anyhow::bail!("unknown button '{other}' (left|right|middle)"),
            };
            let target = resolve_target(&driver, &target, &app)?;
            act(&driver, &Action::Click { target, button }, &app, coords)
        }
        Command::Type {
            app,
            text,
            target,
            coords,
        } => {
            let target = target
                .map(|t| resolve_target(&driver, &t, &app))
                .transpose()?;
            act(&driver, &Action::TypeText { text, target }, &app, coords)
        }
        Command::Key { app, chord, coords } => {
            let chord = dexter_core::KeyChord::parse(&chord)?;
            act(&driver, &Action::Key { chord }, &app, coords)
        }
        Command::Scroll {
            app,
            target,
            dx,
            dy,
            coords,
        } => {
            let target = target
                .map(|t| resolve_target(&driver, &t, &app))
                .transpose()?;
            act(
                &driver,
                &Action::Scroll {
                    delta: ScrollDelta { dx, dy },
                    target,
                },
                &app,
                coords,
            )
        }
        Command::Focus { app, target } => {
            let target = resolve_target(&driver, &target, &app)?;
            act(&driver, &Action::Focus { target }, &app, false)
        }
        Command::SetValue { app, target, value } => {
            let target = resolve_target(&driver, &target, &app)?;
            act(&driver, &Action::SetValue { target, value }, &app, false)
        }
    }
}

/// Parse a `--target` flag into a `Target`. `element:N` takes a fresh
/// observation first — element ids are only meaningful against the
/// observation that produced them, and only inside this process.
fn resolve_target(driver: &MacOsDriver, raw: &str, app: &Option<String>) -> Result<Target> {
    if let Some(rest) = raw.strip_prefix("element:") {
        let n: u64 = rest
            .parse()
            .with_context(|| format!("invalid element id '{rest}'"))?;
        let scope = ObservationScope {
            app: app.as_deref().map(AppSelector::parse),
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

fn act(driver: &MacOsDriver, action: &Action, app: &Option<String>, coords: bool) -> Result<()> {
    let ctx = ActContext {
        app: app.as_deref().map(AppSelector::parse),
        allow_coordinates: coords,
    };
    let result = driver.act(action, &ctx).context("act failed")?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result.status.ok() {
        Ok(())
    } else {
        anyhow::bail!("action returned {:?}", result.status)
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
        println!("{}", serde_json::to_string_pretty(&obs)?);
    }
    Ok(())
}
