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

    // `--children` dumps the descendant-HWND census the MSAA pass decides
    // on: visibility, class, rect, claimed-by-UIA, interior UIA count.
    if std::env::var("PROBE_CHILDREN").is_ok() {
        children(pid);
        return;
    }
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
        let walk = dexter_windows::msaa::walk_hwnd(hwnd, 40, 4000, false);
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
            let hidden = obs
                .elements
                .iter()
                .filter(|e| e.hidden.unwrap_or(false))
                .count();
            println!(
                "pid {pid}: {} window(s), {} element(s), {} MSAA-sourced, {} hidden, \
                 truncated={}, errors={}, ax_limited={}",
                obs.windows.len(),
                obs.elements.len(),
                msaa.len(),
                hidden,
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

/// Descendant-HWND census for one pid: every child window with its
/// visibility, class, title and rect, plus whether the UIA walk claimed
/// it (`NativeWindowHandle`) and how many UIA elements lie strictly
/// inside its rect — the exact inputs `msaa::augment` decides on.
#[cfg(windows)]
fn children(pid: i32) {
    use dexter_windows::msaa;
    use dexter_windows::uia::{self, LiveNode};
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumChildWindows, GetClassNameW, GetWindowRect, GetWindowTextW, IsChild, IsWindowVisible,
    };

    fn cls(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let n = unsafe { GetClassNameW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n as usize])
    }
    fn txt(hwnd: HWND) -> String {
        let mut buf = [0u16; 128];
        let n = unsafe { GetWindowTextW(hwnd, &mut buf) };
        String::from_utf16_lossy(&buf[..n as usize])
    }
    fn rect(hwnd: HWND) -> Option<dexter_core::Rect> {
        let mut rc = windows::Win32::Foundation::RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rc) }.ok().and_then(|_| {
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            (w > 0 && h > 0).then_some(dexter_core::Rect {
                x: rc.left as f64,
                y: rc.top as f64,
                w: w as f64,
                h: h as f64,
            })
        })
    }

    let _g = uia::OBSERVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (_guard, uia) = uia::connect().expect("COM+UIA");
    let roots: Vec<_> = dexter_windows::win::hwnds()
        .iter()
        .filter(|(_, p)| *p == pid)
        .map(|(h, _)| (*h, uia.element_from(*h).ok().map(|e| uia.normalize(e))))
        .collect();
    let live = uia::walk_roots(&uia, &roots, 40, 4000);
    // HWNDs UIA claimed (every element's NativeWindowHandle).
    let mut claimed: std::collections::HashSet<usize> = std::collections::HashSet::new();
    for node in &live.nodes {
        if let LiveNode::Uia(el) = node {
            if let Ok(h) = unsafe { el.CurrentNativeWindowHandle() } {
                if !h.0.is_null() {
                    claimed.insert(h.0 as usize);
                }
            }
        }
    }
    for (top, _) in &roots {
        println!(
            "TOP {:x} cls={:?} txt={:?} rect={:?} elements={}",
            top.0 as usize,
            cls(*top),
            txt(*top),
            rect(*top),
            live.elements.len(),
        );
        let mut kids: Vec<HWND> = Vec::new();
        unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
            let out = &mut *(lparam.0 as *mut Vec<HWND>);
            out.push(hwnd);
            windows::core::BOOL(1)
        }
        let _ =
            unsafe { EnumChildWindows(Some(*top), Some(cb), LPARAM(&mut kids as *mut _ as isize)) };
        for k in &kids {
            let r = rect(*k);
            let interior = r
                .map(|r| {
                    live.elements
                        .iter()
                        .filter(|e| {
                            e.bounds.is_some_and(|b| {
                                b.x >= r.x - 4.0
                                    && b.y >= r.y - 4.0
                                    && b.x + b.w <= r.x + r.w + 4.0
                                    && b.y + b.h <= r.y + r.h + 4.0
                                    && (b.w < r.w - 8.0 || b.h < r.h - 8.0)
                            })
                        })
                        .count()
                })
                .unwrap_or(0);
            let nested_parent = kids
                .iter()
                .filter(|o| *o != k)
                .find(|o| unsafe { IsChild(**o, *k) }.as_bool());
            println!(
                "  {:x} vis={} claimed={} interior={} cls={:?} txt={:?} rect={:?} under={:?}",
                k.0 as usize,
                unsafe { IsWindowVisible(*k) }.as_bool(),
                claimed.contains(&(k.0 as usize)),
                interior,
                cls(*k),
                txt(*k),
                r,
                nested_parent.map(|h| h.0 as usize),
            );
        }
        // What the MSAA walk sees at OBJID_CLIENT per unclaimed child.
        let _ = msaa::ROLE_SYSTEM_WINDOW; // keep the import used
    }
}
