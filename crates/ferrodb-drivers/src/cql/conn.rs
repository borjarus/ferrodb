//! Connecting (contact points, local data center, authentication,
//! keyspace) and the [`Connection`] implementation: statements paged with
//! the server's paging state, one page per `batch_rows` rows, so a paused
//! result stops fetching pages; server warnings become notices.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use ferrodb_api::{Error, NoticeLevel, QueryError, Result};
use scylla::{
    client::{
        execution_profile::ExecutionProfile, session::Session, session_builder::SessionBuilder,
    },
    errors::{ExecutionError, RequestAttemptError},
    policies::load_balancing::DefaultPolicy,
    response::{PagingState, PagingStateResponse},
    statement::{Consistency, SerialConsistency, Statement},
    value::Row,
};

use super::{DEFAULT_PORT, types};
use crate::{
    BoxFuture, CancellationToken, Connection, ExecSummary, RemoteCancel, Request, ResolvedConfig,
    ResultSink, SinkEvent,
    columns::{BatchBuilder, schema},
};

/// Sends every node's traffic to the (first) contact point: single nodes
/// behind NAT, Docker port mappings or tunnels, whose advertised addresses
/// are not reachable from here (`address_translation=contact_point`).
struct ContactPoint(std::net::SocketAddr);

#[async_trait::async_trait]
impl scylla::policies::address_translator::AddressTranslator for ContactPoint {
    async fn translate_address(
        &self,
        _peer: &scylla::policies::address_translator::UntranslatedPeer,
    ) -> std::result::Result<std::net::SocketAddr, scylla::errors::TranslationError> {
        Ok(self.0)
    }
}

pub struct CqlConnection {
    session: Session,
    consistency: Consistency,
    serial: Option<SerialConsistency>,
    read_only: bool,
    version: String,
    alive: Arc<AtomicBool>,
}

/// `ONE`, `local_quorum`… as a consistency level.
pub fn parse_consistency(s: &str) -> Option<Consistency> {
    Some(match s.trim().to_ascii_uppercase().as_str() {
        "ANY" => Consistency::Any,
        "ONE" => Consistency::One,
        "TWO" => Consistency::Two,
        "THREE" => Consistency::Three,
        "QUORUM" => Consistency::Quorum,
        "ALL" => Consistency::All,
        "LOCAL_QUORUM" => Consistency::LocalQuorum,
        "EACH_QUORUM" => Consistency::EachQuorum,
        "LOCAL_ONE" => Consistency::LocalOne,
        "SERIAL" => Consistency::Serial,
        "LOCAL_SERIAL" => Consistency::LocalSerial,
        _ => return None,
    })
}

pub fn parse_serial(s: &str) -> Option<SerialConsistency> {
    match s.trim().to_ascii_uppercase().as_str() {
        "SERIAL" => Some(SerialConsistency::Serial),
        "LOCAL_SERIAL" => Some(SerialConsistency::LocalSerial),
        _ => None,
    }
}

fn consistency_name(c: Consistency) -> String {
    format!("{c:?}")
        .chars()
        .enumerate()
        .flat_map(|(i, ch)| {
            let up = ch.to_ascii_uppercase();
            if i > 0 && ch.is_ascii_uppercase() {
                vec!['_', up]
            } else {
                vec![up]
            }
        })
        .collect()
}

/// A cqlsh-style shell command handled by the driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shell {
    /// `CONSISTENCY` (show) / `CONSISTENCY QUORUM`.
    Consistency(Option<String>),
    /// `SERIAL CONSISTENCY` (show) / `SERIAL CONSISTENCY LOCAL_SERIAL`.
    Serial(Option<String>),
    /// `USE ks` / `USE "Ks"` (name, quoted).
    Use(String, bool),
}

pub fn shell(text: &str) -> Option<Shell> {
    let t = text.trim().trim_end_matches(';').trim();
    let words: Vec<&str> = t.split_whitespace().collect();
    match words.as_slice() {
        [w] if w.eq_ignore_ascii_case("consistency") => Some(Shell::Consistency(None)),
        [w, l] if w.eq_ignore_ascii_case("consistency") => {
            Some(Shell::Consistency(Some((*l).to_owned())))
        }
        [s, w] if s.eq_ignore_ascii_case("serial") && w.eq_ignore_ascii_case("consistency") => {
            Some(Shell::Serial(None))
        }
        [s, w, l] if s.eq_ignore_ascii_case("serial") && w.eq_ignore_ascii_case("consistency") => {
            Some(Shell::Serial(Some((*l).to_owned())))
        }
        [u, ks] if u.eq_ignore_ascii_case("use") => {
            let quoted = ks.starts_with('"') && ks.ends_with('"') && ks.len() >= 2;
            let name = if quoted {
                ks[1..ks.len() - 1].replace("\"\"", "\"")
            } else {
                (*ks).to_owned()
            };
            Some(Shell::Use(name, quoted))
        }
        _ => None,
    }
}

