use dexter_core::{Event, EventKind};
use dexter_desktop::{
    load_policy_view, parse_args, Desktop, JournalStatus, PolicyView, JOURNAL_CAP,
};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_PATH: AtomicUsize = AtomicUsize::new(0);

fn temp_path(label: &str) -> PathBuf {
    let id = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "dexter-desktop-{label}-{}-{id}",
        std::process::id()
    ))
}

fn event_line(kind: EventKind, data: serde_json::Value) -> String {
    serde_json::to_string(&Event::new(kind, data)).unwrap()
}

#[test]
fn no_events_path_is_not_configured() {
    let snapshot = Desktop::new(None, None).poll();

    assert_eq!(snapshot.journal, JournalStatus::NotConfigured);
    assert!(snapshot.events.is_empty());
}

#[test]
fn missing_events_file_is_waiting() {
    let path = temp_path("missing-events");
    let snapshot = Desktop::new(Some(path.clone()), None).poll();

    assert_eq!(
        snapshot.journal,
        JournalStatus::Waiting {
            path: path.display().to_string()
        }
    );
    assert!(snapshot.events.is_empty());
}

#[test]
fn events_are_live_and_keep_oldest_to_newest_order() {
    let path = temp_path("ordered-events");
    let lines = [
        event_line(EventKind::ObservationCreated, serde_json::json!({"n": 1})),
        event_line(EventKind::TaskCompleted, serde_json::json!({"n": 2})),
        event_line(EventKind::TaskFailed, serde_json::json!({"n": 3})),
    ];
    fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();

    let snapshot = Desktop::new(Some(path.clone()), None).poll();

    assert_eq!(
        snapshot.journal,
        JournalStatus::Live {
            path: path.display().to_string()
        }
    );
    assert_eq!(snapshot.events.len(), 3);
    assert_eq!(
        snapshot
            .events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>(),
        ["ObservationCreated", "TaskCompleted", "TaskFailed"]
    );
    assert_eq!(snapshot.events[0].summary, r#"{"n":1}"#);
    fs::remove_file(path).unwrap();
}

#[test]
fn trailing_partial_line_waits_for_newline() {
    let path = temp_path("partial-events");
    let first = event_line(EventKind::ObservationCreated, serde_json::json!({"n": 1}));
    let second = event_line(EventKind::TaskCompleted, serde_json::json!({"n": 2}));
    fs::write(&path, format!("{first}\n{second}")).unwrap();

    let mut desktop = Desktop::new(Some(path.clone()), None);
    assert_eq!(desktop.poll().events.len(), 1);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\n")
        .unwrap();

    let snapshot = desktop.poll();
    assert_eq!(snapshot.events.len(), 2);
    assert_eq!(snapshot.events[1].kind, "TaskCompleted");
    fs::remove_file(path).unwrap();
}

#[test]
fn journal_ring_keeps_newest_events_at_the_cap() {
    let path = temp_path("capped-events");
    let contents = (0..JOURNAL_CAP + 5)
        .map(|n| event_line(EventKind::ObservationCreated, serde_json::json!({"n": n})))
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&path, format!("{contents}\n")).unwrap();

    let snapshot = Desktop::new(Some(path.clone()), None).poll();

    assert_eq!(snapshot.events.len(), JOURNAL_CAP);
    assert_eq!(snapshot.events.first().unwrap().summary, r#"{"n":5}"#);
    assert_eq!(
        snapshot.events.last().unwrap().summary,
        format!(r#"{{"n":{}}}"#, JOURNAL_CAP + 4)
    );
    fs::remove_file(path).unwrap();
}

#[test]
fn absent_policy_uses_embedded_summary() {
    assert!(matches!(
        load_policy_view(None),
        PolicyView::Embedded { summary } if summary.mutating == "require_approval"
    ));
}

#[test]
fn valid_policy_file_is_reported_with_its_summary() {
    let path = temp_path("valid-policy.toml");
    fs::write(
        &path,
        r#"
        [defaults]
        mutating = "deny"
        "#,
    )
    .unwrap();

    assert!(matches!(
        load_policy_view(Some(&path)),
        PolicyView::File { path: actual_path, summary }
            if actual_path == path.display().to_string() && summary.mutating == "deny"
    ));
    fs::remove_file(path).unwrap();
}

#[test]
fn malformed_policy_is_an_error_not_embedded_default() {
    let path = temp_path("malformed-policy.toml");
    fs::write(&path, "this is not [toml").unwrap();

    assert!(matches!(
        load_policy_view(Some(&path)),
        PolicyView::Error { path: actual_path, .. }
            if actual_path == path.display().to_string()
    ));
    fs::remove_file(path).unwrap();
}

#[test]
fn nonexistent_policy_is_an_error_not_embedded_default() {
    let path = temp_path("missing-policy.toml");

    assert!(matches!(
        load_policy_view(Some(&path)),
        PolicyView::Error { path: actual_path, .. }
            if actual_path == path.display().to_string()
    ));
}

#[test]
fn snapshot_serializes_the_read_only_panels() {
    let snapshot = Desktop::new(None, None).poll();
    let value = serde_json::to_value(snapshot).unwrap();

    assert!(value.get("journal").is_some());
    assert!(value.get("events").is_some());
    assert!(value.get("policy").is_some());
    assert_eq!(value["journal"]["state"], "not_configured");
    assert_eq!(value["policy"]["state"], "embedded");
}

#[test]
fn parses_events_and_policy_paths() {
    let args = parse_args([
        "--events".to_string(),
        "journal.jsonl".to_string(),
        "--policy".to_string(),
        "policy.toml".to_string(),
    ])
    .unwrap();

    assert_eq!(args.events, Some(PathBuf::from("journal.jsonl")));
    assert_eq!(args.policy, Some(PathBuf::from("policy.toml")));
}

#[test]
fn unknown_flags_and_missing_values_are_rejected() {
    assert!(parse_args(["--unknown".to_string()]).is_err());
    assert!(parse_args(["--events".to_string()]).is_err());
}
