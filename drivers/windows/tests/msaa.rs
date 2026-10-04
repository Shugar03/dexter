//! Live MSAA tests — run only on Windows, against real legacy Win32
//! apps on the box. The unit-level tables/merge rules live in
//! `src/msaa.rs` tests and run everywhere; this file proves the COM
//! machinery against real `IAccessible` providers: a whole-window
//! `OBJID_CLIENT` walk, the augment seam, the merge producing no
//! duplicates on covered apps, and the act-path calls (`take_focus`,
//! `put_accValue`, `accDoDefaultAction` handles) on live nodes.
#![cfg(windows)]

use dexter_core::{AppSelector, ObservationScope, Rect};
use dexter_driver::ComputerDriver;
use dexter_windows::msaa::{self, MsaaRef};
use dexter_windows::uia::{self, LiveNode};
use dexter_windows::{win, WindowsDriver};
use std::collections::HashSet;

/// The tests serialize through the same lock the driver uses — COM
/// probing two live trees from parallel threads corrupts neither, but
/// one hung app would make every test look broken. Poison is fine to
/// step over: a prior panic doesn't invalidate the handle itself.
fn serialized() -> std::sync::MutexGuard<'static, ()> {
    uia::OBSERVE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// A launched app whose process dies with the guard — `Child`'s own
/// Drop leaves it running, and an orphaned single-instance app makes
/// the next launch hand off to the orphan and exit.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Launch `exe`, wait for its window, return the kill-on-drop guard
/// plus the pid.
fn launch(exe: &str, args: &[&str]) -> (Running, i32) {
    let mut child = std::process::Command::new(exe)
        .args(args)
        .spawn()
        .unwrap_or_else(|e| panic!("spawn {exe}: {e}"));
    let pid = child.id() as i32;
    for _ in 0..80 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if win::hwnds().iter().any(|(_, p)| *p == pid) {
            std::thread::sleep(std::time::Duration::from_millis(800));
            return (Running(child), pid);
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!(
                "{exe} exited ({status}) before opening a window — a \
                 leftover instance stole the handoff?"
            );
        }
    }
    panic!("{exe} produced no top-level window");
}

fn main_hwnd(pid: i32) -> windows::Win32::Foundation::HWND {
    // The app's named top-level window — its real client host (IME and
    // tool windows come back unnamed).
    let wins = win::list_windows().expect("list windows");
    let w = wins
        .iter()
        .find(|w| w.pid == pid && w.title.as_deref().is_some_and(|t| !t.is_empty()))
        .unwrap_or_else(|| panic!("no named window for pid {pid}"));
    win::hwnd_of(w.id)
}

/// msconfig: an MFC property sheet — the canonical legacy dialog.
#[test]
fn msaa_walks_a_real_window_tree() {
    let _serialize = serialized();
    let (_guard, _uia) = uia::connect().expect("COM+UIA session");
    let (_child, pid) = launch("C:\\Windows\\System32\\msconfig.exe", &[]);
    let hwnd = main_hwnd(pid);
    let w = msaa::walk_hwnd(hwnd, 40, 4000);
    assert!(
        w.forest.len() > 5,
        "msconfig OBJID_CLIENT should yield a real tree, got {}",
        w.forest.len()
    );
    assert_eq!(w.forest.len(), w.handles.len(), "handles align with forest");
    assert!(
        w.forest
            .iter()
            .any(|n| n.raw_role.as_deref() == Some("msaa:ROLE_SYSTEM_PUSHBUTTON")),
        "expected real buttons in the walk: {:?}",
        w.forest
            .iter()
            .map(|n| n.raw_role.as_deref())
            .collect::<Vec<_>>()
    );
    assert!(
        w.forest.iter().all(|n| n.name.is_some()
            || n.role.is_some()
            || n.bounds.is_some()
            || n.raw_role.is_some()),
        "every kept node carries something identifying"
    );
}

/// The augment seam end to end: an empty partition forces the
/// warranted whole-window walk and returns elements + live handles —
/// the exact call observe makes per window.
#[test]
fn augment_produces_real_elements_and_handles() {
    let _serialize = serialized();
    let (_guard, _uia) = uia::connect().expect("COM+UIA session");
    let (_child, pid) = launch("C:\\Windows\\System32\\msconfig.exe", &[]);
    let hwnd = main_hwnd(pid);
    let claimed: HashSet<usize> = HashSet::new();
    let mut next_id = 1u64;
    let (els, handles, _truncated, _errors) =
        msaa::augment(&[], &claimed, hwnd, 40, 4000, &mut next_id);
    assert_eq!(els.len(), handles.len(), "one handle per element");
    assert!(!els.is_empty(), "empty partition must earn the full walk");
    for (e, h) in els.iter().zip(handles.iter()) {
        assert!(
            e.raw_role
                .as_deref()
                .is_some_and(|r| r.starts_with("msaa:")),
            "raw_role carries the msaa origin: {:?}",
            e.raw_role
        );
        assert_eq!(e.source, dexter_core::ElementSource::Accessibility);
        // A live handle answers a live call — the act seam is real.
        let _ = msaa::state_of(h);
    }
}

