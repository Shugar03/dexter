//! macOS permission state. Dexter is fail-closed: it reports what it can
//! prove, never assumes access. See `docs/sdd/permissions.md`.

use crate::ffi;
use core_foundation::base::TCFType;
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::CFString;

/// A TCC permission the driver needs, plus the metadata `doctor`
/// renders for it. `ALL` is both the check order and the display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// AX trees of other apps and synthetic input.
    Accessibility,
    /// Window titles and pixel capture.
    ScreenRecording,
}

impl Permission {
    pub const ALL: [Permission; 2] = [Self::Accessibility, Self::ScreenRecording];

    /// Short display name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Accessibility => "accessibility",
            Self::ScreenRecording => "screen recording",
        }
    }

    /// What the grant unlocks for the driver.
    pub fn enables(self) -> &'static str {
        match self {
            Self::Accessibility => "AX element trees and synthetic input",
            Self::ScreenRecording => "window titles and screenshots",
        }
    }

    /// Exact System Settings path for a manual grant.
    pub fn remediation(self) -> &'static str {
        match self {
            Self::Accessibility => {
                "System Settings > Privacy & Security > Accessibility — \
                 enable this terminal/binary (add it if absent). Or run \
                 `dexter doctor --request`."
            }
            Self::ScreenRecording => {
                "System Settings > Privacy & Security > Screen Recording — \
                 enable this terminal/binary. Or run `dexter doctor --request`."
            }
        }
    }
}

/// The OS seam — reads TCC state and triggers its prompts. `check` and
/// `request_missing` go through this trait so tests can fake the OS.
pub trait Probe {
    /// Whether `p` is granted right now.
    fn granted(&self, p: Permission) -> bool;
    /// Show the system prompt for `p`; returns the post-prompt state —
    /// the user may deny, so false here is honest, not an error.
    fn request(&self, p: Permission) -> bool;
}

/// The real TCC-backed probe.
pub struct System;

impl Probe for System {
    fn granted(&self, p: Permission) -> bool {
        match p {
            Permission::Accessibility => accessibility_trusted(),
            Permission::ScreenRecording => screen_capture_allowed(),
        }
    }

    fn request(&self, p: Permission) -> bool {
        match p {
            Permission::Accessibility => request_accessibility(),
            Permission::ScreenRecording => request_screen_capture(),
        }
    }
}

/// One row of the doctor permission report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    pub permission: Permission,
    pub granted: bool,
}

/// Snapshot of every required permission, in `Permission::ALL` order.
pub fn check(probe: &impl Probe) -> Vec<Status> {
    Permission::ALL
        .into_iter()
        .map(|permission| Status {
            permission,
            granted: probe.granted(permission),
        })
        .collect()
}

/// Prompt each missing permission — a granted one is never poked — then
/// return fresh statuses (the prompt's post-state).
pub fn request_missing(probe: &impl Probe, statuses: &[Status]) -> Vec<Status> {
    statuses
        .iter()
        .map(|s| Status {
            permission: s.permission,
            granted: s.granted || probe.request(s.permission),
        })
        .collect()
}

/// Whether this process is trusted for Accessibility (required to read AX
/// trees of other apps and to send input).
pub fn accessibility_trusted() -> bool {
    unsafe { ffi::AXIsProcessTrusted() != 0 }
}

/// Ask macOS to show the Accessibility grant prompt, then report trust.
/// The prompt is a system dialog; the user may deny it.
pub fn request_accessibility() -> bool {
    let options = unsafe {
        let key = CFString::wrap_under_get_rule(ffi::kAXTrustedCheckOptionPrompt);
        CFDictionary::from_CFType_pairs(&[(key.as_CFType(), CFBoolean::true_value().as_CFType())])
    };
    unsafe { ffi::AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef()) != 0 }
}

/// Whether screen capture is allowed (window titles, pixel capture).
pub fn screen_capture_allowed() -> bool {
    unsafe { ffi::CGPreflightScreenCaptureAccess() }
}

/// Ask macOS to show the Screen Recording prompt. Returns current state.
pub fn request_screen_capture() -> bool {
    unsafe { ffi::CGRequestScreenCaptureAccess() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Scripted TCC: `request` records the ask and applies the scripted
    /// post-prompt state, like the real dialog mutating a grant.
    struct FakeProbe {
        state: Mutex<HashMap<Permission, bool>>,
        after_prompt: HashMap<Permission, bool>,
        requested: Mutex<Vec<Permission>>,
    }

    impl FakeProbe {
        fn new(initial: &[(Permission, bool)], after_prompt: &[(Permission, bool)]) -> Self {
            Self {
                state: Mutex::new(initial.iter().copied().collect()),
                after_prompt: after_prompt.iter().copied().collect(),
                requested: Mutex::new(Vec::new()),
            }
        }

        fn requests(&self) -> Vec<Permission> {
            self.requested.lock().unwrap().clone()
        }
    }

    impl Probe for FakeProbe {
        fn granted(&self, p: Permission) -> bool {
            *self.state.lock().unwrap().get(&p).unwrap_or(&false)
        }

        fn request(&self, p: Permission) -> bool {
            self.requested.lock().unwrap().push(p);
            let post = *self.after_prompt.get(&p).unwrap_or(&false);
            self.state.lock().unwrap().insert(p, post);
            post
        }
    }

    #[test]
    fn check_reports_every_permission_in_order() {
        let probe = FakeProbe::new(
            &[
                (Permission::Accessibility, true),
                (Permission::ScreenRecording, false),
            ],
            &[],
        );
        assert_eq!(
            check(&probe),
            vec![
                Status {
                    permission: Permission::Accessibility,
                    granted: true
                },
                Status {
                    permission: Permission::ScreenRecording,
                    granted: false
                },
            ]
        );
    }

    #[test]
    fn request_missing_prompts_only_missing() {
        let probe = FakeProbe::new(
            &[
                (Permission::Accessibility, true),
                (Permission::ScreenRecording, false),
            ],
            &[(Permission::ScreenRecording, true)],
        );
        let statuses = request_missing(&probe, &check(&probe));
        assert_eq!(probe.requests(), vec![Permission::ScreenRecording]);
        assert!(statuses.iter().all(|s| s.granted));
    }

    #[test]
    fn request_missing_reports_denial_honestly() {
        let probe = FakeProbe::new(
            &[
                (Permission::Accessibility, false),
                (Permission::ScreenRecording, true),
            ],
            &[],
        );
        let statuses = request_missing(&probe, &check(&probe));
        assert_eq!(probe.requests(), vec![Permission::Accessibility]);
        assert_eq!(
            statuses
                .iter()
                .find(|s| s.permission == Permission::Accessibility)
                .map(|s| s.granted),
            Some(false)
        );
    }

    #[test]
    fn every_permission_carries_guidance() {
        for p in Permission::ALL {
            assert!(!p.name().is_empty());
            assert!(!p.enables().is_empty());
            assert!(p.remediation().contains("System Settings"));
        }
    }
}
