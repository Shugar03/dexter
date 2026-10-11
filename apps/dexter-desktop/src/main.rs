//! Tauri shell exposing only the read-only `snapshot` command.

use dexter_desktop::{parse_args, Desktop, Snapshot};
use std::sync::Mutex;

#[tauri::command]
fn snapshot(desktop: tauri::State<'_, Mutex<Desktop>>) -> Snapshot {
    desktop.lock().expect("desktop state poisoned").poll()
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
        .manage(Mutex::new(Desktop::new(args.events, args.policy)))
        .invoke_handler(tauri::generate_handler![snapshot])
        .run(tauri::generate_context!())
        .expect("error while running Dexter desktop");
}
