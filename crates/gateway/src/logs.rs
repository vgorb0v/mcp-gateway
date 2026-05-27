use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::Mutex;
use serde::Serialize;

use crate::config::LimitsConfig;
use crate::security::Redactor;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub timestamp_ms: u128,
    pub stream: String,
    pub line: String,
}

#[derive(Clone)]
pub struct LogStore {
    inner: Arc<Mutex<BTreeMap<String, VecDeque<LogEntry>>>>,
    redactor: Redactor,
    capacity: usize,
    max_line_bytes: usize,
}

impl LogStore {
    pub fn new(redactor: Redactor) -> Self {
        Self::with_limits(redactor, &LimitsConfig::default())
    }

    pub fn with_limits(redactor: Redactor, limits: &LimitsConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BTreeMap::new())),
            redactor,
            capacity: limits.max_log_entries_per_server,
            max_line_bytes: limits.max_log_line_bytes,
        }
    }

    pub fn append(&self, server: &str, stream: &str, line: impl AsRef<str>) {
        // Do all the expensive work (redaction allocation, UTF-8 truncation,
        // timestamp syscall) *before* taking the lock. The critical section is
        // now just two BTreeMap/VecDeque operations.
        let redacted = self.redactor.redact(line.as_ref());
        let line = truncate_utf8(&redacted, self.max_line_bytes);
        let timestamp_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let entry = LogEntry {
            timestamp_ms,
            stream: stream.to_string(),
            line,
        };

        let mut logs = self.inner.lock();
        let ring = logs.entry(server.to_string()).or_default();
        if ring.len() == self.capacity {
            ring.pop_front();
        }
        ring.push_back(entry);
    }

    pub fn get(&self, server: &str) -> Vec<LogEntry> {
        self.inner
            .lock()
            .get(server)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

impl Default for LogStore {
    fn default() -> Self {
        Self::new(Redactor::default())
    }
}
