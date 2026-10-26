//! Linux-capable twin of `dexter replay`.
//!
//! Usage:
//! `cargo run -p dexter-eval --example replay -- <recording.json> --driver linux|browser|sim [--browser-url URL] [--policy FILE] [--grant FP]... [--honor-recorded-grants] [--coords]`

use dexter_driver::ComputerDriver;
use dexter_engine::RunConfig;
use dexter_eval::{
    recording::{replay_with, Recording, ReplayOptions, StepResult},
    scenario::{self, ScenarioSpec},
};
use dexter_policy::{fingerprint, ActionContext, Policy};
use serde_json::json;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn run_config(approve_all: bool) -> RunConfig {
    RunConfig {
        app: None,
        max_attempts: 1,
        verify_delay: Duration::from_millis(5),
        post_act_settle: Duration::ZERO,
        allow_coordinates: false,
        approve_all,
        observe_max_elements: 4_000,
    }
}

fn sim_spec_path(recording: &Path, scenario: &str) -> Option<PathBuf> {
    let name = format!("{scenario}.toml");
    let parent = recording.parent().unwrap_or_else(|| Path::new("."));
    [
        parent.join("../scenarios").join(&name),
        PathBuf::from("datasets/scenarios").join(name),
    ]
    .into_iter()
    .find(|path| path.is_file())
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let recording_path = args.next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: replay <recording.json> --driver linux|browser|sim [options]",
        )
    })?;
    let mut driver_name = None;
    let mut browser_url = "http://localhost:9515".to_string();
    let mut policy_path = None;
    let mut grants = Vec::new();
    let mut honor_recorded_grants = false;
    let mut coords = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--driver" => {
                driver_name = Some(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--driver requires a value")
                })?);
            }
            "--browser-url" => {
                browser_url = args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--browser-url requires a URL")
                })?;
            }
            "--policy" => {
                policy_path = Some(args.next().ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "--policy requires a file")
                })?);
            }
            "--grant" => grants.push(args.next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "--grant requires a fingerprint",
                )
            })?),
            "--honor-recorded-grants" => honor_recorded_grants = true,
            "--coords" => coords = true,
            other if other.starts_with('-') => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown argument '{other}'"),
                )
                .into());
            }
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unexpected argument '{other}'"),
                )
                .into());
            }
        }
    }

    let driver_name = driver_name.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "--driver is required (linux|browser|sim)",
        )
    })?;
    let recording: Recording = serde_json::from_str(&std::fs::read_to_string(&recording_path)?)?;
    let mut policy = match policy_path {
        Some(path) => Policy::load(Path::new(&path))?,
        None => Policy::embedded(),
    };
    if coords {
        policy.permit_physical();
    }

    let mut spec_used = None;
    let mut config = run_config(false);
    let cli_grants = grants;
    let mut applied_grants = cli_grants.clone();
    let driver: Box<dyn ComputerDriver> = match driver_name.as_str() {
        "sim" => {
            let path = sim_spec_path(Path::new(&recording_path), &recording.scenario).ok_or_else(
                || {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("no scenario TOML found for '{}'", recording.scenario),
                    )
                },
            )?;
            let spec: ScenarioSpec = toml::from_str(&std::fs::read_to_string(&path)?)?;
            applied_grants = spec.task.grants.clone();
            applied_grants.extend(cli_grants.iter().cloned());
            config = run_config(true);
            spec_used = Some(path.display().to_string());
            Box::new(scenario::build_driver(&spec))
        }
        "browser" => Box::new(dexter_browser::BrowserDriver::connect(
            &browser_url,
            "chrome",
        )?),
        "linux" => {
            #[cfg(target_os = "linux")]
            {
                Box::new(dexter_linux::LinuxDriver::new())
            }
            #[cfg(not(target_os = "linux"))]
            {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "the Linux driver is only available on Linux",
                )
                .into());
            }
        }
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown driver '{other}' — available: linux, browser, sim"),
            )
            .into());
        }
    };

    let options = ReplayOptions {
        grants: applied_grants.clone(),
        honor_recorded_grants,
        allow_coordinates: coords,
    };
    let report = replay_with(&recording, driver, policy, config.clone(), &options);
    let steps: Vec<_> = recording
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| {
            let mut row = json!({
                "index": index,
                "recorded": step.result,
                "replayed": report.replayed.get(index),
                "granted": step.granted,
                "fingerprint": &step.fingerprint,
            });
            if report.replayed.get(index) == Some(&StepResult::NeedsApproval) {
                let approval_fingerprint = fingerprint(
                    &step.action,
                    &ActionContext {
                        app: step.app.clone().or(config.app.clone()),
                        target_hint: None,
                    },
                );
                row["approval_fingerprint"] = json!(approval_fingerprint);
                row["hint"] = json!("re-run with --grant '<fingerprint>' to approve this step");
            }
            row
        })
        .collect();
    let faithful = report.faithful(&recording);
    println!(
        "{}",
        json!({
            "scenario": recording.scenario,
            "goal": recording.goal,
            "driver": driver_name,
            "spec": spec_used,
            "grants": applied_grants,
            "recorded_outcome": recording.outcome,
            "recorded_driver": recording.driver,
            "steps": steps,
            "diverged_at": report.diverged_at,
            "divergence": report.divergence,
            "done_when": report.done_when,
            "faithful": faithful,
        })
    );
    if faithful {
        Ok(())
    } else {
        Err(io::Error::other("replay diverged").into())
    }
}
