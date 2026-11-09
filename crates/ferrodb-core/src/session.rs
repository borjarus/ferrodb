//! Sessions and script execution.
//!
//! A session is an actor task owning one connection; commands (execute,
//! commit, rollback, close) run in order. A script is split into statements
//! (`ferrodb-lang`), each one is checked (read-only), wrapped in a
//! transaction when the source is in manual-commit mode, parameter values
//! are substituted, and results stream to the UI page by page:
//!
//! - the first `page_size` rows of each result are delivered, then the
//!   result pauses (`ResultPaused { has_more: true }`) and the driver stops
//!   reading thanks to the bounded sink (backpressure up to the socket);
//! - `fetch_more` adds credits to the paused result;
//! - only the last statement of a script can stay paused (its cursor keeps
//!   the connection busy): earlier results are truncated to their first page;
//! - a new command on the session (another execute, commit…) closes a paused
//!   result.

use std::{
    ops::Range,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use ferrodb_api::{
    ApplyReport, BoxFuture, CancelHandle, ChangeSet, DataSourceConfig, DataSourceId, Error,
    ExecOptions, HistoryEntry, KeyValue, NoticeLevel, PlanNode, QueryError, QueryEvent,
    QueryHandle, QueryId, QueryService, Result, ResultChunk, ResultId, ResultShape, Script,
    ServerNotice, SessionId, SessionInfo, TxMode,
};
use ferrodb_drivers::{CancellationToken, Connection, Request, ResultSink, SinkEvent};
use ferrodb_lang::sql::{self, Flavor, StatementClass, TxCommand};
use ferrodb_storage::{LogEntry, LogSource};
use tokio::sync::{mpsc, oneshot, watch};

use crate::Inner;

/// UI event channel capacity (ADR-002).
const EVENTS_CAPACITY: usize = 64;
/// How often the UI's cancel flag is checked.
const CANCEL_POLL: Duration = Duration::from_millis(20);

pub(crate) struct Session {
    pub id: SessionId,
    pub data_source: DataSourceId,
    pub opened_at: SystemTime,
    cmds: mpsc::UnboundedSender<Cmd>,
    pub status: Mutex<Status>,
    /// Bumped whenever a command is queued: a paused result gives way.
    preempt: watch::Sender<u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Status {
    pub in_tx: bool,
    pub schema: Option<String>,
    pub busy: bool,
}

impl Session {
    fn status(&self) -> std::sync::MutexGuard<'_, Status> {
        self.status.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn send(&self, cmd: Cmd) -> bool {
        self.preempt.send_modify(|g| *g += 1);
        self.cmds.send(cmd).is_ok()
    }
}

enum Cmd {
    Execute(Job),
    Tx {
        commit: bool,
        reply: oneshot::Sender<Result<()>>,
    },
    Close {
        reply: oneshot::Sender<()>,
    },
}

struct Job {
    script: Script,
    opts: ExecOptions,
    events: async_channel::Sender<QueryEvent>,
    cancel: CancelHandle,
}

/// What a session needs to know about its source.
#[derive(Clone)]
struct Ctx {
    dialect: String,
    read_only: bool,
    manual_tx: bool,
    timeout: Option<Duration>,
}

impl Ctx {
    fn new(cfg: &DataSourceConfig, caps: &ferrodb_api::Capabilities) -> Self {
        let timeout = cfg
            .properties
            .iter()
            .find(|(k, _)| k == "option.query_timeout")
            .and_then(|(_, v)| v.trim().parse::<u64>().ok())
            .filter(|s| *s > 0)
            .map(Duration::from_secs);
        Self {
            dialect: ferrodb_lang::service::dialect_of(&caps.language).to_owned(),
            read_only: cfg.read_only,
            manual_tx: cfg.tx_mode == TxMode::Manual && caps.supports_transactions(),
            timeout,
        }
    }
}

/// Sends UI events; remembers whether the UI is still listening.
struct Events {
    tx: async_channel::Sender<QueryEvent>,
    closed: AtomicBool,
}

impl Events {
    async fn send(&self, ev: QueryEvent) {
        if self.tx.send(ev).await.is_err() {
            self.closed.store(true, Ordering::Relaxed);
        }
    }
    fn closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
    async fn error(
        &self,
        message: impl Into<String>,
        code: Option<&str>,
        range: Option<Range<usize>>,
    ) {
        self.send(QueryEvent::Error(QueryError {
            message: message.into(),
            code: code.map(str::to_owned),
            range,
        }))
        .await;
    }
}

fn to_query_error(e: &Error, timeout: Option<Duration>) -> QueryError {
    match e {
        Error::Query(q) => q.clone(),
        Error::Timeout => QueryError {
            message: match timeout {
                Some(t) => format!("statement timed out after {} s", t.as_secs()),
                None => "statement timed out".into(),
            },
            code: Some("FERRODB-TIMEOUT".into()),
            range: None,
        },
        Error::Cancelled => QueryError {
            message: "cancelled".into(),
            code: Some("FERRODB-CANCELLED".into()),
            range: None,
        },
        other => QueryError {
            message: other.to_string(),
            code: None,
            range: None,
        },
    }
}

// ------------------------------------------------------------------ actor

struct Actor {
    core: Weak<Inner>,
    session: Arc<Session>,
    conn: Option<Box<dyn Connection>>,
    ctx: Ctx,
}

impl Actor {
    fn core(&self) -> Option<Arc<Inner>> {
        self.core.upgrade()
    }

    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Cmd>) {
        while let Some(cmd) = rx.recv().await {
            match cmd {
                Cmd::Execute(job) => self.execute(job).await,
                Cmd::Tx { commit, reply } => {
                    let r = self.transaction(commit).await;
                    let _ = reply.send(r);
                }
                Cmd::Close { reply } => {
                    self.close().await;
                    let _ = reply.send(());
                    return;
                }
            }
        }
        self.close().await;
    }

    async fn close(&mut self) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };
        if self.session.status().in_tx {
            // Never leave an open transaction in a pooled connection.
            let _ = conn.rollback().await;
        }
        if let Some(core) = self.core() {
            core.checkin(&self.session.data_source, conn);
        }
    }

    fn log(
        &self,
        source: LogSource,
        text: &str,
        started: (SystemTime, Instant),
        rows: Option<u64>,
        error: Option<String>,
    ) {
        let Some(core) = self.core() else { return };
        let entry = LogEntry {
            data_source: self.session.data_source.clone(),
            session: self.session.id.0,
            source,
            text: text.to_owned(),
            started_at: started.0,
            elapsed: started.1.elapsed(),
            rows,
            error,
        };
        let storage = core.storage.clone();
        core.handle().spawn_blocking(move || {
            if let Err(e) = storage.log(&entry) {
                tracing::warn!(error = %e, "query log write failed");
            }
        });
    }

    fn record_history(&self, text: &str, elapsed: Duration, succeeded: bool) {
        let Some(core) = self.core() else { return };
        let entry = HistoryEntry {
            data_source: self.session.data_source.clone(),
            text: text.to_owned(),
            executed_at: SystemTime::now(),
            elapsed,
            succeeded,
        };
        let storage = core.storage.clone();
        core.handle().spawn_blocking(move || {
            if let Err(e) = storage.record_history(&entry) {
                tracing::warn!(error = %e, "history write failed");
            }
        });
    }

    /// The connection, reconnecting when it died since the last command.
    async fn connection(&mut self, events: Option<&Events>) -> Result<&mut Box<dyn Connection>> {
        let alive = self.conn.as_ref().is_some_and(|c| c.is_alive());
        if !alive {
            let core = self.core().ok_or(Error::ConnectionLost)?;
            let lost_tx = std::mem::take(&mut self.session.status().in_tx);
            self.conn = None;
            let mut conn = core.checkout(&self.session.data_source).await?;
            let schema = conn.current_schema().await.ok().flatten();
            self.session.status().schema = schema;
            if let Some(ev) = events {
                let msg = if lost_tx {
                    "The connection was lost and re-established; the open transaction was rolled back"
                } else {
                    "The connection was lost and re-established"
                };
                ev.send(QueryEvent::Notice(ServerNotice {
                    level: NoticeLevel::Warning,
                    message: msg.into(),
                }))
                .await;
            }
            self.conn = Some(conn);
        }
        Ok(self.conn.as_mut().expect("connection set above"))
    }

    /// Session statements issued by the IDE itself (logged as internal).
    async fn internal(&mut self, op: Op<'_>) -> Result<()> {
        let started = (SystemTime::now(), Instant::now());
        let label = match op {
            Op::Begin => "BEGIN".to_owned(),
            Op::Commit => "COMMIT".to_owned(),
            Op::Rollback => "ROLLBACK".to_owned(),
            Op::SetSchema(s) => format!("-- switch schema to {s}"),
        };
        let conn = self.connection(None).await?;
        let r = match op {
            Op::Begin => conn.begin().await,
            Op::Commit => conn.commit().await,
            Op::Rollback => conn.rollback().await,
            Op::SetSchema(s) => conn.set_schema(s).await,
        };
        self.log(
            LogSource::Internal,
            &label,
            started,
            None,
            r.as_ref().err().map(|e| e.to_string()),
        );
        r
    }

    async fn transaction(&mut self, commit: bool) -> Result<()> {
        if self.conn.is_none() {
            return Ok(());
        }
        let r = self
            .internal(if commit { Op::Commit } else { Op::Rollback })
            .await;
        self.session.status().in_tx = false;
        r
    }

    async fn execute(&mut self, job: Job) {
        let events = Events {
            tx: job.events.clone(),
            closed: AtomicBool::new(false),
        };
        let start_gen = *self.session.preempt.borrow();
        let token = CancellationToken::new();
        // Map the UI's cancel flag onto the token.
        let watcher = {
            let (flag, token) = (job.cancel.clone(), token.clone());
            let done = token.child_token();
            let guard = done.clone();
            tokio::spawn(async move {
                loop {
                    if flag.is_cancelled() {
                        token.cancel();
                        return;
                    }
                    tokio::select! {
                        _ = tokio::time::sleep(CANCEL_POLL) => {}
                        _ = guard.cancelled() => return,
                    }
                }
            });
            done
        };
        self.session.status().busy = true;
        let finished = self.run_script(&job, &events, &token, start_gen).await;
        if !finished {
            events.send(QueryEvent::Finished).await;
        }
        self.session.status().busy = false;
        watcher.cancel();
    }

    /// Returns whether `Finished` was already sent (paused last result).
    async fn run_script(
        &mut self,
        job: &Job,
        ev: &Events,
        token: &CancellationToken,
        start_gen: u64,
    ) -> bool {
        let text = &job.script.text;
        let base = job.script.base_offset;
        let flavor = Flavor::for_dialect(&self.ctx.dialect);
        let statements = sql::split(text, &flavor);
        if let Some(schema) = job.opts.schema.clone().filter(|s| !s.is_empty()) {
            let current = self.session.status().schema.clone();
            if current.as_deref() != Some(schema.as_str()) {
                match self.internal(Op::SetSchema(&schema)).await {
                    Ok(()) => self.session.status().schema = Some(schema),
                    Err(e) => {
                        ev.error(e.to_string(), None, None).await;
                        return false;
                    }
                }
            }
        }
        if let Err(e) = self.connection(Some(ev)).await {
            ev.error(e.to_string(), None, None).await;
            return false;
        }
        let count = statements.len();
        for (index, st) in statements.into_iter().enumerate() {
            if token.is_cancelled() || ev.closed() {
                ev.error("cancelled", Some("FERRODB-CANCELLED"), None).await;
                return false;
            }
            let body = st.text(text);
            let abs = base + st.body.start..base + st.body.end;
            let (sql, map) = match sql::substitute(body, &flavor, &job.opts.params) {
                Ok(v) => v,
                Err(missing) => {
                    ev.error(
                        format!("no value for parameter(s): {}", missing.join(", ")),
                        Some("FERRODB-PARAMS"),
                        Some(abs),
                    )
                    .await;
                    return false;
                }
            };
            ev.send(QueryEvent::StatementStarted {
                index,
                range: abs.clone(),
            })
            .await;
            let class = sql::classify(body, &self.ctx.dialect);
            if self.ctx.read_only && !class.allowed_read_only() {
                ev.error(
                    "the data source is read-only: this statement would modify data or schema",
                    Some("FERRODB-READ-ONLY"),
                    Some(abs),
                )
                .await;
                self.record_history(body, Duration::ZERO, false);
                return false;
            }
            if self.ctx.manual_tx
                && !self.session.status().in_tx
                && !matches!(
                    class,
                    StatementClass::Transaction(_) | StatementClass::Session
                )
            {
                if let Err(e) = self.internal(Op::Begin).await {
                    ev.error(e.to_string(), None, Some(abs)).await;
                    return false;
                }
                self.session.status().in_tx = true;
            }
            let started = (SystemTime::now(), Instant::now());
            let r = self
                .run_statement(
                    StatementRun {
                        sql: sql.clone(),
                        title: st.title.clone(),
                        index,
                        last: index + 1 == count,
                        page_size: job.opts.page_size.max(1),
                        timeout: job.opts.timeout.or(self.ctx.timeout),
                        started: started.1,
                        start_gen,
                    },
                    ev,
                    token,
                )
                .await;
            match r {
                Ok(out) => {
                    match class {
                        StatementClass::Transaction(TxCommand::Begin) => {
                            self.session.status().in_tx = true
                        }
                        StatementClass::Transaction(TxCommand::Commit | TxCommand::Rollback) => {
                            self.session.status().in_tx = false
                        }
                        StatementClass::Session => {
                            if let Ok(conn) = self.connection(None).await
                                && let Ok(s) = conn.current_schema().await
                            {
                                self.session.status().schema = s;
                            }
                        }
                        _ => {}
                    }
                    let elapsed = out.elapsed.unwrap_or_else(|| started.1.elapsed());
                    if !out.finished_sent {
                        ev.send(QueryEvent::StatementFinished {
                            index,
                            affected: out.affected,
                            elapsed,
                        })
                        .await;
                    }
                    self.record_history(body, elapsed, true);
                    self.log(
                        LogSource::User,
                        &sql,
                        started,
                        out.affected.or(Some(out.rows)),
                        None,
                    );
                    if out.finished_sent {
                        return true;
                    }
                }
                Err(e) => {
                    let mut q = to_query_error(&e, job.opts.timeout.or(self.ctx.timeout));
                    q.range = Some(match q.range.take() {
                        Some(r) => {
                            let r =
                                map.range_to_original(r.start.min(sql.len())..r.end.min(sql.len()));
                            abs.start + r.start..abs.start + r.end.max(r.start + 1).min(body.len())
                        }
                        None => abs.clone(),
                    });
                    self.record_history(body, started.1.elapsed(), false);
                    self.log(
                        LogSource::User,
                        &sql,
                        started,
                        None,
                        Some(q.message.clone()),
                    );
                    ev.send(QueryEvent::Error(q)).await;
                    return false;
                }
            }
        }
        false
    }

    async fn run_statement(
        &mut self,
        run: StatementRun,
        ev: &Events,
        token: &CancellationToken,
    ) -> Result<StatementOut> {
        let core = self.core().ok_or(Error::ConnectionLost)?;
        let session = self.session.clone();
        let stmt_token = token.child_token();
        let finished = Arc::new(AtomicBool::new(false));
        let timed_out = Arc::new(AtomicBool::new(false));
        let (sink, rx) = ResultSink::channel(4, ResultSink::BATCH_ROWS);
        let conn = self.connection(Some(ev)).await?;
        let exec = conn.execute(
            Request::Text {
                text: run.sql.clone(),
            },
            sink,
            stmt_token.clone(),
        );
        let fwd = forward(&core, &session, rx, ev, &run, &stmt_token, &finished);
        let both = async { futures::join!(exec, fwd) };
        tokio::pin!(both);
        let timer = {
            let (finished, timed_out, t) =
                (finished.clone(), timed_out.clone(), stmt_token.clone());
            let timeout = run.timeout;
            async move {
                match timeout {
                    Some(d) => {
                        tokio::time::sleep(d).await;
                        if !finished.load(Ordering::SeqCst) {
                            timed_out.store(true, Ordering::SeqCst);
                            t.cancel();
                        }
                        std::future::pending::<()>().await
                    }
                    None => std::future::pending().await,
                }
            }
        };
        let (summary, fout) = tokio::select! {
            r = &mut both => r,
            _ = timer => unreachable!("the timer never completes"),
        };
        match summary {
            Ok(s) => Ok(StatementOut {
                affected: s.affected,
                rows: fout.rows,
                finished_sent: fout.finished_sent,
                elapsed: fout.elapsed,
            }),
            Err(Error::Cancelled) if timed_out.load(Ordering::SeqCst) => Err(Error::Timeout),
            // A paused result closed by a new command / cancel after the
            // script already finished: not an error for this statement.
            Err(Error::Cancelled) if fout.finished_sent => Ok(StatementOut {
                affected: None,
                rows: fout.rows,
                finished_sent: true,
                elapsed: fout.elapsed,
            }),
            Err(e) => Err(e),
        }
    }
}