/// The coverage trigger measured on real apps: UIA covers odbcad32's
/// visible interior, so the merged observation must NOT gain MSAA
/// duplicates of those controls. A multi-instance app is required:
/// msconfig hands off to a running instance, which would collide with
/// the serialized tests' own msconfig launches.
#[test]
fn observe_adds_no_duplicates_on_a_covered_app() {
    // No `serialized()` here — `observe()` serializes itself on the
    // same lock; holding it first would self-deadlock.
    let (_child, pid) = launch("C:\\Windows\\System32\\odbcad32.exe", &[]);
    let driver = WindowsDriver::new();
    let scope = ObservationScope {
        app: Some(AppSelector::Pid(pid)),
        ..Default::default()
    };
    let obs = driver.observe(&scope).expect("observe");
    assert!(!obs.elements.is_empty());
    // Whatever MSAA found, it must never duplicate a control the UIA
    // walk already surfaced: no two visible elements share name+bounds
    // across the uia/msaa boundary unless one is a container.
    let dupes = obs
        .elements
        .iter()
        .filter(|e| {
            e.raw_role
                .as_deref()
                .is_some_and(|r| r.starts_with("msaa:"))
        })
        .filter(|m| {
            obs.elements.iter().any(|u| {
                !u.raw_role
                    .as_deref()
                    .is_some_and(|r| r.starts_with("msaa:"))
                    && u.name == m.name
                    && identical(u.bounds, m.bounds)
            })
        })
        .count();
    assert_eq!(dupes, 0, "MSAA duplicated UIA-covered controls");
}

fn identical(a: Option<Rect>, b: Option<Rect>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x.x == y.x && x.y == y.y && x.w == y.w && x.h == y.h)
}

/// `walk_roots` carries LiveNode handles aligned with elements — the
/// merged order act() re-resolves against.
#[test]
fn live_walk_nodes_align_with_elements() {
    let _serialize = serialized();
    let (_guard, uia) = uia::connect().expect("COM+UIA session");
    let (_child, pid) = launch("C:\\Windows\\System32\\msconfig.exe", &[]);
    let roots: Vec<_> = win::hwnds()
        .iter()
        .filter(|(_, p)| *p == pid)
        .map(|(h, _)| (*h, uia.element_from(*h).ok().map(|e| uia.normalize(e))))
        .collect();
    let live = uia::walk_roots(&uia, &roots, 40, 4000);
    assert_eq!(live.elements.len(), live.nodes.len());
    // Every node is one of the two handle kinds, in element order.
    for node in &live.nodes {
        match node {
            LiveNode::Uia(el) => {
                let _ = unsafe { el.CurrentControlType() };
            }
            LiveNode::Msaa(m) => {
                let _ = msaa::describe(m);
            }
        }
    }
}

/// charmap's "Characters to copy" edit is a real Win32 EDIT control —
/// `put_accValue` writes it through the same call act's SetValue arm
/// makes, and the read-back proves the write happened.
#[test]
fn put_acc_value_round_trips_on_a_real_edit() {
    let _serialize = serialized();
    let (_guard, _uia) = uia::connect().expect("COM+UIA session");
    let (_child, pid) = launch("C:\\Windows\\System32\\charmap.exe", &[]);
    let hwnd = main_hwnd(pid);
    let w = msaa::walk_hwnd(hwnd, 40, 4000);
    let idx = w
        .forest
        .iter()
        .position(|n| n.raw_role.as_deref() == Some("msaa:ROLE_SYSTEM_TEXT"))
        .expect("charmap hosts a real EDIT control");
    let m: &MsaaRef = &w.handles[idx];
    msaa::try_put_value(m, "Q").expect("put_accValue on charmap's edit");
    let var = msaa::var_child(m.child);
    let read_back = unsafe { m.acc.get_accValue(&var) }
        .map(|b| String::from_utf16_lossy(&b))
        .unwrap_or_default();
    // Win32 EDIT appends a CR on accValue writes — the text itself is proof.
    assert_eq!(
        read_back.trim_end_matches(['\r', '\n']),
        "Q",
        "the edit's value is the written text"
    );
    // Focus + describe on the same live handle.
    msaa::take_focus(m).expect("accSelect(TAKEFOCUS)");
    assert!(!msaa::describe(m).is_empty());
    assert!(msaa::bounds_of(m).is_some(), "a real edit reports bounds");
}
