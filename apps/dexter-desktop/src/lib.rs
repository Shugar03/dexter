//! Dexter desktop journal, policy, approvals, and read-only session views.

use dexter_core::{Event, EventKind, JournalTail};
use dexter_eval::recording::{events_from_jsonl, Recording, StepResult};
use dexter_policy::{Policy, PolicySummary};
use serde::Serialize;
use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const JOURNAL_CAP: usize = 200;
const SESSION_ENTRY_CAP: usize = 200;
const SESSION_SIZE_CAP: u64 = 16 * 1024 * 1024;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct EventRow {
    pub ts_ms: u64,
    pub kind: String,
    pub summary: String,
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionsView {
    NotConfigured,
    Ready {
        path: String,
        entries: Vec<SessionEntry>,
        truncated: bool,
    },
    Error {
        path: String,
        message: String,
    },
}

#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct SessionEntry {
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub modified_ms: u64,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionDetail {
    Recording {
        name: String,
        scenario: String,
        goal: String,
        outcome: String,
        steps: Vec<StepRow>,
    },
    Journal {
        name: String,
        events: Vec<EventRow>,
    },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct StepRow {
    pub index: usize,
    pub action_json: serde_json::Value,
    pub app_json: serde_json::Value,
    pub observes_before: u32,
    pub result: StepResult,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PolicyView {
    Embedded {
        summary: PolicySummary,
    },
    File {
        path: String,
        summary: PolicySummary,
    },
    Error {
        path: String,
        message: String,
    },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum JournalStatus {
    NotConfigured,
    Waiting { path: String },
    Live { path: String },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ApprovalChannel {
    Disabled,
    Enabled { path: String },
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct PendingApproval {
    pub fingerprint: String,
    pub reason: String,
    pub ts_ms: u64,
    pub sent: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    Grant,
    Deny,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub journal: JournalStatus,
    pub events: Vec<EventRow>,
    pub policy: PolicyView,
    pub approvals: ApprovalChannel,
    pub pending: Vec<PendingApproval>,
}

pub struct Desktop {
    events_path: Option<PathBuf>,
    approvals_path: Option<PathBuf>,
    sessions_path: Option<PathBuf>,
    tail: JournalTail,
    ring: VecDeque<EventRow>,
    policy: PolicyView,
    pending: BTreeMap<String, PendingApproval>,
}

impl Desktop {
    pub fn new(
        events: Option<PathBuf>,
        policy: Option<PathBuf>,
        approvals: Option<PathBuf>,
    ) -> Self {
        Self::new_with_sessions(events, policy, approvals, None)
    }

    pub fn new_with_sessions(
        events: Option<PathBuf>,
        policy: Option<PathBuf>,
        approvals: Option<PathBuf>,
        sessions: Option<PathBuf>,
    ) -> Self {
        Self {
            events_path: events,
            approvals_path: approvals,
            sessions_path: sessions,
            tail: JournalTail::replay(),
            ring: VecDeque::new(),
            policy: load_policy_view(policy.as_deref()),
            pending: BTreeMap::new(),
        }
    }

    pub fn sessions(&self) -> SessionsView {
        let Some(path) = self.sessions_path.as_deref() else {
            return SessionsView::NotConfigured;
        };
        let entries = match fs::read_dir(path) {
            Ok(entries) => entries,
            Err(error) => {
                return SessionsView::Error {
                    path: path.display().to_string(),
                    message: error.to_string(),
                }
            }
        };

        let mut sessions = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    return SessionsView::Error {
                        path: path.display().to_string(),
                        message: error.to_string(),
                    }
                }
            };
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) => continue,
            };
            let kind = match Path::new(&name).extension().and_then(|ext| ext.to_str()) {
                Some("jsonl") => "journal",
                Some("json") => "recording",
                _ => continue,
            };
            let metadata = match fs::symlink_metadata(entry.path()) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return SessionsView::Error {
                        path: path.display().to_string(),
                        message: error.to_string(),
                    }
                }
            };
            if !metadata.file_type().is_file() {
                continue;
            }
            let modified_ms = metadata
                .modified()
                .ok()
                .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
                .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or_default();
            sessions.push(SessionEntry {
                name,
                kind: kind.to_string(),
                bytes: metadata.len(),
                modified_ms,
            });
        }

        sessions.sort_by(|left, right| {
            right
                .modified_ms
                .cmp(&left.modified_ms)
                .then_with(|| left.name.cmp(&right.name))
        });
        let truncated = sessions.len() > SESSION_ENTRY_CAP;
        sessions.truncate(SESSION_ENTRY_CAP);
        SessionsView::Ready {
            path: path.display().to_string(),
            entries: sessions,
            truncated,
        }
    }

    pub fn open_session(&self, name: &str) -> Result<SessionDetail, String> {
        let directory = self
            .sessions_path
            .as_deref()
            .ok_or_else(|| "sessions directory is not configured".to_string())?;
        if name.is_empty()
            || matches!(name, "." | "..")
            || name.contains('/')
            || name.contains('\\')
            || name.contains('\0')
        {
            return Err("session name must be a bare file name".into());
        }
        let kind = match Path::new(name).extension().and_then(|ext| ext.to_str()) {
            Some("json") => "recording",
            Some("jsonl") => "journal",
            _ => return Err("session name must end in .json or .jsonl".into()),
        };
        let path = directory.join(name);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("cannot inspect session '{}': {error}", path.display()))?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "session '{}' is not a regular file",
                path.display()
            ));
        }
        if metadata.len() > SESSION_SIZE_CAP {
            return Err(format!(
                "session '{}' exceeds the 16 MiB size limit",
                path.display()
            ));
        }
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read session '{}': {error}", path.display()))?;

        if kind == "recording" {
            let recording: Recording = serde_json::from_str(&text)
                .map_err(|error| format!("cannot parse recording '{}': {error}", path.display()))?;
            let steps = recording
                .steps
                .into_iter()
                .enumerate()
                .map(|(index, step)| {
                    Ok(StepRow {
                        index: index + 1,
                        action_json: serde_json::to_value(step.action).map_err(|error| {
                            format!("cannot serialize recorded action: {error}")
                        })?,
                        app_json: serde_json::to_value(step.app)
                            .map_err(|error| format!("cannot serialize recorded app: {error}"))?,
                        observes_before: step.observes_before,
                        result: step.result,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(SessionDetail::Recording {
                name: name.to_string(),
                scenario: recording.scenario,
                goal: recording.goal,
                outcome: recording.outcome,
                steps,
            })
        } else {
            let events = events_from_jsonl(&text)
                .map_err(|error| format!("cannot parse journal '{}': {error}", path.display()))?;
            Ok(SessionDetail::Journal {
                name: name.to_string(),
                events: events.into_iter().map(EventRow::from).collect(),
            })
        }
    }

    pub fn decide(&mut self, fingerprint: &str, verdict: Verdict) -> Result<(), String> {
        let path = self
            .approvals_path
            .as_deref()
            .ok_or_else(|| "approval channel is disabled".to_string())?;
        let pending = self
            .pending
            .get(fingerprint)
            .ok_or_else(|| "fingerprint is not currently pending".to_string())?;
        if pending.sent.is_some() {
            return Err("a decision has already been sent for this fingerprint".into());
        }

        match fs::metadata(path) {
            Ok(metadata) => check_inbox_permissions(path, &metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "cannot inspect approval inbox '{}': {error}",
                    path.display()
                ))
            }
        }

        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(path)
            .map_err(|error| format!("cannot open approval inbox '{}': {error}", path.display()))?;
        let metadata = file.metadata().map_err(|error| {
            format!(
                "cannot inspect approval inbox '{}': {error}",
                path.display()
            )
        })?;
        check_inbox_permissions(path, &metadata)?;

        let decision = match verdict {
            Verdict::Grant => "grant",
            Verdict::Deny => "deny",
        };
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        let mut line = serde_json::to_vec(&serde_json::json!({
            "fingerprint": fingerprint,
            "decision": decision,
            "ts_ms": ts_ms,
        }))
        .map_err(|error| format!("cannot serialize approval decision: {error}"))?;
        line.push(b'\n');
        file.write_all(&line).map_err(|error| {
            format!(
                "cannot append approval decision to '{}': {error}",
                path.display()
            )
        })?;
        self.pending
            .get_mut(fingerprint)
            .expect("pending approval checked before writing")
            .sent = Some(decision.to_string());
        Ok(())
    }

    pub fn poll(&mut self) -> Snapshot {
        let journal = match self.events_path.clone() {
            None => JournalStatus::NotConfigured,
            Some(path) if !path.is_file() => JournalStatus::Waiting {
                path: path.display().to_string(),
            },
            Some(path) => {
                for event in self.tail.poll(&path) {
                    self.track_approval(&event);
                    self.ring.push_back(EventRow::from(event));
                    if self.ring.len() > JOURNAL_CAP {
                        self.ring.pop_front();
                    }
                }
                JournalStatus::Live {
                    path: path.display().to_string(),
                }
            }
        };

        Snapshot {
            journal,
            events: self.ring.iter().cloned().collect(),
            policy: self.policy.clone(),
            approvals: match self.approvals_path.as_deref() {
                None => ApprovalChannel::Disabled,
                Some(path) => ApprovalChannel::Enabled {
                    path: path.display().to_string(),
                },
            },
            pending: self.pending.values().cloned().collect(),
        }
    }

    fn track_approval(&mut self, event: &Event) {
        match event.kind {
            EventKind::HumanApprovalRequired if event.data.get("granted").is_none() => {
                let Some(fingerprint) = event.data["fingerprint"].as_str() else {
                    return;
                };
                let timestamp = EventRow::from(event.clone()).ts_ms;
                self.pending.insert(
                    fingerprint.to_string(),
                    PendingApproval {
                        fingerprint: fingerprint.to_string(),
                        reason: event.data["reason"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                        ts_ms: timestamp,
                        sent: None,
                    },
                );
            }
            EventKind::PolicyChecked => {
                let decision = event.data["decision"].as_str().unwrap_or_default();
                if matches!(
                    decision,
                    "granted" | "grant_rejected" | "denied_by_human" | "deny_rejected" | "approved"
                ) {
                    if let Some(fingerprint) = event.data["fingerprint"].as_str() {
                        self.pending.remove(fingerprint);
                    }
                }
            }
            _ => {}
        }
    }
}

fn check_inbox_permissions(path: &Path, metadata: &fs::Metadata) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "approval inbox '{}' is group/world-writable",
                path.display()
            ));
        }
    }
    Ok(())
}

