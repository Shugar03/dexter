//! Read-only desktop snapshot of Dexter's journal and policy state.

use dexter_core::{Event, JournalTail};
use dexter_policy::{Policy, PolicySummary};
use serde::Serialize;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const JOURNAL_CAP: usize = 200;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct EventRow {
    pub ts_ms: u64,
    pub kind: String,
    pub summary: String,
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

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub journal: JournalStatus,
    pub events: Vec<EventRow>,
    pub policy: PolicyView,
}

pub struct Desktop {
    events_path: Option<PathBuf>,
    tail: JournalTail,
    ring: VecDeque<EventRow>,
    policy: PolicyView,
}

impl Desktop {
    pub fn new(events: Option<PathBuf>, policy: Option<PathBuf>) -> Self {
        Self {
            events_path: events,
            tail: JournalTail::replay(),
            ring: VecDeque::new(),
            policy: load_policy_view(policy.as_deref()),
        }
    }

    pub fn poll(&mut self) -> Snapshot {
        let journal = match self.events_path.as_deref() {
            None => JournalStatus::NotConfigured,
            Some(path) if !path.is_file() => JournalStatus::Waiting {
                path: path.display().to_string(),
            },
            Some(path) => {
                for event in self.tail.poll(path) {
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
        }
    }
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
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mut parsed = Args {
        events: None,
        policy: None,
    };

    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--events" | "--policy" => {
                let value = args
                    .next()
                    .filter(|value| !value.starts_with("--"))
                    .ok_or_else(|| format!("{flag} requires a path"))?;
                let path = PathBuf::from(value);
                if flag == "--events" {
                    parsed.events = Some(path);
                } else {
                    parsed.policy = Some(path);
                }
            }
            other => return Err(format!("unknown flag '{other}'")),
        }
    }

    Ok(parsed)
}