#[derive(Clone, Copy)]
enum Op<'a> {
    Begin,
    Commit,
    Rollback,
    SetSchema(&'a str),
}

struct StatementRun {
    sql: String,
    title: Option<String>,
    index: usize,
    last: bool,
    page_size: usize,
    timeout: Option<Duration>,
    started: Instant,
    start_gen: u64,
}

struct StatementOut {
    affected: Option<u64>,
    rows: u64,
    /// `StatementFinished` + `Finished` were sent when the result paused.
    finished_sent: bool,
    elapsed: Option<Duration>,
}

#[derive(Default)]
struct ForwardOut {
    rows: u64,
    finished_sent: bool,
    elapsed: Option<Duration>,
}

/// Moves sink events to the UI, applying page credits.
async fn forward(
    core: &Inner,
    session: &Session,
    mut rx: mpsc::Receiver<SinkEvent>,
    ev: &Events,
    run: &StatementRun,
    token: &CancellationToken,
    finished: &AtomicBool,
) -> ForwardOut {
    let mut out = ForwardOut::default();
    let mut current: Option<(ResultId, mpsc::UnboundedReceiver<usize>)> = None;
    let mut credits = 0usize;
    let mut results = 0usize;
    let mut pending: Option<SinkEvent> = None;

    let unregister = |rid: ResultId| {
        core.state().results.remove(&rid);
    };

    loop {
        let next = match pending.take() {
            Some(e) => Some(e),
            None => rx.recv().await,
        };
        if ev.closed() {
            break;
        }
        let Some(event) = next else { break };
        match event {
            SinkEvent::Begin { schema } => {
                if let Some((rid, _)) = current.take() {
                    unregister(rid);
                    ev.send(QueryEvent::ResultPaused {
                        result: rid,
                        has_more: false,
                    })
                    .await;
                }
                results += 1;
                let rid = ResultId(core.next_id());
                let (ctx, crx) = mpsc::unbounded_channel();
                core.state().results.insert(rid, ctx);
                current = Some((rid, crx));
                credits = run.page_size;
                let title = match (&run.title, results) {
                    (Some(t), 1) => Some(t.clone()),
                    (Some(t), n) => Some(format!("{t} ({n})")),
                    (None, _) => None,
                };
                ev.send(QueryEvent::ResultStarted {
                    result: rid,
                    shape: ResultShape::Table(schema),
                    title,
                })
                .await;
            }
            SinkEvent::Batch(batch) => {
                let Some((rid, _)) = &current else { continue };
                let rid = *rid;
                if credits == 0 {
                    // Page full and more rows exist: pause.
                    if !run.last {
                        ev.send(QueryEvent::ResultPaused {
                            result: rid,
                            has_more: false,
                        })
                        .await;
                        ev.send(QueryEvent::Notice(ServerNotice {
                            level: NoticeLevel::Info,
                            message: format!(
                                "Result truncated to {} rows; run the statement on its own to page through it",
                                out.rows
                            ),
                        }))
                        .await;
                        unregister(rid);
                        return out;
                    }
                    ev.send(QueryEvent::ResultPaused {
                        result: rid,
                        has_more: true,
                    })
                    .await;
                    if !out.finished_sent {
                        let elapsed = run.started.elapsed();
                        out.elapsed = Some(elapsed);
                        out.finished_sent = true;
                        finished.store(true, Ordering::SeqCst);
                        ev.send(QueryEvent::StatementFinished {
                            index: run.index,
                            affected: None,
                            elapsed,
                        })
                        .await;
                        ev.send(QueryEvent::Finished).await;
                        session.status().busy = false;
                    }
                    let mut pre = session.preempt.subscribe();
                    let preempted = *pre.borrow_and_update() != run.start_gen;
                    let granted = if preempted {
                        None
                    } else {
                        let crx = &mut current.as_mut().expect("current result").1;
                        tokio::select! {
                            n = crx.recv() => n,
                            _ = pre.changed() => None,
                            _ = token.cancelled() => None,
                        }
                    };
                    match granted {
                        Some(n) => {
                            credits += n.max(1);
                            pending = Some(SinkEvent::Batch(batch));
                            continue;
                        }
                        None => {
                            ev.send(QueryEvent::ResultPaused {
                                result: rid,
                                has_more: false,
                            })
                            .await;
                            unregister(rid);
                            return out;
                        }
                    }
                }
                let take = credits.min(batch.num_rows());
                let chunk = if take == batch.num_rows() {
                    batch.clone()
                } else {
                    batch.slice(0, take)
                };
                credits -= take;
                out.rows += take as u64;
                ev.send(QueryEvent::Data {
                    result: rid,
                    chunk: ResultChunk::Rows(Arc::new(chunk)),
                })
                .await;
                if take < batch.num_rows() {
                    pending = Some(SinkEvent::Batch(batch.slice(take, batch.num_rows() - take)));
                }
            }
            SinkEvent::End => {
                if let Some((rid, _)) = current.take() {
                    unregister(rid);
                    ev.send(QueryEvent::ResultPaused {
                        result: rid,
                        has_more: false,
                    })
                    .await;
                }
            }
            SinkEvent::Notice(n) => ev.send(QueryEvent::Notice(n)).await,
        }
    }
    if let Some((rid, _)) = current.take() {
        unregister(rid);
        ev.send(QueryEvent::ResultPaused {
            result: rid,
            has_more: false,
        })
        .await;
    }
    out
}