impl From<Event> for EventRow {
    fn from(event: Event) -> Self {
        let ts_ms = event
            .ts
            .duration_since(UNIX_EPOCH)
            .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        let kind = serde_json::to_value(event.kind)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{:?}", event.kind));
        let serialized = serde_json::to_string(&event.data).unwrap_or_default();
        let summary = if serialized.chars().count() > 160 {
            let mut summary = serialized.chars().take(157).collect::<String>();
            summary.push_str("...");
            summary
        } else {
            serialized
        };

        Self {
            ts_ms,
            kind,
            summary,
        }
    }
}

pub fn load_policy_view(path: Option<&Path>) -> PolicyView {
    let Some(path) = path else {
        return PolicyView::Embedded {
            summary: Policy::embedded().summary(),
        };
    };

    match Policy::load(path) {
        Ok(policy) => PolicyView::File {
            path: path.display().to_string(),
            summary: policy.summary(),
        },
        Err(error) => PolicyView::Error {
            path: path.display().to_string(),
            message: error.to_string(),
        },
    }
}

pub struct Args {
    pub events: Option<PathBuf>,
    pub policy: Option<PathBuf>,
    pub approvals: Option<PathBuf>,
    pub sessions: Option<PathBuf>,
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut parsed = Args {
        events: None,
        policy: None,
        approvals: None,
        sessions: None,
    };

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--events" | "--policy" | "--approvals" | "--sessions" => {
                let value = args
                    .next()
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| format!("{flag} requires a path"))?;
                let path = PathBuf::from(value);
                if flag == "--events" {
                    parsed.events = Some(path);
                } else if flag == "--policy" {
                    parsed.policy = Some(path);
                } else if flag == "--approvals" {
                    parsed.approvals = Some(path);
                } else {
                    parsed.sessions = Some(path);
                }
            }
            other => return Err(format!("unknown flag '{other}'")),
        }
    }

    Ok(parsed)
}
