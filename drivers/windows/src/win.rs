//! Top-level window enumeration via `EnumWindows`.
//!
//! Every returned `Window` is a real `HWND`: `id` is the handle value
//! truncated to `u32` (HWNDs fit 32 bits on every shipping Windows),
//! `app` the owning process' image stem, `bundle_id` the AUMID for
//! packaged apps. Invisible/minimized windows are still listed with
//! `on_screen: false` — the same window-list honesty CGWindowList
//! gives on macOS.

use dexter_core::{Rect, Window};
use dexter_driver::DriverError;
use std::collections::HashMap;
use windows::core::BOOL;
use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsIconic, IsWindowVisible,
};

struct Raw {
    hwnd: HWND,
    pid: i32,
    title: Option<String>,
    bounds: Option<Rect>,
    on_screen: bool,
}

/// `(HWND, owner pid)` for every top-level window — used by app
/// resolution and window-scoped observation to map ids back to
/// handles.
pub fn hwnds() -> Vec<(HWND, i32)> {
    raw_windows().into_iter().map(|r| (r.hwnd, r.pid)).collect()
}

/// Distinct pids owning at least one top-level window — the set app
/// selectors can meaningfully resolve to.
pub fn windowed_pids() -> Vec<i32> {
    let mut seen = std::collections::HashSet::new();
    hwnds()
        .into_iter()
        .map(|(_, pid)| pid)
        .filter(|pid| seen.insert(*pid))
        .collect()
}

fn raw_windows() -> Vec<Raw> {
    unsafe extern "system" fn cb(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let out = &mut *(lparam.0 as *mut Vec<Raw>);
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 {
            return true.into();
        }
        let title = {
            let len = GetWindowTextLengthW(hwnd);
            if len <= 0 {
                None
            } else {
                let mut buf = vec![0u16; len as usize + 1];
                let read = GetWindowTextW(hwnd, buf.as_mut_slice());
                (read > 0).then(|| String::from_utf16_lossy(&buf[..read as usize]))
            }
        };
        let mut rc = RECT::default();
        let bounds = GetWindowRect(hwnd, &mut rc).ok().map(|_| Rect {
            x: rc.left as f64,
            y: rc.top as f64,
            w: (rc.right - rc.left) as f64,
            h: (rc.bottom - rc.top) as f64,
        });
        // A window is on-screen when Windows would draw it: visible
        // and not minimized (minimized windows keep WS_VISIBLE but
        // report the -32000 parked rect).
        let on_screen = IsWindowVisible(hwnd).as_bool() && !IsIconic(hwnd).as_bool();
        out.push(Raw {
            hwnd,
            pid: pid as i32,
            title,
            bounds,
            on_screen,
        });
        true.into()
    }
    let mut out = Vec::new();
    // EnumWindows failure is only reported via GetLastError and never
    // for a callback that keeps returning TRUE — an empty list is
    // honest data, not an error.
    let _ = unsafe { EnumWindows(Some(cb), LPARAM(&mut out as *mut _ as isize)) };
    out
}

/// `Window::id` round-trip: `HWND` ⇄ `u32`.
pub fn hwnd_of(id: u32) -> HWND {
    HWND(id as usize as *mut core::ffi::c_void)
}

pub fn hwnd_id(hwnd: HWND) -> u32 {
    hwnd.0 as usize as u32
}

/// All top-level windows. `layer` is always 0 — `EnumWindows` yields
/// only top-level HWNDs; UIA supplies whatever lives inside them.
pub fn list_windows() -> Result<Vec<Window>, DriverError> {
    let mut names: HashMap<i32, Option<String>> = HashMap::new();
    let mut aumids: HashMap<i32, Option<String>> = HashMap::new();
    let out = raw_windows()
        .into_iter()
        .filter_map(|r| {
            let bounds = r.bounds?;
            let app = names
                .entry(r.pid)
                .or_insert_with(|| crate::apps::process_name(r.pid))
                .clone()
                // Protected/system processes can't be opened for a
                // name — the window is still real, its owner is just
                // unreadable, so it lists with an empty `app`.
                .unwrap_or_default();
            let bundle_id = aumids
                .entry(r.pid)
                .or_insert_with(|| crate::apps::aumid_for_pid(r.pid))
                .clone();
            Some(Window {
                id: hwnd_id(r.hwnd),
                pid: r.pid,
                app,
                bundle_id,
                title: r.title,
                bounds,
                on_screen: r.on_screen,
                layer: 0,
            })
        })
        .collect();
    Ok(out)
}
