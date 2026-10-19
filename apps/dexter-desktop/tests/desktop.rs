use dexter_core::{Event, EventKind};
use dexter_desktop::{
    load_policy_view, parse_args, ApprovalChannel, Desktop, JournalStatus, PolicyView,
    SessionDetail, SessionsView, Verdict, JOURNAL_CAP,
};
use dexter_engine::{Engine, RunConfig, Step, StepStatus};
use dexter_sim::SimDriver;
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

fn pending_event(fingerprint: &str) -> String {
    event_line(
        EventKind::HumanApprovalRequired,
        serde_json::json!({
            "fingerprint": fingerprint,
            "reason": "mutation requires approval",
        }),
    )
}

fn desktop_with_pending(label: &str, approvals: Option<PathBuf>) -> (Desktop, PathBuf) {
    let events = temp_path(&format!("{label}-events"));
    fs::write(
        &events,
        format!("{}\n", pending_event("pending-fingerprint")),
    )
    .unwrap();
    let mut desktop = Desktop::new(Some(events.clone()), None, approvals);
    assert_eq!(desktop.poll().pending.len(), 1);
    (desktop, events)
}

#[test]
fn no_events_path_is_not_configured() {
    let snapshot = Desktop::new(None, None, None).poll();

    assert_eq!(snapshot.journal, JournalStatus::NotConfigured);
    assert!(snapshot.events.is_empty());
}

#[test]
fn missing_events_file_is_waiting() {
    let path = temp_path("missing-events");
    let snapshot = Desktop::new(Some(path.clone()), None, None).poll();

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

    let snapshot = Desktop::new(Some(path.clone()), None, None).poll();

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

    let mut desktop = Desktop::new(Some(path.clone()), None, None);
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

    let snapshot = Desktop::new(Some(path.clone()), None, None).poll();

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
    let snapshot = Desktop::new(None, None, None).poll();
    let value = serde_json::to_value(snapshot).unwrap();

    assert!(value.get("journal").is_some());
    assert!(value.get("events").is_some());
    assert!(value.get("policy").is_some());
    assert!(value.get("approvals").is_some());
    assert!(value.get("pending").is_some());
    assert_eq!(value["journal"]["state"], "not_configured");
    assert_eq!(value["policy"]["state"], "embedded");
    assert_eq!(value["approvals"]["state"], "disabled");
}

#[test]
fn parses_events_and_policy_paths() {
    let args = parse_args([
        "--events".to_string(),
        "journal.jsonl".to_string(),
        "--policy".to_string(),
        "policy.toml".to_string(),
        "--approvals".to_string(),
        "inbox.jsonl".to_string(),
        "--sessions".to_string(),
        "sessions".to_string(),
    ])
    .unwrap();

    assert_eq!(args.events, Some(PathBuf::from("journal.jsonl")));
    assert_eq!(args.policy, Some(PathBuf::from("policy.toml")));
    assert_eq!(args.approvals, Some(PathBuf::from("inbox.jsonl")));
    assert_eq!(args.sessions, Some(PathBuf::from("sessions")));
}

#[test]
fn unknown_flags_and_missing_values_are_rejected() {
    assert!(parse_args(["--unknown".to_string()]).is_err());
    assert!(parse_args(["--events".to_string()]).is_err());
    assert!(parse_args(["--approvals".to_string()]).is_err());
    assert!(parse_args(["--sessions".to_string()]).is_err());
}

#[test]
fn sessions_without_a_directory_are_not_configured() {
    assert_eq!(
        Desktop::new(None, None, None).sessions(),
        SessionsView::NotConfigured
    );
}

