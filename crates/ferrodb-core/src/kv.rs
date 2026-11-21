//! Key-value sources (Redis) and Cassandra specifics of the core.
//!
//! - console commands that list keys (`SCAN 0 MATCH p COUNT n`, `KEYS p`)
//!   become key browser results: the driver's native scan streams key
//!   rows, the session forwards them as [`ResultChunk::Keys`] with the
//!   usual paging (a paused result stops the scan through backpressure);
//! - [`QueryService::key_value`](ferrodb_api::QueryService::key_value) and
//!   key edits run on a pooled connection switched to the session's
//!   database, so a paused key listing (which keeps the session's
//!   connection busy) does not block them;
//! - the explorer lists databases → key prefixes → keys live, with a
//!   capped scan per expansion (`ferrodb_meta::redis`);
//! - risks: dangerous Redis commands and CQL `DROP` / `TRUNCATE` /
//!   unrestricted `DELETE` ask for confirmation like SQL; CQL `ALLOW
//!   FILTERING` gives a warning before the statement runs.

use std::sync::Arc;

use arrow_array::RecordBatch;
use ferrodb_api::{
    ApplyReport, Change, ChangeSet, DataSourceId, DbObject, Error, KeyEntry, KeyType, KeyValue,
    NodeRef, ObjectKind, Result, ResultChunk, ResultId, Risk, RiskKind, SessionId,
};
use ferrodb_drivers::{
    CancellationToken, Connection, Request,
    kv::{self, CommandClass, method},
    sink::{collect, collect_request},
};
use ferrodb_lang::sql::{StatementClass, TxCommand};
use ferrodb_meta::redis::{KeyNode, prefix_children};

use crate::Inner;

/// Elements read per key by the key browser.
pub(crate) const VALUE_LIMIT: usize = 10_000;

/// A key listing result: the session it belongs to (its database).
#[derive(Clone, Debug)]
pub(crate) struct KeyResult {
    pub session: SessionId,
    pub data_source: DataSourceId,
    /// The session's database when the listing ran.
    pub database: Option<String>,
}

/// Key entries of a streamed key listing batch.
pub(crate) fn keys_chunk(b: &RecordBatch) -> ResultChunk {
    ResultChunk::Keys(Arc::new(kv::keys_from_batch(b)))
}

/// What a console line of a Redis source runs.
// Built once per console line and consumed right away: its size is irrelevant.
#[allow(clippy::large_enum_variant)]
pub(crate) enum RedisLine {
    /// A key listing for the key browser: (pattern, request).
    Keys(String, ferrodb_drivers::Request),
    /// Any other command.
    Command(CommandClass),
}

/// Parses and classifies one console line.
pub(crate) fn redis_line(line: &str) -> std::result::Result<RedisLine, String> {
    let args = kv::split_args(line)?;
    if args.is_empty() {
        return Err("empty command".into());
    }
    if let Some((pattern, count, kind)) = kv::key_listing(&args) {
        let count = count.unwrap_or(500).clamp(10, 10_000).to_string();
        let mut a: Vec<(&str, &str)> = vec![("pattern", &pattern), ("count", &count)];
        if let Some(k) = &kind {
            a.push(("type", k));
        }
        let req = Request::native(method::SCAN, &a);
        return Ok(RedisLine::Keys(pattern, req));
    }
    Ok(RedisLine::Command(kv::classify(&args)))
}

/// Risks of a Redis script: dangerous commands always, writes on
/// production sources (`all_modifications`).
pub(crate) fn redis_risks(text: &str, all_modifications: bool) -> Vec<Risk> {
    kv::split_commands(text)
        .into_iter()
        .filter_map(|r| {
            let args = kv::split_args(&text[r.clone()]).ok()?;
            let name = args.first()?.to_ascii_uppercase();
            let kind = match kv::classify(&args) {
                CommandClass::Dangerous if name.starts_with("FLUSH") => RiskKind::Truncate,
                CommandClass::Dangerous => RiskKind::Drop,
                CommandClass::Write if all_modifications => RiskKind::Modification,
                _ => return None,
            };
            Some(Risk { kind, range: r })
        })
        .collect()
}

/// Statement class of CQL (`BEGIN BATCH` is one modifying statement, not
/// a transaction; `LIST` / `DESCRIBE` read).
pub(crate) fn cql_class(stmt: &str) -> StatementClass {
    let first = stmt
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match first.as_str() {
        "select" | "describe" | "desc" | "list" => StatementClass::Read,
        "use" | "consistency" | "serial" => StatementClass::Session,
        "begin" | "apply" => StatementClass::Modification,
        "" => StatementClass::Session,
        _ => match ferrodb_lang::sql::classify(stmt, "cql") {
            StatementClass::Transaction(TxCommand::Begin) => StatementClass::Modification,
            c => c,
        },
    }
}

