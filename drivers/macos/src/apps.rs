//! Resolving an `AppSelector` to a real pid via NSRunningApplication.

use cocoa::base::{id, nil};
use cocoa::foundation::{NSAutoreleasePool, NSString};
use dexter_core::AppSelector;
use dexter_driver::DriverError;
use objc::{class, msg_send, sel, sel_impl};

pub fn resolve_pid(selector: &AppSelector) -> Result<i32, DriverError> {
    match selector {
        AppSelector::Pid(pid) => Ok(*pid),
        AppSelector::BundleId(bundle) => pid_for_bundle(bundle),
        AppSelector::Name(name) => pid_for_name(name),
    }
}

fn pid_for_bundle(bundle: &str) -> Result<i32, DriverError> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let apps: id = msg_send![
            class!(NSRunningApplication),
            runningApplicationsWithBundleIdentifier: crate::v2::ns_str(bundle)
        ];
        let count: usize = msg_send![apps, count];
        let pid = if count > 0 {
            let app: id = msg_send![apps, objectAtIndex: 0usize];
            let pid: i32 = msg_send![app, processIdentifier];
            pid
        } else {
            -1
        };
        pool.drain();
        if pid > 0 {
            Ok(pid)
        } else {
            Err(DriverError::AppNotFound(format!(
                "no running application with bundle id '{bundle}'"
            )))
        }
    }
}

/// Raise `pid`'s windows and make it key — the one activation a lazy
/// app needs so its AX windows exist. Native NSRunningApplication call;
/// no Apple Events, so no Automation grant is required.
pub fn activate_pid(pid: i32) -> bool {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let app: id = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        let ok = if app == nil {
            false
        } else {
            // AllWindows | IgnoringOtherApps — bring every window forward.
            let activated: bool = msg_send![app, activateWithOptions: 3usize];
            activated
        };
        pool.drain();
        ok
    }
}

/// Pid of the frontmost application, if the workspace reports one.
pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let app: id = msg_send![workspace, frontmostApplication];
        let pid = if app == nil {
            None
        } else {
            Some(msg_send![app, processIdentifier])
        };
        pool.drain();
        pid
    }
}

/// Launch an app through `/usr/bin/open` — argument array, never a
/// shell. `activate: false` passes `-g` (open in background). Returns
/// once `open` exits; the app's windows appear asynchronously — the
/// caller verifies through observation.
pub fn launch(selector: &AppSelector, activate: bool) -> Result<(), DriverError> {
    let mut args: Vec<String> = Vec::new();
    if !activate {
        args.push("-g".into());
    }
    match selector {
        AppSelector::Name(n) => {
            args.push("-a".into());
            args.push(n.clone());
        }
        AppSelector::BundleId(b) => {
            args.push("-b".into());
            args.push(b.clone());
        }
        AppSelector::Pid(_) => {
            return Err(DriverError::Unsupported(
                "cannot launch an app by pid — it must already be running".into(),
            ));
        }
    }
    let status = std::process::Command::new("/usr/bin/open")
        .args(&args)
        .status()
        .map_err(|e| DriverError::Platform(format!("spawn /usr/bin/open: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(DriverError::AppNotFound(format!(
            "/usr/bin/open {:?} exited {status}",
            args
        )))
    }
}

/// Ask the app to terminate normally (NSRunningApplication::terminate —
/// the same path as ⌘Q; no force-quit).
pub fn terminate(selector: &AppSelector) -> Result<(), DriverError> {
    // resolve_pid gives AppNotFound/Ambiguous for free on names.
    let pid = resolve_pid(selector)?;
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let app: id = msg_send![
            class!(NSRunningApplication),
            runningApplicationWithProcessIdentifier: pid
        ];
        let ok = if app == nil {
            false
        } else {
            let terminated: bool = msg_send![app, terminate];
            terminated
        };
        pool.drain();
        if ok {
            Ok(())
        } else {
            Err(DriverError::Platform(format!(
                "NSRunningApplication::terminate refused for pid {pid}"
            )))
        }
    }
}

fn pid_for_name(name: &str) -> Result<i32, DriverError> {
    unsafe {
        let pool = NSAutoreleasePool::new(nil);
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        let apps: id = msg_send![workspace, runningApplications];
        let count: usize = msg_send![apps, count];
        let mut found: Option<i32> = None;
        for i in 0..count {
            let app: id = msg_send![apps, objectAtIndex: i];
            let localized: id = msg_send![app, localizedName];
            if localized == nil {
                continue;
            }
            let utf8 = localized.UTF8String();
            if utf8.is_null() {
                continue;
            }
            let s = std::ffi::CStr::from_ptr(utf8)
                .to_string_lossy()
                .into_owned();
            if s.eq_ignore_ascii_case(name) {
                let pid: i32 = msg_send![app, processIdentifier];
                if found.is_some() {
                    pool.drain();
                    return Err(DriverError::Ambiguous(format!(
                        "more than one running application named '{name}' — use --pid or bundle id"
                    )));
                }
                found = Some(pid);
            }
        }
        pool.drain();
        found.ok_or_else(|| {
            DriverError::AppNotFound(format!("no running application named '{name}'"))
        })
    }
}