#[test]
fn session_listing_filters_sorts_and_caps_entries() {
    let directory = temp_path("sessions-listing");
    fs::create_dir_all(&directory).unwrap();
    for index in 0..202 {
        fs::write(directory.join(format!("recording-{index:03}.json")), "{}").unwrap();
    }
    fs::write(directory.join("journal.jsonl"), "{}\n").unwrap();
    fs::write(directory.join("ignored.txt"), "{}").unwrap();
    fs::create_dir(directory.join("nested.json")).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        directory.join("recording-000.json"),
        directory.join("linked.json"),
    )
    .unwrap();

    let desktop = Desktop::new_with_sessions(None, None, None, Some(directory.clone()));
    let SessionsView::Ready {
        entries, truncated, ..
    } = desktop.sessions()
    else {
        panic!("configured sessions directory should be listed");
    };
    assert_eq!(entries.len(), 200);
    assert!(truncated);
    assert!(entries.iter().any(|entry| entry.name == "journal.jsonl"));
    assert!(entries
        .iter()
        .all(|entry| entry.name.ends_with(".json") || entry.name.ends_with(".jsonl")));
    assert!(!entries.iter().any(|entry| entry.name == "linked.json"));
    assert!(entries.windows(2).all(|pair| {
        pair[0].modified_ms > pair[1].modified_ms
            || (pair[0].modified_ms == pair[1].modified_ms && pair[0].name <= pair[1].name)
    }));
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn missing_sessions_directory_is_an_error_view() {
    let directory = temp_path("missing-sessions");
    let desktop = Desktop::new_with_sessions(None, None, None, Some(directory.clone()));

    assert!(matches!(
        desktop.sessions(),
        SessionsView::Error { path, .. } if path == directory.display().to_string()
    ));
}

