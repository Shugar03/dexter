// `cocoa`/`objc` are deprecated in favor of `objc2`; the `accessibility`
// crate itself is built on them, so the migration happens together.
#![allow(deprecated)]
// objc's `class!`/`msg_send!` macros probe a `cargo-clippy` cfg.
#![allow(unexpected_cfgs)]

//! `MacOsDriver` — the macOS implementation of `ComputerDriver`.
//!
//! Honest capability model: the accessibility tree and window enumeration
//! are read-only; input is added by the action slice. Nothing is simulated.

mod apps;
mod ax;
mod ffi;
mod screenshot;
mod windows;

pub mod permissions;

use accessibility::AXUIElement;
use dexter_core::{Observation, ObservationId, ObservationScope, Window};
use dexter_driver::{ComputerDriver, DriverCapabilities, DriverError};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

#[derive(Default)]
pub struct MacOsDriver {
    next_observation: AtomicU64,
}

impl MacOsDriver {
    pub fn new() -> Self {
        Self {
            next_observation: AtomicU64::new(1),
        }
    }
}

impl ComputerDriver for MacOsDriver {
    fn capabilities(&self) -> DriverCapabilities {
        DriverCapabilities {
            name: "macos",
            element_tree: permissions::accessibility_trusted(),
            screenshots: permissions::screen_capture_allowed(),
            // AX semantic actions are background-safe; coordinate input is not
            // — the action layer reports FOREGROUND_REQUIRED where relevant.
            background_input: false,
        }
    }

    fn windows(&self) -> Result<Vec<Window>, DriverError> {
        windows::list_windows()
    }

    fn observe(&self, scope: &ObservationScope) -> Result<Observation, DriverError> {
        let id = ObservationId(self.next_observation.fetch_add(1, Ordering::SeqCst));
        let mut obs = Observation {
            id,
            timestamp: SystemTime::now(),
            app: scope.app.clone(),
            pid: None,
            windows: windows::list_windows()?,
            elements: Vec::new(),
            elements_truncated: false,
            collection_errors: 0,
            screenshot: None,
            digest: String::new(),
        };

        if let Some(selector) = &scope.app {
            if !permissions::accessibility_trusted() {
                return Err(DriverError::PermissionDenied(
                    "accessibility permission not granted — run `dexter doctor --request` \
                     or enable it in System Settings > Privacy & Security > Accessibility"
                        .into(),
                ));
            }
            let pid = apps::resolve_pid(selector)?;
            obs.pid = Some(pid);
            obs.windows.retain(|w| w.pid == pid);

            let app = AXUIElement::application(pid);
            // Per-call AX timeout so a hung app can't freeze the runtime.
            let _ = app.set_messaging_timeout(1.5);
            let tree = ax::collect(&app, scope.max_depth, scope.max_elements);
            obs.elements_truncated = tree.truncated;
            obs.collection_errors = tree.errors;
            obs.elements = tree.elements;

            if scope.screenshot {
                let path = scope
                    .screenshot_path
                    .clone()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| {
                        std::env::temp_dir().join(format!("dexter-obs-{}.png", id.0))
                    });
                screenshot::capture_app_window(pid, &obs.windows, &path)?;
                obs.screenshot = Some(path.display().to_string());
            }
        }

        obs.digest = dexter_world_model::digest(&obs, 250);
        Ok(obs)
    }
}
