//! `dexter` CLI — thin shell over the runtime. Every command goes through
//! the same driver path; there are no privileged shortcuts.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dexter_core::{AppSelector, ObservationScope};
use dexter_driver::ComputerDriver;
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
