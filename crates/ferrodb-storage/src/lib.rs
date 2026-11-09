//! # ferrodb-storage
//!
//! The application database `app.db` (SQLite, plan section 5.10):
//!
//! - **query history** — what the user ran, per data source, searchable
//!   (FTS5 trigram index, substring search);
//! - **query log** — every statement sent to a server, including the ones
//!   the IDE issues itself (`SET search_path`, `BEGIN`…), with duration and
//!   outcome.
//!
//! Synchronous API: the core calls it from blocking tasks.

use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ferrodb_api::{DataSourceId, HistoryEntry, HistoryFilter};
use rusqlite::{Connection, OptionalExtension, params};

pub use rusqlite::Error as StorageError;

const SCHEMA_VERSION: i32 = 1;

/// Rows kept per table; older rows are pruned.
const MAX_HISTORY: i64 = 20_000;
const MAX_LOG: i64 = 100_000;

/// Who issued a logged statement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogSource {
    User,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub data_source: DataSourceId,
    pub session: u64,
    pub source: LogSource,
    pub text: String,
    pub started_at: SystemTime,
    pub elapsed: Duration,
    pub rows: Option<u64>,
    /// Error message; `None` on success.
    pub error: Option<String>,
}

pub struct Storage {
    conn: Mutex<Connection>,
    fts: bool,
}

fn millis(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn from_millis(ms: i64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64)
}

impl Storage {
    pub fn open(path: &Path) -> Result<Self, StorageError> {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self::init(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, StorageError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, StorageError> {
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "normal")?;
        conn.busy_timeout(Duration::from_secs(2))?;
        let version: i32 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version < SCHEMA_VERSION {
            conn.execute_batch(
                "create table if not exists history (
                    id integer primary key,
                    data_source text not null,
                    text text not null,
                    executed_at integer not null,
                    elapsed_ms integer not null,
                    succeeded integer not null
                 );
                 create index if not exists history_ds on history (data_source, executed_at);
                 create table if not exists query_log (
                    id integer primary key,
                    data_source text not null,
                    session integer not null,
                    internal integer not null,
                    text text not null,
                    started_at integer not null,
                    elapsed_ms integer not null,
                    rows integer,
                    error text
                 );
                 create index if not exists query_log_time on query_log (started_at);",
            )?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }
        // Substring search; trigram needs SQLite ≥ 3.34 with FTS5 (bundled).
        let fts = conn
            .execute_batch(
                "create virtual table if not exists history_fts using fts5(
                    text, content='history', content_rowid='id', tokenize='trigram');
                 create trigger if not exists history_ai after insert on history begin
                    insert into history_fts(rowid, text) values (new.id, new.text);
                 end;
                 create trigger if not exists history_ad after delete on history begin
                    insert into history_fts(history_fts, rowid, text) values ('delete', old.id, old.text);
                 end;",
            )
            .map_err(|e| tracing::warn!(error = %e, "FTS5 unavailable, history search uses LIKE"))
            .is_ok();
        Ok(Self {
            conn: Mutex::new(conn),
            fts,
        })
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn record_history(&self, e: &HistoryEntry) -> Result<(), StorageError> {
        let c = self.conn();
        c.execute(
            "insert into history (data_source, text, executed_at, elapsed_ms, succeeded)
             values (?1, ?2, ?3, ?4, ?5)",
            params![
                e.data_source.as_str(),
                e.text,
                millis(e.executed_at),
                e.elapsed.as_millis() as i64,
                e.succeeded
            ],
        )?;
        let id = c.last_insert_rowid();
        if id % 500 == 0 {
            c.execute(
                "delete from history where id <= ?1",
                params![id - MAX_HISTORY],
            )?;
        }
        Ok(())
    }

