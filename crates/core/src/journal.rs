use crate::Event;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

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
        if len < self.offset {
            self.offset = 0; // journal was truncated/rotated — reread
        }
        if len == self.offset {
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