/// Statements allowed on read-only sources.
pub fn is_read(text: &str) -> bool {
    let first = text
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        first.as_str(),
        "select" | "describe" | "desc" | "list" | "use" | "consistency" | "serial"
    )
}

/// Driver error → contract error.
pub(crate) fn map_err(e: ExecutionError) -> Error {
    match e {
        ExecutionError::LastAttemptError(RequestAttemptError::DbError(db, message)) => {
            Error::Query(QueryError {
                message,
                code: Some(db_code(&db)),
                range: None,
            })
        }
        ExecutionError::LastAttemptError(RequestAttemptError::BrokenConnectionError(_)) => {
            Error::ConnectionLost
        }
        // No usable connection to the node: say why (unreachable address,
        // node down) instead of a bare "connection lost".
        ExecutionError::ConnectionPoolError(e) => Error::Connection(e.to_string()),
        ExecutionError::RequestTimeout(_) => Error::Timeout,
        other => Error::Query(QueryError {
            message: other.to_string(),
            code: None,
            range: None,
        }),
    }
}

/// `SyntaxError`, `Invalid`, `Unavailable`… (the error class name).
fn db_code(db: &scylla::errors::DbError) -> String {
    let s = format!("{db:?}");
    s.split([' ', '{', '(']).next().unwrap_or(&s).to_owned()
}

impl CqlConnection {
    pub async fn connect(cfg: &ResolvedConfig) -> Result<Self> {
        if cfg.ssh.is_some() {
            return Err(Error::Connection(
                "SSH tunnels are not supported for Cassandra (the driver connects to every node)"
                    .into(),
            ));
        }
        if matches!(
            cfg.tls,
            ferrodb_net::TlsMode::Require | ferrodb_net::TlsMode::VerifyFull
        ) {
            return Err(Error::Connection(
                "TLS to Cassandra is not available in this build (the CQL driver's TLS needs aws-lc-rs)"
                    .into(),
            ));
        }
        let port = cfg.port(DEFAULT_PORT)?;
        let nodes: Vec<String> = cfg
            .host()
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(|h| {
                if h.rsplit_once(':')
                    .is_some_and(|(a, p)| !a.ends_with(':') && p.parse::<u16>().is_ok())
                {
                    h.to_owned()
                } else {
                    format!("{h}:{port}")
                }
            })
            .collect();
        let consistency = match cfg.prop("consistency") {
            Some(c) => parse_consistency(c)
                .ok_or_else(|| Error::Connection(format!("consistency: unknown level {c}")))?,
            None => Consistency::LocalOne,
        };
        let serial = match cfg.prop("serial_consistency") {
            Some(c) => Some(parse_serial(c).ok_or_else(|| {
                Error::Connection(format!("serial_consistency: unknown level {c}"))
            })?),
            None => None,
        };
        let mut policy = DefaultPolicy::builder();
        if let Some(dc) = cfg.prop("local_dc") {
            policy = policy
                .prefer_datacenter(dc.to_owned())
                .permit_dc_failover(true);
        }
        let profile = ExecutionProfile::builder()
            .consistency(consistency)
            .load_balancing_policy(policy.build())
            .request_timeout(None)
            .build();
        let mut b = SessionBuilder::new()
            .known_nodes(&nodes)
            .connection_timeout(cfg.connect_timeout)
            .default_execution_profile_handle(profile.into_handle());
        if let Some(u) = cfg.user() {
            b = b.user(u, cfg.password().unwrap_or_default());
        }
        match cfg.prop("address_translation") {
            None | Some("none") => {}
            Some("contact_point") => {
                let first = nodes.first().cloned().unwrap_or_default();
                let addr = tokio::net::lookup_host(first.as_str())
                    .await
                    .ok()
                    .and_then(|mut a| a.next())
                    .ok_or_else(|| Error::Connection(format!("cannot resolve {first}")))?;
                b = b.address_translator(Arc::new(ContactPoint(addr)));
            }
            Some(other) => {
                return Err(Error::Connection(format!(
                    "address_translation: expected none or contact_point, got {other}"
                )));
            }
        }
        let session = tokio::time::timeout(cfg.connect_timeout + Duration::from_secs(5), b.build())
            .await
            .map_err(|_| Error::Connection("timed out connecting to Cassandra".into()))?
            .map_err(|e| Error::Connection(e.to_string()))?;
        if let Some(ks) = cfg.database() {
            let quoted = ks.chars().any(|c| c.is_ascii_uppercase());
            session
                .use_keyspace(ks, quoted)
                .await
                .map_err(|e| Error::Connection(format!("keyspace {ks}: {e}")))?;
        }
        let mut c = Self {
            session,
            consistency,
            serial,
            read_only: cfg.read_only,
            version: String::new(),
            alive: Arc::new(AtomicBool::new(true)),
        };
        c.version = c
            .single_text("SELECT release_version FROM system.local")
            .await
            .ok()
            .flatten()
            .map(|v| format!("Apache Cassandra {v}"))
            .unwrap_or_else(|| "Apache Cassandra".into());
        Ok(c)
    }

