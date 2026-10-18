//! Tauri shell for journal snapshots and live approval decisions.

use dexter_desktop::{parse_args, Desktop, Snapshot, Verdict};
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

fn main() {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    tauri::Builder::default()
        .manage(Mutex::new(Desktop::new(
            args.events,
            args.policy,
            args.approvals,
        )))
        .invoke_handler(tauri::generate_handler![snapshot, approve, deny])
        .run(tauri::generate_context!())
        .expect("error while running Dexter desktop");
}
