//! `dexter-overlay` — the presence layer.
//!
//! Pure logic lives here: a reducer that folds journal events into a
//! [`PresenceState`] (where the agent cursor is, what it's doing, whether
//! the human still owns input) and a file tailer that yields new events.
//! The platform shell (`main.rs`) renders this state click-through —
//! it never injects or captures input itself.

use dexter_core::{Event, EventKind, Rect};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// What the overlay cursor communicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresenceStatus {
    Idle,
    Observing,
    Thinking,
    Acting,
    /// The agent wants a mutation a human hasn't granted yet.
    WaitingApproval,
    Verifying,
    Retrying,
    Verified,
    /// Policy refused the action — a terminal-looking state.
    Denied,
    Abstained,
    Completed,
    Failed,
}

/// Who owns the physical input right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserControl {
    /// Semantic/visual actions only — the user's pointer is untouched.
    Retained,
    /// A physical-tier action is running — the real cursor may move.
    /// Rendered prominently; this state should be rare and consented.
    Exclusive,
}

/// Everything the overlay needs to draw one frame.
#[derive(Debug, Clone)]
pub struct PresenceState {
    /// Agent identity shown on the cursor tag.
    pub agent: String,
    /// Whether the cursor/tag renders at all.
    pub visible: bool,
    /// What the tag says after the agent name (`"clicking e_4"`).
    pub status_line: String,
    pub status: PresenceStatus,
    /// Where the cursor is headed (screen coords, top-left origin).
    pub cursor: Option<(f64, f64)>,
    /// Lock-on rect around the action's target, when resolvable.
    pub target: Option<Rect>,
    pub user_control: UserControl,
}

impl PresenceState {
    pub fn new(agent: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            visible: false,
            status_line: "idle".into(),
            status: PresenceStatus::Idle,
            cursor: None,
            target: None,
            user_control: UserControl::Retained,
        }
    }
}

/// One journal event folded into presence state.
pub fn reduce(state: &mut PresenceState, ev: &Event) {
    match ev.kind {
        EventKind::ObservationCreated => {
            state.visible = true;
            state.status = PresenceStatus::Observing;
            state.status_line = "observing".into();
        }
        EventKind::CandidatesGenerated => {
            let n = ev.data["count"].as_u64().unwrap_or(0);
            state.status = PresenceStatus::Thinking;
            state.status_line = format!("{n} candidates");
        }
        EventKind::DecisionMade => {
            state.status = PresenceStatus::Thinking;
            let kind = ev.data["decision"]["type"].as_str().unwrap_or("decided");
            state.status_line = kind.replace('_', " ");
        }
        EventKind::ActionProposed => {
            state.visible = true;
            state.status = PresenceStatus::Acting;
            let verb = ev.data["action"]["type"].as_str().unwrap_or("act");
            let physical = ev.data["intrusiveness"].as_str() == Some("physical");
            state.user_control = if physical {
                UserControl::Exclusive
            } else {
                UserControl::Retained
            };
            state.status_line = if physical {
                format!("{verb} — physical input")
            } else {
                verb.replace('_', " ")
            };
            if let Some(r) = ev
                .data
                .get("target_bounds")
                .and_then(|v| serde_json::from_value::<Rect>(v.clone()).ok())
            {
                state.target = Some(r);
                state.cursor = Some((r.x + r.w / 2.0, r.y + r.h / 2.0));
            }
        }
        EventKind::HumanApprovalRequired => {
            state.status = PresenceStatus::WaitingApproval;
            state.status_line = "waiting for approval".into();
        }
        EventKind::PolicyChecked => {
            if ev.data["decision"].as_str() == Some("deny") {
                state.status = PresenceStatus::Denied;
                state.status_line = "denied by policy".into();
            }
        }
        EventKind::ActionExecuted => {
            let mechanism = ev.data["mechanism"].as_str().unwrap_or("act");
            state.status = PresenceStatus::Verifying;
            state.status_line = format!("acted · {}", mechanism.to_lowercase());
        }
        EventKind::RecoveryStarted => {
            let n = ev.data["attempt"].as_u64().unwrap_or(1);
            state.status = PresenceStatus::Retrying;
            state.status_line = format!("retry {n}");
        }
        EventKind::VerificationPassed => {
            state.status = PresenceStatus::Verified;
            state.status_line = "verified".into();
        }
        EventKind::VerificationFailed | EventKind::ActionFailed => {
            state.status = PresenceStatus::Retrying;
            state.status_line = "verify failed".into();
        }
        EventKind::RecoveryCompleted | EventKind::StateChanged => {}
        EventKind::TaskCompleted => {
            state.status = PresenceStatus::Completed;
            state.status_line = "done".into();
            state.target = None;
        }
        EventKind::TaskFailed => {
            let outcome = ev.data["outcome"].as_str().unwrap_or("failed");
            state.target = None;
            match outcome {
                "abstain" => {
                    state.status = PresenceStatus::Abstained;
                    state.status_line = "abstained".into();
                }
                "escalated" => {
                    state.status = PresenceStatus::WaitingApproval;
                    state.status_line = "needs a human".into();
                }
                _ => {
                    state.status = PresenceStatus::Failed;
                    state.status_line = "failed".into();
                }
            }
        }
        EventKind::TaskCancelled => {
            state.status = PresenceStatus::Abstained;
            state.status_line = "cancelled".into();
            state.target = None;
        }
        EventKind::TaskTimedOut => {
            state.status = PresenceStatus::Failed;
            state.status_line = "timed out".into();
            state.target = None;
        }
    }
}

/// Tail a journal JSONL file, yielding only newly appended events.
/// The writer appends one JSON event per line; partial trailing lines
/// (writer mid-flush) are retried on the next poll.
pub struct JournalTail {
    offset: u64,
}

impl JournalTail {
    /// Start reading at the current end — live overlay mode.
    pub fn live(path: &Path) -> std::io::Result<Self> {
        let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        Ok(Self { offset: len })
    }

    /// Start at byte 0 — replay mode for demos and tests.
    pub fn replay() -> Self {
        Self { offset: 0 }
    }

    /// Parse any complete lines appended since the last poll.
    pub fn poll(&mut self, path: &Path) -> Vec<Event> {
        let Ok(mut f) = File::open(path) else {
            return Vec::new(); // journal may not exist yet — wait
        };
        let Ok(len) = f.metadata().map(|m| m.len()) else {
            return Vec::new();
        };
        if len <= self.offset {
            return Vec::new();
        }
        let mut buf = String::new();
        if f.seek(SeekFrom::Start(self.offset)).is_err() || f.read_to_string(&mut buf).is_err() {
            return Vec::new();
        }
        // Consume only complete lines; keep the partial tail for next poll.
        let complete = buf.rfind('\n').map(|i| i + 1).unwrap_or(0);
        self.offset += complete as u64;
        buf[..complete]
            .lines()
            .filter_map(|l| serde_json::from_str::<Event>(l).ok())
            .collect()
    }
}
