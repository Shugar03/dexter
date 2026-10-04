//! Measurement probe for the MSAA fallback: launch a legacy Win32
//! app, observe it, and report which elements are MSAA-sourced vs
//! UIA-sourced. What the fallback adds on real apps is what justifies
//! when the pass runs (documented in docs/sdd/windows.md).
//!
//! Usage: probe.exe <exe path> [args...]
//! Example: probe.exe C:\Windows\System32\charmap.exe

#[cfg(windows)]
use dexter_core::{AppSelector, ObservationScope};
#[cfg(windows)]
use dexter_driver::ComputerDriver;
#[cfg(windows)]
use dexter_windows::WindowsDriver;

fn main() {
    #[cfg(windows)]
    run();
    #[cfg(not(windows))]
    eprintln!("probe is a Windows-only measurement tool");
}

#[cfg(windows)]
fn run() {
    let mut args = std::env::args().skip(1);
    let Some(exe) = args.next() else {
        eprintln!("usage: probe <exe> [args...]");
        std::process::exit(2);
    };
    let rest: Vec<String> = args.collect();
    // `probe --pid <n>` observes an already-running process instead of
    // spawning (for measuring a specific live instance).
    let (child, pid) = if exe == "--pid" {
        let pid: i32 = rest
            .first()
            .and_then(|s| s.parse().ok())
            .expect("--pid <number>");
        (None, pid)
    } else {
        let child = std::process::Command::new(&exe)
            .args(&rest)
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {exe}: {e}"));
        let settle_ms: u64 = std::env::var("PROBE_SETTLE_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(2500);
        std::thread::sleep(std::time::Duration::from_millis(settle_ms));
        let pid = child.id() as i32;
        (Some(child), pid)
    };

    // `--walk-hwnd <pid>` instead runs the raw per-window MSAA walk on
    // the process's named window — timing each stage.
    if std::env::var("PROBE_WALK_HWND").is_ok() {
        let _g = dexter_windows::uia::OBSERVE_LOCK.lock().unwrap();
        let (_guard, _uia) = dexter_windows::uia::connect().expect("COM+UIA");
        let wins = dexter_windows::win::list_windows().expect("windows");
        let w = wins
            .iter()
            .find(|w| w.pid == pid && w.title.as_deref().is_some_and(|t| !t.is_empty()))
            .expect("named window");
        let hwnd = dexter_windows::win::hwnd_of(w.id);
        let t0 = std::time::Instant::now();
        let walk = dexter_windows::msaa::walk_hwnd(hwnd, 40, 4000);
        println!(
            "walk_hwnd: {} nodes, {} handles, truncated={} errors={} in {:?}",
            walk.forest.len(),
            walk.handles.len(),
            walk.truncated,
            walk.errors,
            t0.elapsed()
        );
        return;
    }
    let driver = WindowsDriver::new();
    let scope = ObservationScope {
        app: Some(AppSelector::Pid(pid)),
        ..Default::default()
    };
    match driver.observe(&scope) {
        Ok(obs) => {
            let msaa: Vec<_> = obs
                .elements
                .iter()
                .filter(|e| {
                    e.raw_role
                        .as_deref()
                        .is_some_and(|r| r.starts_with("msaa:"))
                })
                .collect();
            println!(
                "pid {pid}: {} window(s), {} element(s), {} MSAA-sourced, \
                 truncated={}, errors={}, ax_limited={}",
                obs.windows.len(),
                obs.elements.len(),
                msaa.len(),
                obs.elements_truncated,
                obs.collection_errors,
                obs.ax_limited,
            );
            for e in &msaa {
                println!(
                    "  msaa  d{} role={:?} raw={} name={:?} value={:?} enabled={:?} actions={:?} bounds={:?}",
                    e.depth, e.role, e.raw_role.as_deref().unwrap_or(""), e.name,
                    e.value.as_deref().map(|v| &v[..v.len().min(40)]),
                    e.enabled, e.actions, e.bounds,
                );
            }
            for e in obs.elements.iter().filter(|e| {
                !e.raw_role
                    .as_deref()
                    .is_some_and(|r| r.starts_with("msaa:"))
            }) {
                println!(
                    "  uia   d{} role={:?} raw={} name={:?} actions={:?} bounds={:?}",
                    e.depth,
                    e.role,
                    e.raw_role.as_deref().unwrap_or(""),
                    e.name,
                    e.actions,
                    e.bounds,
                );
            }
        }
        Err(e) => println!("observe failed: {e:?}"),
    }
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
}