    async fn single_text(&self, cql: &str) -> Result<Option<String>> {
        let r = self
            .session
            .query_unpaged(cql, &[])
            .await
            .map_err(map_err)?;
        let rows = r
            .into_rows_result()
            .map_err(|e| Error::Other(e.to_string()))?;
        let row: Option<(Option<String>,)> = rows
            .maybe_first_row()
            .map_err(|e| Error::Other(e.to_string()))?;
        Ok(row.and_then(|r| r.0))
    }

    fn check(&self, e: Error) -> Error {
        if e == Error::ConnectionLost {
            self.alive.store(false, Ordering::SeqCst);
        }
        e
    }

    fn info_table(
        &self,
        name: &str,
        value: String,
    ) -> (arrow_schema::SchemaRef, arrow_array::RecordBatch) {
        let s = schema(&[crate::columns::ColumnSpec::new(
            name,
            crate::columns::ColumnType::Text,
            "text",
        )]);
        let b = arrow_array::RecordBatch::try_new(
            s.clone(),
            vec![Arc::new(arrow_array::StringArray::from(vec![value])) as _],
        )
        .expect("one column");
        (s, b)
    }

    async fn run_shell(&mut self, sh: Shell, sink: &ResultSink) -> Result<ExecSummary> {
        let shown = match sh {
            Shell::Consistency(None) => Some(("consistency", consistency_name(self.consistency))),
            Shell::Consistency(Some(l)) => {
                self.consistency = parse_consistency(&l).ok_or_else(|| {
                    Error::Query(QueryError {
                        message: format!("unknown consistency level {l}"),
                        code: Some("FERRODB-CQL".into()),
                        range: None,
                    })
                })?;
                sink.notice(
                    NoticeLevel::Info,
                    format!(
                        "Consistency level set to {}",
                        consistency_name(self.consistency)
                    ),
                )
                .await;
                None
            }
            Shell::Serial(None) => Some((
                "serial_consistency",
                self.serial
                    .map(|s| format!("{s:?}").to_ascii_uppercase())
                    .unwrap_or_else(|| "SERIAL".into()),
            )),
            Shell::Serial(Some(l)) => {
                self.serial = Some(parse_serial(&l).ok_or_else(|| {
                    Error::Query(QueryError {
                        message: format!("unknown serial consistency level {l}"),
                        code: Some("FERRODB-CQL".into()),
                        range: None,
                    })
                })?);
                None
            }
            Shell::Use(ks, quoted) => {
                self.session
                    .use_keyspace(ks.clone(), quoted)
                    .await
                    .map_err(|e| {
                        Error::Query(QueryError {
                            message: e.to_string(),
                            code: Some("Invalid".into()),
                            range: None,
                        })
                    })?;
                None
            }
        };
        if let Some((name, value)) = shown {
            let (s, b) = self.info_table(name, value);
            let _ = sink.send(SinkEvent::Begin { schema: s }).await;
            let _ = sink.send(SinkEvent::Batch(b)).await;
            let _ = sink.send(SinkEvent::End).await;
            return Ok(ExecSummary {
                affected: None,
                results: 1,
                truncated: false,
            });
        }
        Ok(ExecSummary::default())
    }