/// The warning shown before a CQL statement with `ALLOW FILTERING`.
pub(crate) fn allow_filtering_warning(stmt: &str) -> Option<String> {
    let words: Vec<String> = stmt
        .split(|c: char| c.is_whitespace() || c == ';')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    let has = words
        .windows(2)
        .any(|w| w[0] == "allow" && w[1] == "filtering");
    has.then(|| {
        "ALLOW FILTERING: Cassandra may scan every partition of the table (and every node) to \
         answer this query; on large tables it is slow and can time out. Prefer a query by \
         partition key, a secondary / SAI index or a materialized view."
            .to_owned()
    })
}

impl Inner {
    pub(crate) fn register_keys(&self, rid: ResultId, r: KeyResult) {
        self.state().keys.insert(rid, r);
    }

    pub(crate) fn is_key_result(&self, rid: ResultId) -> bool {
        self.state().keys.contains_key(&rid)
    }

    fn key_result(&self, rid: ResultId) -> Result<KeyResult> {
        self.state()
            .keys
            .get(&rid)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("result {rid}")))
    }

    /// A pooled connection on `database`; give it back with `checkin`.
    async fn kv_connection(
        &self,
        ds: &DataSourceId,
        database: Option<&str>,
    ) -> Result<Box<dyn Connection>> {
        let mut conn = self.checkout(ds).await?;
        if let Some(db) = database.filter(|d| !d.is_empty()) {
            conn.set_schema(db).await?;
        }
        Ok(conn)
    }

    /// Value of one key in the session's database.
    pub(crate) async fn key_value_of(&self, session: SessionId, key: String) -> Result<KeyValue> {
        let (ds, db) = self.session_place(session)?;
        let mut conn = self.kv_connection(&ds, db.as_deref()).await?;
        let limit = VALUE_LIMIT.to_string();
        let out = collect_request(
            conn.as_mut(),
            Request::native(method::VALUE, &[("key", &key), ("limit", &limit)]),
            CancellationToken::new(),
        )
        .await;
        self.checkin(&ds, conn);
        kv::value_from_results(&out?.results, None)
    }

    /// Commands for key changes (`TYPE` is read for member edits).
    async fn key_commands(
        &self,
        conn: &mut dyn Connection,
        changes: &ChangeSet,
    ) -> Result<Vec<String>> {
        use ferrodb_drivers::redis::native::edit_commands;
        let mut out = Vec::new();
        for ch in &changes.changes {
            let (m, args, needs_type): (&str, Vec<(String, String)>, bool) = match ch {
                Change::SetKey { key, value } => (
                    method::SET,
                    vec![("key".into(), key.clone()), ("value".into(), value.clone())],
                    true,
                ),
                Change::SetKeyMember { key, member, value } => (
                    method::SET_MEMBER,
                    vec![
                        ("key".into(), key.clone()),
                        ("member".into(), member.clone()),
                        ("value".into(), value.clone()),
                    ],
                    true,
                ),
                Change::ExpireKey { key, ttl_secs } => (
                    method::EXPIRE,
                    vec![
                        ("key".into(), key.clone()),
                        (
                            "ttl".into(),
                            ttl_secs.map(|t| t.to_string()).unwrap_or_default(),
                        ),
                    ],
                    false,
                ),
                Change::DeleteKey { key } => {
                    (method::DELETE, vec![("key".into(), key.clone())], false)
                }
                other => {
                    return Err(Error::Unsupported(format!(
                        "{other:?} on a key-value result"
                    )));
                }
            };
            let mut args = args;
            let kind = if needs_type {
                let key = args[0].1.clone();
                let t = collect(
                    conn,
                    kv::command_line(&["TYPE", &key]),
                    CancellationToken::new(),
                )
                .await?;
                let name = ferrodb_meta::rows::rows(&t.results)
                    .first()
                    .and_then(|r| r.values().first().cloned().flatten())
                    .unwrap_or_default();
                Some(kv::key_type(&name))
            } else {
                None
            };
            if m == method::SET && kind == Some(KeyType::Json) {
                args.push(("kind".into(), "json".into()));
            }
            for c in edit_commands(m, &args, kind)? {
                let refs: Vec<&str> = c.iter().map(String::as_str).collect();
                out.push(kv::command_line(&refs));
            }
        }
        Ok(out)
    }

    pub(crate) async fn preview_keys(
        &self,
        rid: ResultId,
        changes: &ChangeSet,
    ) -> Result<Vec<String>> {
        let r = self.key_result(rid)?;
        let mut conn = self
            .kv_connection(&r.data_source, r.database.as_deref())
            .await?;
        let out = self.key_commands(conn.as_mut(), changes).await;
        self.checkin(&r.data_source, conn);
        out
    }

    pub(crate) async fn apply_keys(
        &self,
        rid: ResultId,
        changes: ChangeSet,
    ) -> Result<ApplyReport> {
        let r = self.key_result(rid)?;
        if self.source_config(&r.data_source)?.read_only {
            return Err(Error::ReadOnly);
        }
        let mut conn = self
            .kv_connection(&r.data_source, r.database.as_deref())
            .await?;
        let res = async {
            let statements = self.key_commands(conn.as_mut(), &changes).await?;
            let mut applied = 0;
            for s in &statements {
                let started = (std::time::SystemTime::now(), std::time::Instant::now());
                let out = collect(conn.as_mut(), s.clone(), CancellationToken::new()).await;
                self.log_internal(&r.data_source, r.session, s, started, &out);
                out?;
                applied += 1;
            }
            Ok(ApplyReport {
                applied,
                statements,
                pending_commit: false,
            })
        }
        .await;
        self.checkin(&r.data_source, conn);
        res
    }

    fn log_internal(
        &self,
        ds: &DataSourceId,
        session: SessionId,
        text: &str,
        started: (std::time::SystemTime, std::time::Instant),
        out: &Result<ferrodb_drivers::sink::CollectedOutput>,
    ) {
        let entry = ferrodb_storage::LogEntry {
            data_source: ds.clone(),
            session: session.0,
            source: ferrodb_storage::LogSource::Internal,
            text: text.to_owned(),
            started_at: started.0,
            elapsed: started.1.elapsed(),
            rows: out.as_ref().ok().and_then(|o| o.summary.affected),
            error: out.as_ref().err().map(|e| e.to_string()),
        };
        let storage = self.storage.clone();
        self.handle().spawn_blocking(move || {
            let _ = storage.log(&entry);
        });
    }

    /// Children of a Redis database or key prefix node (live, capped scan).
    pub(crate) async fn redis_children(&self, node: &NodeRef) -> Result<Vec<DbObject>> {
        let Some(((_, db), rest)) = node.path.split_first() else {
            return Ok(Vec::new());
        };
        let sep = ":";
        let prefix: Vec<&str> = rest.iter().map(|(_, n)| n.as_str()).collect();
        let prefix = prefix.join(sep);
        let conn = self.meta_connection(&node.data_source).await?;
        let mut guard = conn.lock().await;
        let r = prefix_children(guard.as_mut(), db, &prefix, sep).await;
        if r == Err(Error::ConnectionLost) {
            drop(guard);
            self.state().meta.remove(&node.data_source);
        }
        Ok(r?
            .into_iter()
            .map(|n| match n {
                KeyNode::Prefix { name, keys, more } => DbObject {
                    node: node.child(ObjectKind::KeyPrefix, &name),
                    kind: ObjectKind::KeyPrefix,
                    detail: Some(format!("{keys}{} keys", if more { "+" } else { "" })),
                    name,
                    has_children: true,
                },
                KeyNode::Key { name, kind } => DbObject {
                    node: node.child(ObjectKind::Key, &name),
                    kind: ObjectKind::Key,
                    detail: Some(format!("{kind:?}")),
                    name,
                    has_children: false,
                },
            })
            .collect())
    }
}

