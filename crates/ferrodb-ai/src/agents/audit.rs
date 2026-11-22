//! The audit log of agent calls: `<data_dir>/agents-audit.jsonl` (one JSON
//! object per line), or a ring of the last entries in memory. Entries hold
//! the query text, never result data.

use std::{collections::VecDeque, io::Write as _, path::PathBuf, sync::Mutex};

use ferrodb_api::{AuditEntry, Error, Result};

pub const FILE_NAME: &str = "agents-audit.jsonl";
/// Entries kept when there is no data directory.
const RING: usize = 1000;

pub struct AuditLog {
    path: Option<PathBuf>,
    ring: Mutex<VecDeque<AuditEntry>>,
}

impl AuditLog {
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        Self {
            path: data_dir.map(|d| d.join(FILE_NAME)),
            ring: Mutex::new(VecDeque::new()),
        }
    }

    pub fn append(&self, entry: &AuditEntry) {
        let Some(path) = &self.path else {
            let mut ring = self.ring.lock().unwrap();
            if ring.len() == RING {
                ring.pop_front();
            }
            ring.push_back(entry.clone());
            return;
        };
        // The lock also serializes appends to the file.
        let _guard = self.ring.lock().unwrap();
        let write = || -> std::io::Result<()> {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let mut line = serde_json::to_string(entry).map_err(std::io::Error::other)?;
            line.push('\n');
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?
                .write_all(line.as_bytes())
        };
        if let Err(e) = write() {
            tracing::warn!(error = %e, path = %path.display(), "cannot write the agents audit log");
        }
    }

    /// The last `limit` entries, newest first.
    pub fn read(&self, limit: usize) -> Result<Vec<AuditEntry>> {
        let Some(path) = &self.path else {
            let ring = self.ring.lock().unwrap();
            return Ok(ring.iter().rev().take(limit).cloned().collect());
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Other(format!("read {}: {e}", path.display()))),
        };
        Ok(text
            .lines()
            .rev()
            .filter(|l| !l.trim().is_empty())
            // A line cut short by a crash is skipped, not fatal.
            .filter_map(|l| serde_json::from_str(l).ok())
            .take(limit)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use ferrodb_api::{AuditOutcome, DataSourceId};

    use super::*;

    fn entry(i: u64) -> AuditEntry {
        AuditEntry {
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + i),
            client: "claude-code".into(),
            tool: "run_query".into(),
            data_source: Some(DataSourceId::new("shop-dev")),
            text: Some(format!("select {i}")),
            outcome: AuditOutcome::AllowedByUser,
            items: Some(i),
            error: None,
            elapsed_ms: Some(3),
        }
    }

    #[test]
    fn file_round_trip_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let log = AuditLog::new(Some(dir.path().join("data")));
        assert!(log.read(10).unwrap().is_empty());
        for i in 0..5 {
            log.append(&entry(i));
        }
        // A new instance (next start of the IDE) reads the same file.
        let again = AuditLog::new(Some(dir.path().join("data")));
        let got = again.read(3).unwrap();
        assert_eq!(got, vec![entry(4), entry(3), entry(2)]);
        assert_eq!(again.read(100).unwrap().len(), 5);
        let text = std::fs::read_to_string(dir.path().join("data").join(FILE_NAME)).unwrap();
        assert_eq!(text.lines().count(), 5);
    }

    #[test]
    fn memory_ring_keeps_the_last_entries() {
        let log = AuditLog::new(None);
        for i in 0..(RING as u64 + 10) {
            log.append(&entry(i));
        }
        let got = log.read(usize::MAX).unwrap();
        assert_eq!(got.len(), RING);
        assert_eq!(got[0], entry(RING as u64 + 9));
        assert_eq!(got.last().unwrap(), &entry(10));
    }
}