// ------------------------------------------------------------------ service

impl Inner {
    pub(crate) async fn close_session_now(&self, id: SessionId) {
        let s = self.state().sessions.remove(&id);
        if let Some(s) = s {
            let (tx, rx) = oneshot::channel();
            if s.send(Cmd::Close { reply: tx }) {
                let _ = rx.await;
            }
        }
    }

    fn session(&self, id: SessionId) -> Result<Arc<Session>> {
        self.state()
            .sessions
            .get(&id)
            .cloned()
            .ok_or_else(|| Error::NotFound(format!("session {id}")))
    }

    fn tx_command(&self, id: SessionId, commit: bool) -> BoxFuture<'_, Result<()>> {
        let s = self.session(id);
        Box::pin(async move {
            let s = s?;
            let (tx, rx) = oneshot::channel();
            if !s.send(Cmd::Tx { commit, reply: tx }) {
                return Err(Error::ConnectionLost);
            }
            rx.await.map_err(|_| Error::ConnectionLost)?
        })
    }
}

impl QueryService for Inner {
    fn open_session(&self, ds: &DataSourceId) -> BoxFuture<'_, Result<SessionId>> {
        let me = self.arc();
        let ds = ds.clone();
        self.spawn(async move {
            let cfg = me.source_config(&ds)?;
            let caps = me.driver_for(&cfg)?.capabilities();
            let mut conn = me.checkout(&ds).await?;
            let schema = conn.current_schema().await.ok().flatten();
            let (tx, rx) = mpsc::unbounded_channel();
            let session = Arc::new(Session {
                id: SessionId(me.next_id()),
                data_source: ds.clone(),
                opened_at: SystemTime::now(),
                cmds: tx,
                status: Mutex::new(Status {
                    schema,
                    ..Default::default()
                }),
                preempt: watch::channel(0).0,
            });
            let actor = Actor {
                core: Arc::downgrade(&me),
                session: session.clone(),
                conn: Some(conn),
                ctx: Ctx::new(&cfg, &caps),
            };
            tokio::spawn(actor.run(rx));
            let id = session.id;
            me.state().sessions.insert(id, session);
            Ok(id)
        })
    }

    fn close_session(&self, session: SessionId) -> BoxFuture<'_, Result<()>> {
        let me = self.arc();
        self.spawn(async move {
            me.close_session_now(session).await;
            Ok(())
        })
    }

    fn sessions(&self) -> Vec<SessionInfo> {
        let mut v: Vec<SessionInfo> = self
            .state()
            .sessions
            .values()
            .map(|s| {
                let st = s.status();
                SessionInfo {
                    id: s.id,
                    data_source: s.data_source.clone(),
                    opened_at: s.opened_at,
                    in_transaction: st.in_tx,
                    schema: st.schema.clone(),
                    busy: st.busy,
                }
            })
            .collect();
        v.sort_by_key(|s| s.id);
        v
    }

    fn execute(&self, session: SessionId, script: Script, opts: ExecOptions) -> QueryHandle {
        let (tx, rx) = async_channel::bounded(EVENTS_CAPACITY);
        let cancel = CancelHandle::new();
        let handle = QueryHandle {
            id: QueryId(self.next_id()),
            events: rx,
            cancel: cancel.clone(),
        };
        let job = Job {
            script,
            opts,
            events: tx.clone(),
            cancel,
        };
        let sent = self
            .session(session)
            .map(|s| s.send(Cmd::Execute(job)))
            .unwrap_or(false);
        if !sent {
            self.handle().spawn(async move {
                let _ = tx
                    .send(QueryEvent::Error(QueryError {
                        message: format!("session {session} is closed"),
                        code: None,
                        range: None,
                    }))
                    .await;
                let _ = tx.send(QueryEvent::Finished).await;
            });
        }
        handle
    }

    fn fetch_more(&self, result: ResultId, items: usize) -> BoxFuture<'_, Result<()>> {
        let r = self
            .state()
            .results
            .get(&result)
            .map(|tx| tx.send(items).is_ok())
            .unwrap_or(false);
        Box::pin(async move {
            if r {
                Ok(())
            } else {
                Err(Error::NotFound(format!("result {result} has no more rows")))
            }
        })
    }

    fn estimate_cost(
        &self,
        _session: SessionId,
        _script: Script,
    ) -> BoxFuture<'_, Result<Option<u64>>> {
        Box::pin(async { Ok(None) })
    }

    fn explain(&self, _session: SessionId, _script: Script) -> BoxFuture<'_, Result<PlanNode>> {
        Box::pin(async { Err(Error::Unsupported("execution plans (phase 7)".into())) })
    }

    fn key_value(&self, _session: SessionId, _key: String) -> BoxFuture<'_, Result<KeyValue>> {
        Box::pin(async { Err(Error::Unsupported("key-value sources".into())) })
    }

    fn preview_changes(
        &self,
        _result: ResultId,
        _changes: ChangeSet,
    ) -> BoxFuture<'_, Result<Vec<String>>> {
        Box::pin(async { Err(Error::Unsupported("editing results (phase 6)".into())) })
    }

    fn apply_changes(
        &self,
        _result: ResultId,
        _changes: ChangeSet,
    ) -> BoxFuture<'_, Result<ApplyReport>> {
        Box::pin(async { Err(Error::Unsupported("editing results (phase 6)".into())) })
    }

    fn commit(&self, session: SessionId) -> BoxFuture<'_, Result<()>> {
        self.tx_command(session, true)
    }

    fn rollback(&self, session: SessionId) -> BoxFuture<'_, Result<()>> {
        self.tx_command(session, false)
    }
}