/// Key entries for tests and diagnostics.
#[allow(dead_code)]
pub(crate) fn entries(chunk: &ResultChunk) -> Vec<KeyEntry> {
    match chunk {
        ResultChunk::Keys(k) => k.as_ref().clone(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_risks() {
        assert!(matches!(
            redis_line("SCAN 0 MATCH app:* COUNT 500"),
            Ok(RedisLine::Keys(p, _)) if p == "app:*"
        ));
        assert!(matches!(
            redis_line("GET a"),
            Ok(RedisLine::Command(CommandClass::Read))
        ));
        assert!(redis_line("GET \"a").is_err());
        let r = redis_risks("GET a\nFLUSHALL\nSET a 1\nDEBUG SLEEP 1", false);
        assert_eq!(
            r.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![RiskKind::Truncate, RiskKind::Drop]
        );
        assert_eq!(redis_risks("SET a 1", true)[0].kind, RiskKind::Modification);
    }

    #[test]
    fn cql_statements() {
        assert_eq!(cql_class("SELECT * FROM t"), StatementClass::Read);
        assert_eq!(
            cql_class("BEGIN BATCH INSERT INTO t (a) VALUES (1); APPLY BATCH"),
            StatementClass::Modification
        );
        assert_eq!(cql_class("USE ks"), StatementClass::Session);
        assert_eq!(
            cql_class("INSERT INTO t (a) VALUES (1)"),
            StatementClass::Modification
        );
        assert!(allow_filtering_warning("select * from t where v = 1 ALLOW\nFILTERING;").is_some());
        assert!(allow_filtering_warning("select allow_filtering from t").is_none());
    }
}
