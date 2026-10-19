//! Tauri shell for journal snapshots, approval decisions, and read-only sessions.

use dexter_desktop::{parse_args, Desktop, SessionDetail, SessionsView, Snapshot, Verdict};
use std::sync::Mutex;

#[tauri::command]
fn snapshot(desktop: tauri::State<'_, Mutex<Desktop>>) -> Snapshot {
    desktop.lock().expect("desktop state poisoned").poll()
}

#[tauri::command]
fn approve(fingerprint: String, desktop: tauri::State<'_, Mutex<Desktop>>) -> Result<(), String> {
    desktop
        .lock()
        .expect("desktop state poisoned")
        .decide(&fingerprint, Verdict::Grant)
}

#[tauri::command]
fn deny(fingerprint: String, desktop: tauri::State<'_, Mutex<Desktop>>) -> Result<(), String> {
    desktop
        .lock()
        .expect("desktop state poisoned")
        .decide(&fingerprint, Verdict::Deny)
}

#[tauri::command]
fn sessions(desktop: tauri::State<'_, Mutex<Desktop>>) -> SessionsView {
    desktop.lock().expect("desktop state poisoned").sessions()
}

#[tauri::command]
fn open_session(
    name: String,
    desktop: tauri::State<'_, Mutex<Desktop>>,
) -> Result<SessionDetail, String> {
    desktop
        .lock()
        .expect("desktop state poisoned")
        .open_session(&name)
}

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    tauri::Builder::default()
        .manage(Mutex::new(Desktop::new_with_sessions(
            args.events,
            args.policy,
            args.approvals,
            args.sessions,
        )))
        .invoke_handler(tauri::generate_handler![
            snapshot,
            approve,
            deny,
            sessions,
            open_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running Dexter desktop");
}
