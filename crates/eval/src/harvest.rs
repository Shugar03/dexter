//! Harvest labeled observations from a manifest.

use crate::{EvalItem, Gold};
use dexter_core::{Action, AppSelector, ObservationScope, SemanticTarget, Target};
use dexter_decision::Route;
use dexter_driver::{ActContext, ComputerDriver};
use serde::Deserialize;
use std::path::Path;
use std::time::Duration;

/// A manifest containing pages to observe and label.
#[derive(Deserialize)]
pub struct HarvestManifest {
    /// Pages to harvest, in manifest order.
    #[serde(default)]
    pub page: Vec<HarvestPage>,
}

/// One page and its gold label in a harvest manifest.
#[derive(Deserialize)]
pub struct HarvestPage {
    /// Page id used as the resulting eval item id.
    pub id: String,
    /// Browser: URL to navigate. macOS: omit and use `app` instead.
    pub url: Option<String>,
    /// Browser: HTML file relative to the manifest's directory.
    pub path: Option<String>,
    /// macOS: scope the observation to this app.
    pub app: Option<String>,
    /// Shell command to put the app in the intended state before
    /// observing (e.g. osascript making a new document). Harvest
    /// tooling only — never part of the agent path.
    #[serde(default)]
    pub prep: Option<String>,
    /// Shell command to restore state after observing (e.g. close the
    /// scratch document without saving).
    #[serde(default)]
    pub teardown: Option<String>,
    /// Goal associated with this page.
    pub goal: String,
    /// Question labels expected for this page.
    #[serde(default)]
    pub expect_questions: Option<Vec<String>>,
    /// Previous action and its observed world change.
    #[serde(default)]
    pub last_act: Option<dexter_decision::LastAct>,
    /// Gold label for this page.
    pub gold: HarvestGold,
    /// Extra settle time after navigate/scope before observing (ms).
    #[serde(default)]
    pub settle_ms: Option<u64>,
}

/// The gold label declared by a harvest manifest.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HarvestGold {
    /// The correct move is acting on the element matching `target`.
    Act { target: SemanticTarget },
    /// The correct move is a route (wait/abstain/escalate/...).
    Route { route: Route },
}

/// A harvest error annotated with the failing page or manifest path.
#[derive(Debug, thiserror::Error)]
#[error("{page}: {stage}: {message}")]
pub struct HarvestError {
    /// Page id, or the manifest path for manifest-level errors.
    pub page: String,
    /// Operation stage that failed.
    pub stage: &'static str,
    /// Underlying failure detail.
    pub message: String,
}

/// Observe every manifest page and return its labeled eval item.
pub fn harvest(
    driver: &dyn ComputerDriver,
    manifest: &HarvestManifest,
    manifest_dir: &Path,
) -> Result<Vec<EvalItem>, HarvestError> {
    let mut items = Vec::with_capacity(manifest.page.len());
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
                    &ActContext::default(),
                )
                .map_err(|error| HarvestError {
                    page: page.id.clone(),
                    stage: "navigate",
                    message: error.to_string(),
                })?;
        }
        if let Some(cmd) = &page.prep {
            let status = std::process::Command::new("sh")
                .env("DEXTER_OVERLAY", "0")
                .arg("-c")
                .arg(cmd)
                .status()
                .map_err(|error| HarvestError {
                    page: page.id.clone(),
                    stage: "prep spawn",
                    message: error.to_string(),
                })?;
            if !status.success() {
                eprintln!("{}: prep exited {status} — continuing", page.id);
            }
        }
        std::thread::sleep(Duration::from_millis(page.settle_ms.unwrap_or(600)));

        let scope = ObservationScope {
            app: page.app.as_deref().map(AppSelector::parse),
            ..Default::default()
        };
        let obs = driver.observe(&scope).map_err(|error| HarvestError {
            page: page.id.clone(),
            stage: "observe",
            message: error.to_string(),
        })?;
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
                        .map_err(|error| HarvestError {
                            page: page.id.clone(),
                            stage: "gold target did not resolve uniquely",
                            message: error.to_string(),
                        })?;
                Gold::Act {
                    target: target.clone(),
                    element: el.id,
                }
            }
            HarvestGold::Route { route } => Gold::Route { route: *route },
        };

        items.push(EvalItem {
            id: page.id.clone(),
            goal: page.goal.clone(),
            observation: obs,
            gold,
            last_act: page.last_act.clone(),
            expect_questions: page.expect_questions.clone(),
            source: "harvest".into(),
            meta: serde_json::json!({"url": url, "app": page.app}),
        });
        eprintln!("harvested {}", page.id);
    }
    Ok(items)
}

/// Read and parse a manifest, then harvest its pages.
pub fn harvest_file(
    driver: &dyn ComputerDriver,
    manifest_path: &Path,
) -> Result<Vec<EvalItem>, HarvestError> {
    let manifest_label = manifest_path.display().to_string();
    let text = std::fs::read_to_string(manifest_path).map_err(|error| HarvestError {
        page: manifest_label.clone(),
        stage: "read manifest",
        message: error.to_string(),
    })?;
    let manifest: HarvestManifest = toml::from_str(&text).map_err(|error| HarvestError {
        page: manifest_label.clone(),
        stage: "parse manifest",
        message: error.to_string(),
    })?;
    let manifest_dir = manifest_path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|error| HarvestError {
            page: manifest_label,
            stage: "resolve manifest dir",
            message: error.to_string(),
        })?;
    harvest(driver, &manifest, &manifest_dir)
}