#[test]
fn open_session_rejects_non_bare_names_and_symlinks() {
    let directory = temp_path("unsafe-session-names");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("target.json"), "{}").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(directory.join("target.json"), directory.join("linked.json"))
        .unwrap();
    let desktop = Desktop::new_with_sessions(None, None, None, Some(directory.clone()));

    for name in [
        "",
        ".",
        "..",
        "../x.json",
        "a/b.json",
        r"a\b.json",
        "no-extension",
        "x.json.bak",
        "bad\0name.json",
    ] {
        assert!(desktop.open_session(name).is_err(), "{name:?}");
    }
    #[cfg(unix)]
    assert!(desktop.open_session("linked.json").is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn open_session_rejects_files_over_sixteen_mib() {
    let directory = temp_path("oversized-session");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("large.json"),
        vec![b' '; 16 * 1024 * 1024 + 1],
    )
    .unwrap();
    let desktop = Desktop::new_with_sessions(None, None, None, Some(directory.clone()));

    assert!(desktop.open_session("large.json").is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn open_session_deserializes_recording_steps_strictly() {
    let directory = temp_path("recording-session");
    fs::create_dir_all(&directory).unwrap();
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../datasets/recordings/modal-confirm.json");
    fs::copy(source, directory.join("modal-confirm.json")).unwrap();
    let desktop = Desktop::new_with_sessions(None, None, None, Some(directory.clone()));

    let detail = desktop.open_session("modal-confirm.json").unwrap();
    let SessionDetail::Recording {
        name,
        scenario,
        goal,
        outcome,
        steps,
    } = detail
    else {
        panic!("JSON recording should open as a recording");
    };
    assert_eq!(name, "modal-confirm.json");
    assert_eq!(scenario, "modal-confirm");
    assert_eq!(goal, "descartar el borrador");
    assert_eq!(outcome, "completed");
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[0].index, 1);
    assert_eq!(steps[0].action_json["type"], "click");
    assert_eq!(steps[0].app_json, serde_json::Value::Null);
    assert_eq!(steps[0].observes_before, 1);
    assert_eq!(serde_json::to_value(steps[0].result).unwrap(), "done");

    let mut recording: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.join("modal-confirm.json")).unwrap()).unwrap();
    recording["steps"][0]["result"] = serde_json::Value::String("unknown".into());
    fs::write(
        directory.join("tampered.json"),
        serde_json::to_vec(&recording).unwrap(),
    )
    .unwrap();
    assert!(desktop.open_session("tampered.json").is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn open_session_parses_journals_all_or_error() {
    let directory = temp_path("journal-session");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("valid.jsonl"),
        format!(
            "{}\n",
            event_line(
                EventKind::TaskCompleted,
                serde_json::json!({"outcome": "completed"})
            )
        ),
    )
    .unwrap();
    fs::write(directory.join("malformed.jsonl"), "{\"kind\":\n").unwrap();
    let desktop = Desktop::new_with_sessions(None, None, None, Some(directory.clone()));

    let detail = desktop.open_session("valid.jsonl").unwrap();
    let SessionDetail::Journal { name, events } = detail else {
        panic!("JSONL journal should open as a timeline");
    };
    assert_eq!(name, "valid.jsonl");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "TaskCompleted");
    assert!(desktop.open_session("malformed.jsonl").is_err());
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn pending_approvals_follow_journal_confirmation_not_ring_capacity() {
    let events = temp_path("pending-events");
    fs::write(
        &events,
        format!("{}\n", pending_event("pending-fingerprint")),
    )
    .unwrap();
    let mut desktop = Desktop::new(Some(events.clone()), None, None);
    let first = desktop.poll();
    assert_eq!(first.pending.len(), 1);
    assert_eq!(first.pending[0].fingerprint, "pending-fingerprint");
    assert_eq!(first.pending[0].reason, "mutation requires approval");
    assert_eq!(first.pending[0].sent, None);

    let terminal_decisions = [
        "granted",
        "grant_rejected",
        "denied_by_human",
        "deny_rejected",
        "approved",
    ];
    for decision in terminal_decisions {
        let event = event_line(
            EventKind::PolicyChecked,
            serde_json::json!({
                "fingerprint": "pending-fingerprint",
                "decision": decision,
            }),
        );
        fs::OpenOptions::new()
            .append(true)
            .open(&events)
            .unwrap()
            .write_all(format!("{event}\n").as_bytes())
            .unwrap();
        assert!(desktop.poll().pending.is_empty(), "{decision}");

        let request = pending_event("pending-fingerprint");
        fs::OpenOptions::new()
            .append(true)
            .open(&events)
            .unwrap()
            .write_all(format!("{request}\n").as_bytes())
            .unwrap();
        assert_eq!(
            desktop.poll().pending.len(),
            1,
            "re-request after {decision}"
        );
    }

    let approve_all = event_line(
        EventKind::HumanApprovalRequired,
        serde_json::json!({
            "fingerprint": "approve-all-fingerprint",
            "reason": "operator approved all",
            "granted": "approve_all",
        }),
    );
    fs::OpenOptions::new()
        .append(true)
        .open(&events)
        .unwrap()
        .write_all(format!("{approve_all}\n").as_bytes())
        .unwrap();
    assert_eq!(
        desktop
            .poll()
            .pending
            .iter()
            .map(|pending| pending.fingerprint.as_str())
            .collect::<Vec<_>>(),
        ["pending-fingerprint"]
    );

    let filler = (0..=JOURNAL_CAP)
        .map(|n| event_line(EventKind::ObservationCreated, serde_json::json!({"n": n})))
        .collect::<Vec<_>>()
        .join("\n");
    fs::OpenOptions::new()
        .append(true)
        .open(&events)
        .unwrap()
        .write_all(format!("{filler}\n").as_bytes())
        .unwrap();
    let capped = desktop.poll();
    assert_eq!(capped.events.len(), JOURNAL_CAP);
    assert_eq!(capped.pending.len(), 1);

    fs::remove_file(events).unwrap();
}

#[test]
fn unknown_fingerprint_does_not_create_or_write_the_inbox() {
    let inbox = temp_path("unknown-inbox");
    let (mut desktop, events) = desktop_with_pending("unknown-decision", Some(inbox.clone()));

    assert!(desktop
        .decide("unknown-fingerprint", Verdict::Grant)
        .is_err());
    assert!(!inbox.exists());
    fs::remove_file(events).unwrap();
}

#[test]
fn disabled_approval_channel_rejects_decisions_without_writing() {
    let inbox = temp_path("disabled-inbox");
    let (mut desktop, events) = desktop_with_pending("disabled-decision", None);

    assert!(desktop
        .decide("pending-fingerprint", Verdict::Deny)
        .is_err());
    assert!(!inbox.exists());
    assert_eq!(desktop.poll().approvals, ApprovalChannel::Disabled);
    fs::remove_file(events).unwrap();
}

