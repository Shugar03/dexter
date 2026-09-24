//! Presence overlay spawning — shared by the CLI and MCP surfaces.
//!
//! `dexter-overlay` tails an engine journal sink and renders the agent's
//! cursor over each action's target. Presence is best-effort: a missing
//! binary or a failed spawn never fails the run.

use std::path::{Path, PathBuf};

/// Temp journal path for presence runs without an explicit `--events`.
pub fn overlay_journal_path() -> PathBuf {
    std::env::temp_dir().join(format!("dexter-{}.jsonl", std::process::id()))
}

/// `dexter-overlay` lives next to this binary in both layouts that
/// matter (target/debug siblings, brew bin) — check there before PATH.
fn resolve_overlay_bin() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name("dexter-overlay");
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    std::env::var_os("PATH").and_then(|p| {
        std::env::split_paths(&p)
            .map(|d| d.join("dexter-overlay"))
            .find(|b| b.is_file())
    })
}

/// Spawn the presence overlay on a journal path. Detached and quiet —
/// the overlay exits itself shortly after a terminal event. Returns
/// the child so a caller driving several runs can kill leftovers.
pub fn spawn_overlay(events_path: &Path) -> Option<std::process::Child> {
    let bin = resolve_overlay_bin()?;
    std::process::Command::new(bin)
        .arg("--events")
        .arg(events_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()
}
