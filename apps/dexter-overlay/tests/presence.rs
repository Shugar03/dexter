//! The presence contract: journal events fold into what the overlay
//! draws — cursor position, status label, and who owns physical input.

use dexter_core::{Event, EventKind, Rect};
use dexter_overlay::{reduce, JournalTail, PresenceState, PresenceStatus, UserControl};

fn ev(kind: EventKind, data: serde_json::Value) -> Event {
    Event::new(kind, data)
}

#[test]
fn action_proposed_moves_cursor_to_target_center() {
    let mut s = PresenceState::new("dexter");
    reduce(
        &mut s,
        &ev(
            EventKind::ActionProposed,
            serde_json::json!({
                "action": {"type": "click"},
                "intrusiveness": "background",
                "target_bounds": {"x": 100.0, "y": 40.0, "w": 80.0, "h": 20.0},
            }),
        ),
    );
    assert!(s.visible);
    assert_eq!(s.status, PresenceStatus::Acting);
    assert_eq!(s.user_control, UserControl::Retained);
    assert_eq!(s.cursor, Some((140.0, 50.0)), "center of the target rect");
    assert_eq!(
        s.target,
        Some(Rect {
            x: 100.0,
            y: 40.0,
            w: 80.0,
            h: 20.0
        })
    );
}

#[test]
fn fast_act_keeps_cursor_on_target_after_terminal() {
    // A single click journals proposed+completed within one overlay
    // poll — the cursor must still land on the button it pressed.
    let mut s = PresenceState::new("dexter");
    for e in [
        ev(
            EventKind::ActionProposed,
            serde_json::json!({
                "action": {"type": "click"},
                "intrusiveness": "background",
                "target_bounds": {"x": 295.0, "y": 666.0, "w": 48.0, "h": 48.0},
            }),
        ),
        ev(EventKind::TaskCompleted, serde_json::json!({"steps": 1})),
    ] {
        reduce(&mut s, &e);
    }
    assert_eq!(s.cursor, Some((319.0, 690.0)));
}

#[test]
fn degenerate_bounds_do_not_move_cursor() {
    // Menubar items report 0×0 rects at the screen edge — locking onto
    // them parks the cursor in a corner where nobody sees it.
    let mut s = PresenceState::new("dexter");
    reduce(
        &mut s,
        &ev(
            EventKind::ActionProposed,
            serde_json::json!({
                "action": {"type": "click"},
                "intrusiveness": "background",
                "target_bounds": {"x": 0.0, "y": 956.0, "w": 0.0, "h": 0.0},
            }),
        ),
    );
    assert!(s.cursor.is_none());
    assert!(s.target.is_none());
}

#[test]
fn physical_action_marks_exclusive_control() {
    // The one state the user must notice: the agent touching real input.
    let mut s = PresenceState::new("dexter");
    reduce(
        &mut s,
        &ev(
            EventKind::ActionProposed,
            serde_json::json!({
                "action": {"type": "key", "chord": {"key": "s", "modifiers": ["cmd"]}},
                "intrusiveness": "physical",
                "target_bounds": null,
            }),
        ),
    );
    assert_eq!(s.user_control, UserControl::Exclusive);
    assert!(s.status_line.contains("physical"));
    assert!(s.cursor.is_none(), "no bounds — nothing to lock onto");
}

#[test]
fn lifecycle_reaches_terminal_states() {
    let mut s = PresenceState::new("dexter");
    reduce(
        &mut s,
        &ev(EventKind::ObservationCreated, serde_json::json!({})),
    );
    assert_eq!(s.status, PresenceStatus::Observing);
    reduce(
        &mut s,
        &ev(EventKind::VerificationPassed, serde_json::json!({})),
    );
    assert_eq!(s.status, PresenceStatus::Verified);
    reduce(&mut s, &ev(EventKind::TaskCompleted, serde_json::json!({})));
    assert_eq!(s.status, PresenceStatus::Completed);
    assert_eq!(s.target, None, "terminal state drops the lock-on rect");
}

#[test]
fn abstain_is_distinct_from_failure() {
    let mut s = PresenceState::new("dexter");
    reduce(
        &mut s,
        &ev(
            EventKind::TaskFailed,
            serde_json::json!({"outcome": "abstain"}),
        ),
    );
    assert_eq!(s.status, PresenceStatus::Abstained);

    let mut s2 = PresenceState::new("dexter");
    reduce(
        &mut s2,
        &ev(
            EventKind::TaskFailed,
            serde_json::json!({"outcome": "max_steps"}),
        ),
    );
    assert_eq!(s2.status, PresenceStatus::Failed);
}

#[test]
fn policy_deny_and_approval_wait_are_visible() {
    let mut s = PresenceState::new("dexter");
    reduce(
        &mut s,
        &ev(
            EventKind::HumanApprovalRequired,
            serde_json::json!({"fingerprint": "x"}),
        ),
    );
    assert_eq!(s.status, PresenceStatus::WaitingApproval);
    reduce(
        &mut s,
        &ev(
            EventKind::PolicyChecked,
            serde_json::json!({"decision": "deny", "intrusiveness": "physical"}),
        ),
    );
    assert_eq!(s.status, PresenceStatus::Denied);
}

#[test]
fn journal_tail_yields_only_appended_events() {
    let dir = std::env::temp_dir().join(format!("dexter-tail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("j.jsonl");

    let e1 =
        serde_json::to_string(&ev(EventKind::ObservationCreated, serde_json::json!({}))).unwrap();
    std::fs::write(&path, format!("{e1}\n")).unwrap();

    // Live mode starts at end-of-file: the historical line is skipped.
    let mut tail = JournalTail::live(&path).unwrap();
    assert!(tail.poll(&path).is_empty());

    // Appended lines arrive; a partial line waits for its newline.
    let e2 = serde_json::to_string(&ev(EventKind::TaskCompleted, serde_json::json!({}))).unwrap();
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    write!(f, "{e2}").unwrap(); // no trailing newline yet
    f.flush().unwrap();
    assert!(tail.poll(&path).is_empty(), "partial line not consumed");
    writeln!(f).unwrap();
    drop(f);
    let got = tail.poll(&path);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].kind, EventKind::TaskCompleted);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replay_reads_from_the_start() {
    let dir = std::env::temp_dir().join(format!("dexter-replay-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("j.jsonl");
    let e =
        serde_json::to_string(&ev(EventKind::ObservationCreated, serde_json::json!({}))).unwrap();
    std::fs::write(&path, format!("{e}\n")).unwrap();

    let mut tail = JournalTail::replay();
    assert_eq!(tail.poll(&path).len(), 1);
    assert!(tail.poll(&path).is_empty(), "second poll sees nothing new");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cancelled_and_timed_out_tasks_clear_presence() {
    use dexter_overlay::reduce;
    let mut s = PresenceState::new("dexter");
    s.status = PresenceStatus::Acting;
    s.target = Some(dexter_core::Rect {
        x: 10.0,
        y: 10.0,
        w: 5.0,
        h: 5.0,
    });
    reduce(&mut s, &ev(EventKind::TaskCancelled, serde_json::json!({})));
    assert_eq!(s.status, PresenceStatus::Abstained);
    assert!(s.target.is_none());
    reduce(&mut s, &ev(EventKind::TaskTimedOut, serde_json::json!({})));
    assert_eq!(s.status, PresenceStatus::Failed);
    assert_eq!(s.status_line, "timed out");
}