#[test]
fn valid_decision_appends_one_line_and_marks_pending_as_sent() {
    let inbox = temp_path("grant-inbox");
    let (mut desktop, events) = desktop_with_pending("valid-decision", Some(inbox.clone()));

    desktop
        .decide("pending-fingerprint", Verdict::Grant)
        .unwrap();

    let contents = fs::read_to_string(&inbox).unwrap();
    assert!(contents.ends_with('\n'));
    let line = contents.strip_suffix('\n').unwrap();
    let value: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(value["fingerprint"], "pending-fingerprint");
    assert_eq!(value["decision"], "grant");
    assert!(value["ts_ms"].as_u64().is_some());
    let snapshot = desktop.poll();
    assert_eq!(snapshot.pending.len(), 1);
    assert_eq!(snapshot.pending[0].sent.as_deref(), Some("grant"));
    assert_eq!(
        snapshot.approvals,
        ApprovalChannel::Enabled {
            path: inbox.display().to_string()
        }
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&inbox).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    fs::remove_file(inbox).unwrap();
    fs::remove_file(events).unwrap();
}

#[cfg(unix)]
#[test]
fn existing_group_or_world_writable_inbox_is_rejected() {
    use std::os::unix::fs::PermissionsExt;

    let inbox = temp_path("insecure-inbox");
    fs::write(&inbox, "unchanged\n").unwrap();
    fs::set_permissions(&inbox, fs::Permissions::from_mode(0o666)).unwrap();
    let (mut desktop, events) = desktop_with_pending("insecure-decision", Some(inbox.clone()));

    assert!(desktop
        .decide("pending-fingerprint", Verdict::Deny)
        .is_err());
    assert_eq!(fs::read_to_string(&inbox).unwrap(), "unchanged\n");
    fs::remove_file(inbox).unwrap();
    fs::remove_file(events).unwrap();
}

#[test]
fn dropping_desktop_does_not_write_a_decision() {
    let inbox = temp_path("drop-inbox");
    let (desktop, events) = desktop_with_pending("drop-decision", Some(inbox.clone()));
    drop(desktop);

    assert!(!inbox.exists());
    fs::remove_file(events).unwrap();
}

#[test]
fn engine_and_desktop_share_the_live_approval_path() {
    use dexter_core::{Action, MouseButton, SemanticTarget, Target};
    use dexter_policy::Policy;
    use std::time::Duration;

    let events = temp_path("engine-integration-events");
    let inbox = temp_path("engine-integration-inbox");
    let mut engine = Engine::new(
        SimDriver::new(vec![dexter_core::Element {
            id: dexter_core::ElementId(1),
            role: Some("button".into()),
            name: Some("Guardar".into()),
            enabled: Some(true),
            actions: vec!["press".into()],
            ..Default::default()
        }]),
        Policy::embedded(),
        Duration::from_secs(60),
    );
    engine.set_journal_sink(&events).unwrap();
    engine.set_approval_inbox(inbox.clone()).unwrap();
    let step = Step {
        note: None,
        action: Action::Click {
            target: Target::Semantic(SemanticTarget {
                role: Some("button".into()),
                name: Some("Guardar".into()),
                ..Default::default()
            }),
            button: MouseButton::Left,
        },
        expect: None,
        max_attempts: None,
        app: None,
    };
    assert!(matches!(
        engine.run_step(&step, &RunConfig::default()),
        StepStatus::NeedsApproval { .. }
    ));

    let mut desktop = Desktop::new(Some(events.clone()), None, Some(inbox.clone()));
    let pending = desktop.poll().pending;
    assert_eq!(pending.len(), 1);
    desktop
        .decide(&pending[0].fingerprint, Verdict::Grant)
        .unwrap();
    assert!(engine.run_step(&step, &RunConfig::default()).done());
    assert!(desktop.poll().pending.is_empty());

    fs::remove_file(inbox).unwrap();
    fs::remove_file(events).unwrap();
}