    async fn statement(&mut self, text: &str, sink: &ResultSink) -> Result<ExecSummary> {
        let text = text.trim().trim_end_matches(';').trim();
        if let Some(sh) = shell(text) {
            return self.run_shell(sh, sink).await;
        }
        if self.read_only && !is_read(text) {
            return Err(Error::ReadOnly);
        }
        let mut st = Statement::new(text.to_owned());
        st.set_consistency(self.consistency);
        st.set_serial_consistency(self.serial);
        st.set_page_size(sink.batch_rows().clamp(1, 5000) as i32);
        let mut paging = PagingState::start();
        let mut started = false;
        let mut summary = ExecSummary::default();
        let mut builder: Option<BatchBuilder> = None;
        loop {
            let r = self
                .session
                .query_single_page(st.clone(), &[], paging.clone())
                .await;
            let (res, next) = r.map_err(|e| self.check(map_err(e)))?;
            for w in res.warnings() {
                sink.notice(NoticeLevel::Warning, w.to_owned()).await;
            }
            if !res.is_rows() {
                // DDL, DML, USE: no result set (Cassandra reports no counts).
                break;
            }
            let rows = res
                .into_rows_result()
                .map_err(|e| Error::Other(e.to_string()))?;
            if !started {
                let cols: Vec<_> = rows
                    .column_specs()
                    .iter()
                    .map(|c| types::column(c.name(), c.typ()))
                    .collect();
                let s = schema(&cols);
                if sink
                    .send(SinkEvent::Begin { schema: s.clone() })
                    .await
                    .is_err()
                {
                    summary.truncated = true;
                    return Ok(summary);
                }
                summary.results = 1;
                builder = Some(BatchBuilder::new(s, sink.batch_rows()));
                started = true;
            }
            let b = builder.as_mut().expect("set with the schema");
            let iter = rows
                .rows::<Row>()
                .map_err(|e| Error::Other(e.to_string()))?;
            for row in iter {
                let row = row.map_err(|e| Error::Other(e.to_string()))?;
                for (i, v) in row.columns.iter().enumerate() {
                    types::push(b, i, v.as_ref());
                }
                b.end_row();
                if b.rows() >= sink.batch_rows()
                    && let Some(batch) = b.flush()
                    && sink.send(SinkEvent::Batch(batch)).await.is_err()
                {
                    summary.truncated = true;
                    return Ok(summary);
                }
            }
            // Send what this page gave before asking for the next one: a
            // paused consumer stops the fetching here (backpressure).
            if let Some(batch) = b.flush()
                && sink.send(SinkEvent::Batch(batch)).await.is_err()
            {
                summary.truncated = true;
                return Ok(summary);
            }
            match next {
                PagingStateResponse::HasMorePages { state } => paging = state,
                PagingStateResponse::NoMorePages => break,
            }
        }
        if started && sink.send(SinkEvent::End).await.is_err() {
            summary.truncated = true;
        }
        Ok(summary)
    }
}

impl Connection for CqlConnection {
    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        Box::pin(async move {
            let text = req.into_text()?;
            let fut = self.statement(&text, &sink);
            tokio::pin!(fut);
            // Cassandra has no server-side cancel: the request is abandoned
            // (its response is discarded by the driver when it arrives).
            tokio::select! {
                r = &mut fut => r,
                _ = cancel.cancelled() => Err(Error::Cancelled),
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        None
    }

    fn set_read_only(&mut self, read_only: bool) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.read_only = read_only;
            Ok(())
        })
    }

    fn set_schema<'a>(&'a mut self, schema: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let quoted = schema.chars().any(|c| c.is_ascii_uppercase());
            self.session
                .use_keyspace(schema, quoted)
                .await
                .map_err(|e| Error::Other(e.to_string()))
        })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(self.session.get_keyspace().map(|k| (*k).clone())) })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "transactions (Cassandra runs single statements and BEGIN BATCH … APPLY BATCH)"
                    .into(),
            ))
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            let r = self
                .session
                .query_unpaged("SELECT now() FROM system.local", &[])
                .await;
            r.map(|_| ()).map_err(|e| self.check(map_err(e)))
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_commands() {
        assert_eq!(shell("CONSISTENCY"), Some(Shell::Consistency(None)));
        assert_eq!(
            shell("consistency quorum;"),
            Some(Shell::Consistency(Some("quorum".into())))
        );
        assert_eq!(
            shell("SERIAL CONSISTENCY LOCAL_SERIAL"),
            Some(Shell::Serial(Some("LOCAL_SERIAL".into())))
        );
        assert_eq!(shell("USE ks;"), Some(Shell::Use("ks".into(), false)));
        assert_eq!(
            shell("use \"My\"\"Ks\""),
            Some(Shell::Use("My\"Ks".into(), true))
        );
        assert_eq!(shell("SELECT * FROM t"), None);
        assert_eq!(
            parse_consistency("local_quorum"),
            Some(Consistency::LocalQuorum)
        );
        assert_eq!(consistency_name(Consistency::LocalQuorum), "LOCAL_QUORUM");
        assert!(is_read("select * from t"));
        assert!(!is_read("INSERT INTO t (a) VALUES (1)"));
    }
}