    /// Newest first.
    pub fn history(&self, f: &HistoryFilter) -> Result<Vec<HistoryEntry>, StorageError> {
        let c = self.conn();
        let limit = f.limit.unwrap_or(500) as i64;
        let text = f.text.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let mut sql = String::from(
            "select h.data_source, h.text, h.executed_at, h.elapsed_ms, h.succeeded from history h",
        );
        let mut args: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut conds = Vec::new();
        match text {
            Some(t) if self.fts && t.chars().count() >= 3 => {
                sql.push_str(" join history_fts on history_fts.rowid = h.id");
                conds.push("history_fts match ?".to_owned());
                args.push(Box::new(format!("\"{}\"", t.replace('"', "\"\""))));
            }
            Some(t) => {
                conds.push("h.text like ? escape '\\'".to_owned());
                let escaped = t
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                args.push(Box::new(format!("%{escaped}%")));
            }
            None => {}
        }
        if let Some(ds) = &f.data_source {
            conds.push("h.data_source = ?".to_owned());
            args.push(Box::new(ds.as_str().to_owned()));
        }
        if !conds.is_empty() {
            sql.push_str(" where ");
            sql.push_str(&conds.join(" and "));
        }
        sql.push_str(" order by h.id desc limit ?");
        args.push(Box::new(limit));
        let mut stmt = c.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter().map(|a| a.as_ref())),
            |r| {
                Ok(HistoryEntry {
                    data_source: DataSourceId::new(r.get::<_, String>(0)?),
                    text: r.get(1)?,
                    executed_at: from_millis(r.get(2)?),
                    elapsed: Duration::from_millis(r.get::<_, i64>(3)?.max(0) as u64),
                    succeeded: r.get(4)?,
                })
            },
        )?;
        rows.collect()
    }

    pub fn log(&self, e: &LogEntry) -> Result<(), StorageError> {
        let c = self.conn();
        c.execute(
            "insert into query_log (data_source, session, internal, text, started_at, elapsed_ms, rows, error)
             values (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                e.data_source.as_str(),
                e.session as i64,
                e.source == LogSource::Internal,
                e.text,
                millis(e.started_at),
                e.elapsed.as_millis() as i64,
                e.rows.map(|r| r as i64),
                e.error
            ],
        )?;
        let id = c.last_insert_rowid();
        if id % 1000 == 0 {
            c.execute(
                "delete from query_log where id <= ?1",
                params![id - MAX_LOG],
            )?;
        }
        Ok(())
    }

    /// Newest first.
    pub fn query_log(&self, limit: usize) -> Result<Vec<LogEntry>, StorageError> {
        let c = self.conn();
        let mut stmt = c.prepare(
            "select data_source, session, internal, text, started_at, elapsed_ms, rows, error
             from query_log order by id desc limit ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |r| {
            Ok(LogEntry {
                data_source: DataSourceId::new(r.get::<_, String>(0)?),
                session: r.get::<_, i64>(1)? as u64,
                source: if r.get(2)? {
                    LogSource::Internal
                } else {
                    LogSource::User
                },
                text: r.get(3)?,
                started_at: from_millis(r.get(4)?),
                elapsed: Duration::from_millis(r.get::<_, i64>(5)?.max(0) as u64),
                rows: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                error: r.get(7)?,
            })
        })?;
        rows.collect()
    }

    /// Removes the history of a deleted data source.
    pub fn forget(&self, ds: &DataSourceId) -> Result<(), StorageError> {
        self.conn().execute(
            "delete from history where data_source = ?1",
            params![ds.as_str()],
        )?;
        Ok(())
    }

    /// Number of history rows (diagnostics).
    pub fn history_len(&self) -> Result<u64, StorageError> {
        self.conn()
            .query_row("select count(*) from history", [], |r| r.get::<_, i64>(0))
            .optional()
            .map(|v| v.unwrap_or(0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ds: &str, text: &str, ok: bool) -> HistoryEntry {
        HistoryEntry {
            data_source: ds.into(),
            text: text.into(),
            executed_at: SystemTime::now(),
            elapsed: Duration::from_millis(12),
            succeeded: ok,
        }
    }

    #[test]
    fn history_filters_and_orders() {
        let s = Storage::in_memory().unwrap();
        assert!(s.fts, "bundled SQLite has FTS5");
        s.record_history(&entry("a", "select * from orders", true))
            .unwrap();
        s.record_history(&entry("b", "select * from customers", false))
            .unwrap();
        s.record_history(&entry("a", "update orders set x = 1 -- 100%", true))
            .unwrap();
        let all = s.history(&HistoryFilter::default()).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].text.starts_with("update"), "newest first");
        let f = |ds: Option<&str>, text: Option<&str>| HistoryFilter {
            data_source: ds.map(Into::into),
            text: text.map(Into::into),
            limit: None,
        };
        assert_eq!(s.history(&f(Some("a"), None)).unwrap().len(), 2);
        assert_eq!(s.history(&f(None, Some("orders"))).unwrap().len(), 2);
        assert_eq!(
            s.history(&f(Some("b"), Some("CUSTOM"))).unwrap().len(),
            1,
            "case-insensitive"
        );
        assert_eq!(
            s.history(&f(None, Some("%"))).unwrap().len(),
            1,
            "LIKE escapes"
        );
        assert_eq!(
            s.history(&f(None, Some("\"q"))).unwrap().len(),
            0,
            "FTS quoting"
        );
        s.forget(&"a".into()).unwrap();
        assert_eq!(s.history_len().unwrap(), 1);
    }

    #[test]
    fn query_log_roundtrip_and_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.db");
        {
            let s = Storage::open(&path).unwrap();
            s.log(&LogEntry {
                data_source: "a".into(),
                session: 3,
                source: LogSource::Internal,
                text: "SET search_path TO app".into(),
                started_at: SystemTime::now(),
                elapsed: Duration::from_millis(1),
                rows: None,
                error: None,
            })
            .unwrap();
        }
        let s = Storage::open(&path).unwrap();
        let log = s.query_log(10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].source, LogSource::Internal);
        assert_eq!(log[0].session, 3);
    }
}
