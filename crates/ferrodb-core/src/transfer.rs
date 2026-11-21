//! Data transfer (plan, phase 6): export, import, copying between sources
//! and data models, materializing a result.
//!
//! A transfer is a producer and a consumer joined by a bounded channel of
//! chunks (≤ 1000 rows / documents each), so memory stays constant:
//!
//! - producers: a read-only query or a whole table / collection, run on
//!   its own connection with the driver's streaming sink; or a file read
//!   on a blocking thread;
//! - consumers: a file writer (blocking thread), an SQL table (one
//!   transaction; PostgreSQL `COPY`, MySQL `LOAD DATA LOCAL INFILE`, else
//!   multi-row `INSERT`), or a collection (`insertMany` batches).
//!
//! The producer ends a successful stream with `Chunk::End`; a consumer that
//! sees the channel close without it rolls back and the producer's error is
//! reported.

use std::{path::PathBuf, time::Duration};

use arrow_array::RecordBatch;
use arrow_schema::{DataType, SchemaRef};
use bson::Document;
use ferrodb_api::{
    BoxFuture, CancelHandle, ColumnMapping, DataFormat, DataSourceId, Error, FilePreview,
    FormatOptions, NodeRef, ObjectKind, QueryLanguage, RefreshScope, Result, TransferEvent,
    TransferHandle, TransferReport, TransferRequest, TransferService, TransferSource,
    TransferTarget,
};
use ferrodb_data::{
    Value, ValueType,
    docs::{DocColumn, document_to_row, infer_columns, row_to_document},
    export::Exporter,
    import::{self, Mapping, SourceColumn},
    sql::{ColumnDef, Family, Sql},
    value::value_at,
};
use ferrodb_drivers::{
    CancellationToken, Connection, Request, ResultSink, SinkEvent, sink::collect,
    sink::collect_request,
};
use ferrodb_lang::{
    mql::{self, Command, Op},
    sql::{self, Flavor, StatementClass},
};
use tokio::sync::mpsc;

use crate::Inner;

/// Rows / documents per chunk.
const CHUNK: usize = 1000;
/// Rows per fast-path load (`COPY`, `LOAD DATA`).
const BULK_ROWS: usize = 20_000;
/// Rows per multi-row `INSERT`.
const INSERT_ROWS: usize = 500;
/// Documents per `insertMany`.
const INSERT_DOCS: usize = 1000;

enum Chunk {
    /// A tabular result starts.
    Table(SchemaRef),
    Batch(RecordBatch),
    /// File columns.
    Columns(Vec<SourceColumn>),
    Rows(Vec<Vec<Value>>),
    Docs(Vec<Document>),
    End,
}

fn err(e: impl std::fmt::Display) -> Error {
    Error::Other(e.to_string())
}

/// Progress events, at most every 2000 items.
struct Progress {
    tx: async_channel::Sender<TransferEvent>,
    items: u64,
    reported: u64,
}

impl Progress {
    fn add(&mut self, n: u64) {
        self.items += n;
        if self.items - self.reported >= 2000 {
            self.reported = self.items;
            let _ = self
                .tx
                .try_send(TransferEvent::Progress { items: self.items });
        }
    }
    fn warn(&self, w: impl Into<String>) {
        let _ = self.tx.try_send(TransferEvent::Warning(w.into()));
    }
}

/// Maps the UI's cancel flag onto a token until `done` fires.
fn watch_cancel(handle: &tokio::runtime::Handle, flag: CancelHandle) -> CancellationToken {
    let token = CancellationToken::new();
    let t = token.clone();
    handle.spawn(async move {
        while !t.is_cancelled() {
            if flag.is_cancelled() {
                t.cancel();
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    });
    token
}

fn is_mql(lang: &QueryLanguage) -> bool {
    *lang == QueryLanguage::Mql
}

/// `numeric(10,2)` → (10, 2).
fn precision(db_type: &str) -> Option<(u8, i8)> {
    let inner = db_type.split_once('(')?.1.split_once(')')?.0;
    let (p, s) = inner.split_once(',').unwrap_or((inner, "0"));
    Some((p.trim().parse().ok()?, s.trim().parse().ok()?))
}

/// Source columns of a first chunk. Tabular sources carry the database
/// type of each column in the field metadata: it is more precise than the
/// Arrow type (SQLite `DATE` / `NUMERIC` arrive as text / doubles).
fn columns_of(first: &Chunk) -> (Vec<SourceColumn>, Vec<Option<(u8, i8)>>) {
    match first {
        Chunk::Table(s) => s
            .fields()
            .iter()
            .map(|f| {
                let db = f.metadata().get(ferrodb_drivers::columns::DB_TYPE_KEY);
                let ty = match db {
                    Some(t) if !t.is_empty() => ValueType::from_sql(t),
                    _ => ValueType::from_arrow(f.data_type()),
                };
                let dec = match (f.data_type(), db) {
                    (DataType::Decimal128(p, s), _) => Some((*p, *s)),
                    (_, Some(t)) if ty == ValueType::Decimal => precision(t),
                    _ => None,
                };
                (
                    SourceColumn {
                        name: f.name().clone(),
                        ty,
                    },
                    dec,
                )
            })
            .unzip(),
        Chunk::Columns(c) => (c.clone(), vec![None; c.len()]),
        Chunk::Docs(d) => {
            let c: Vec<SourceColumn> = infer_columns(d)
                .into_iter()
                .map(|c| SourceColumn {
                    name: c.path,
                    ty: c.ty,
                })
                .collect();
            let n = c.len();
            (c, vec![None; n])
        }
        _ => (Vec::new(), Vec::new()),
    }
}

fn batch_rows(b: &RecordBatch) -> Vec<Vec<Value>> {
    (0..b.num_rows())
        .map(|r| {
            b.columns()
                .iter()
                .map(|c| value_at(c.as_ref(), r))
                .collect()
        })
        .collect()
}

impl Inner {
    fn language_of(&self, ds: &DataSourceId) -> Result<QueryLanguage> {
        let cfg = self.source_config(ds)?;
        Ok(self.driver_for(&cfg)?.capabilities().language)
    }

    // ------------------------------------------------------------ producers

    /// Starts the producer of `source`.
    async fn produce(
        &self,
        source: TransferSource,
        want_docs: bool,
        token: CancellationToken,
        progress: &Progress,
    ) -> Result<(mpsc::Receiver<Chunk>, tokio::task::JoinHandle<Result<()>>)> {
        let (tx, rx) = mpsc::channel::<Chunk>(4);
        let handle = match source {
            TransferSource::File {
                path,
                format,
                options,
            } => self.handle().spawn_blocking(move || {
                let mut rd = import::open(&path, format, &options)?;
                tx.blocking_send(Chunk::Columns(rd.columns().to_vec()))
                    .map_err(|_| Error::Cancelled)?;
                loop {
                    if token.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    let chunk = if want_docs {
                        let d = rd.next_documents(CHUNK)?;
                        if d.is_empty() {
                            break;
                        }
                        Chunk::Docs(d)
                    } else {
                        let r = rd.next_rows(CHUNK)?;
                        if r.is_empty() {
                            break;
                        }
                        Chunk::Rows(r)
                    };
                    tx.blocking_send(chunk).map_err(|_| Error::Cancelled)?;
                }
                tx.blocking_send(Chunk::End).map_err(|_| Error::Cancelled)?;
                Ok(())
            }),
            TransferSource::Query {
                data_source,
                text,
                schema,
            } => {
                let lang = self.language_of(&data_source)?;
                let request = if is_mql(&lang) {
                    let cmd = mql::parse(text.trim()).map_err(|e| Error::Other(e.message))?;
                    if cmd.is_write() {
                        return Err(Error::Other(
                            "only reading statements can be exported or copied".into(),
                        ));
                    }
                    Request::Mql(cmd)
                } else {
                    let dialect = ferrodb_lang::service::dialect_of(&lang).to_owned();
                    let flavor = Flavor::for_dialect(&dialect);
                    let stmts = sql::split(&text, &flavor);
                    let [st] = stmts.as_slice() else {
                        return Err(Error::Other(format!(
                            "one statement is expected, found {}",
                            stmts.len()
                        )));
                    };
                    let body = st.text(&text).to_owned();
                    if sql::classify(&body, &dialect) != StatementClass::Read {
                        return Err(Error::Other(
                            "only reading statements can be exported or copied".into(),
                        ));
                    }
                    Request::Text { text: body }
                };
                self.spawn_db_producer(data_source, schema, request, tx, token, progress)
                    .await?
            }
            TransferSource::Object(node) => {
                let lang = self.language_of(&node.data_source)?;
                let (container, name) = match node.path.as_slice() {
                    [(_, c), (_, n), ..] => (Some(c.clone()), n.clone()),
                    [(_, n)] => (None, n.clone()),
                    _ => return Err(Error::Other("choose a table or collection".into())),
                };
                let request = if let Some(text) = crate::documents::select_all(&lang, &node) {
                    // Couchbase collection / DynamoDB table (phase 8).
                    Request::Text { text }
                } else if is_mql(&lang) {
                    let db = container.clone().unwrap_or_default();
                    Request::Mql(Command::on(
                        db,
                        Op::Find {
                            collection: name,
                            filter: Document::new(),
                            projection: None,
                            sort: None,
                            skip: None,
                            limit: None,
                            options: Document::new(),
                            one: false,
                            count: false,
                        },
                    ))
                } else {
                    let s = Sql::new(ferrodb_lang::service::dialect_of(&lang));
                    let table = match container.as_deref() {
                        Some("main") if s.family == Family::Sqlite => s.ident(&name),
                        c => s.qualified(c, &name),
                    };
                    Request::Text {
                        text: s.select_all(&table),
                    }
                };
                self.spawn_db_producer(node.data_source, None, request, tx, token, progress)
                    .await?
            }
        };
        Ok((rx, handle))
    }

    async fn spawn_db_producer(
        &self,
        ds: DataSourceId,
        schema: Option<String>,
        request: Request,
        tx: mpsc::Sender<Chunk>,
        token: CancellationToken,
        progress: &Progress,
    ) -> Result<tokio::task::JoinHandle<Result<()>>> {
        let mut conn = self.checkout(&ds).await?;
        if let Some(s) = schema.filter(|s| !s.is_empty()) {
            conn.set_schema(&s).await?;
        }
        let me = self.arc();
        let warn = progress.tx.clone();
        Ok(self.handle().spawn(async move {
            let (sink, mut rx) = ResultSink::channel(4, ResultSink::BATCH_ROWS);
            let exec = conn.execute(request, sink, token.clone());
            let fwd = async {
                let mut results = 0usize;
                let mut skipping = false;
                while let Some(ev) = rx.recv().await {
                    let chunk = match ev {
                        SinkEvent::Begin { schema } => {
                            results += 1;
                            skipping = results > 1;
                            if skipping {
                                continue;
                            }
                            Chunk::Table(schema)
                        }
                        SinkEvent::BeginDocuments { .. } => {
                            results += 1;
                            skipping = results > 1;
                            continue;
                        }
                        SinkEvent::Batch(b) if !skipping => Chunk::Batch(b),
                        SinkEvent::Documents(d) if !skipping => {
                            Chunk::Docs(d.iter().map(ferrodb_data::docs::decode).collect())
                        }
                        SinkEvent::Notice(n) => {
                            let _ = warn.try_send(TransferEvent::Warning(n.message));
                            continue;
                        }
                        _ => continue,
                    };
                    if tx.send(chunk).await.is_err() {
                        // The consumer stopped: dropping `rx` stops the driver.
                        return false;
                    }
                }
                if results > 1 {
                    let _ = warn.try_send(TransferEvent::Warning(format!(
                        "only the first result was transferred ({} more skipped)",
                        results - 1
                    )));
                }
                true
            };
            let (summary, delivered) = futures::join!(exec, fwd);
            me.checkin(&ds, conn);
            summary?;
            if delivered && !token.is_cancelled() {
                tx.send(Chunk::End).await.map_err(|_| Error::Cancelled)?;
            }
            Ok(())
        }))
    }

    // ------------------------------------------------------------ consumers

    #[allow(clippy::too_many_arguments)]
    async fn write_file(
        &self,
        mut rx: mpsc::Receiver<Chunk>,
        path: PathBuf,
        format: DataFormat,
        mut options: FormatOptions,
        dialect: String,
        default_table: Option<String>,
        token: CancellationToken,
        progress: Progress,
    ) -> Result<TransferReport> {
        if options.table.is_none() {
            options.table = default_table;
        }
        self.handle()
            .spawn_blocking(move || {
                let mut progress = progress;
                let mut ex = Exporter::create(&path, format, &options, &dialect)?;
                let mut ended = false;
                let r: Result<()> = (|| {
                    while let Some(c) = rx.blocking_recv() {
                        if token.is_cancelled() {
                            return Err(Error::Cancelled);
                        }
                        match c {
                            Chunk::Table(s) => {
                                ex.begin_table(&s)?;
                            }
                            Chunk::Batch(b) => {
                                ex.write_batch(&b)?;
                                progress.add(b.num_rows() as u64);
                            }
                            Chunk::Columns(cols) => {
                                let cols: Vec<(String, ValueType)> =
                                    cols.into_iter().map(|c| (c.name, c.ty)).collect();
                                ex.begin_rows(&cols)?;
                            }
                            Chunk::Rows(rows) => {
                                for r in &rows {
                                    ex.write_row(r)?;
                                }
                                progress.add(rows.len() as u64);
                            }
                            Chunk::Docs(d) => {
                                ex.write_decoded(&d)?;
                                progress.add(d.len() as u64);
                            }
                            Chunk::End => {
                                ended = true;
                                break;
                            }
                        }
                    }
                    Ok(())
                })();
                if let Err(e) = r {
                    drop(ex);
                    let _ = std::fs::remove_file(&path);
                    return Err(e);
                }
                if !ended {
                    drop(ex);
                    let _ = std::fs::remove_file(&path);
                    return Err(Error::Other("the source stopped".into()));
                }
                let stats = ex.finish()?;
                Ok(TransferReport {
                    items: stats.items,
                    bytes: stats.bytes,
                    method: format!("{} file", format.label()),
                    warnings: stats.warnings,
                    ..Default::default()
                })
            })
            .await
            .map_err(err)?
    }

    /// Columns (name, type) of an existing table; `None` when missing.
    async fn table_columns(
        &self,
        ds: &DataSourceId,
        container: &str,
        name: &str,
    ) -> Result<Option<(String, Vec<(String, ValueType)>)>> {
        let (_, md) = self.source_dialect(ds)?;
        let Some(md) = md else { return Ok(None) };
        let found = self.find_table(ds, md, container, name).await?.map(|t| {
            (
                t.name.clone(),
                t.columns
                    .iter()
                    .map(|c| (c.name.clone(), ValueType::from_sql(&c.data_type)))
                    .collect(),
            )
        });
        Ok(found)
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_table(
        &self,
        mut rx: mpsc::Receiver<Chunk>,
        ds: DataSourceId,
        container: Option<String>,
        name: String,
        create: bool,
        truncate: bool,
        mapping: Vec<ColumnMapping>,
        token: CancellationToken,
        mut progress: Progress,
    ) -> Result<TransferReport> {
        let cfg = self.source_config(&ds)?;
        if cfg.read_only {
            return Err(Error::ReadOnly);
        }
        let lang = self.language_of(&ds)?;
        let dialect = ferrodb_lang::service::dialect_of(&lang).to_owned();
        let sql = Sql::new(&dialect);
        let mut conn = self.checkout(&ds).await?;
        let container = match container.filter(|c| !c.is_empty()) {
            Some(c) => c,
            None => conn
                .current_schema()
                .await
                .ok()
                .flatten()
                .or_else(|| sql.dialect.default_schema.clone())
                .unwrap_or_else(|| "main".into()),
        };
        let r = self
            .fill_table(
                &mut rx,
                conn.as_mut(),
                &sql,
                &ds,
                &container,
                &name,
                create,
                truncate,
                &mapping,
                &token,
                &mut progress,
            )
            .await;
        if r.is_err() {
            let _ = conn.rollback().await;
        }
        self.checkin(&ds, conn);
        let report = r?;
        // Show the new / changed table in the explorer.
        if let Ok((_, Some(md))) = self.source_dialect(&ds) {
            let node = NodeRef::root(ds.clone())
                .child(ferrodb_meta::tree::container_kind(md), &container)
                .child(ObjectKind::Table, &name);
            let _ = self.refresh_scope(&ds, RefreshScope::Node(node)).await;
        }
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    async fn fill_table(
        &self,
        rx: &mut mpsc::Receiver<Chunk>,
        conn: &mut dyn Connection,
        sql: &Sql,
        ds: &DataSourceId,
        container: &str,
        name: &str,
        create: bool,
        truncate: bool,
        mapping: &[ColumnMapping],
        token: &CancellationToken,
        progress: &mut Progress,
    ) -> Result<TransferReport> {
        let mut report = TransferReport::default();
        let Some(first) = rx.recv().await else {
            return Err(Error::Other("the source stopped".into()));
        };
        let (src_cols, decimals) = columns_of(&first);
        let existing = self.table_columns(ds, container, name).await?;
        let table_name = existing
            .as_ref()
            .map_or_else(|| name.to_owned(), |(n, _)| n.clone());
        let table = if sql.family == Family::Sqlite && container == "main" {
            sql.ident(&table_name)
        } else {
            sql.qualified(Some(container), &table_name)
        };
        let target: Vec<(String, ValueType)> = match existing {
            Some((_, cols)) => cols,
            None if create => {
                if src_cols.is_empty() {
                    return Err(Error::Other("the source has no columns".into()));
                }
                let defs: Vec<ColumnDef> = src_cols
                    .iter()
                    .zip(&decimals)
                    .map(|(c, d)| ColumnDef {
                        name: c.name.clone(),
                        ty: if c.ty == ValueType::Any {
                            ValueType::Text
                        } else {
                            c.ty
                        },
                        decimal: *d,
                    })
                    .collect();
                let ddl = sql.create_table(&table, &defs);
                collect(conn, ddl.clone(), CancellationToken::new()).await?;
                report.statements.push(format!("{ddl};"));
                defs.into_iter().map(|d| (d.name, d.ty)).collect()
            }
            None => {
                return Err(Error::NotFound(format!(
                    "table {container}.{name} (choose \"create\" to create it)"
                )));
            }
        };
        let map = Mapping::new(&src_cols, &target, mapping)?;
        let columns: Vec<String> = map.names().iter().map(|c| sql.ident(c)).collect();
        if truncate {
            let t = sql.truncate(&table);
            collect(conn, t.clone(), CancellationToken::new()).await?;
            report.statements.push(format!("{t};"));
        }
        conn.begin().await?;
        let mut fast = conn.bulk_load_method();
        let mut buf: Vec<Vec<Value>> = Vec::new();
        let doc_cols: Vec<DocColumn> = src_cols
            .iter()
            .map(|c| DocColumn {
                path: c.name.clone(),
                ty: c.ty,
            })
            .collect();
        let mut row_no = 0u64;
        let mut ended = false;
        let mut next = Some(first);
        loop {
            let chunk = match next.take() {
                Some(c) => c,
                None => match rx.recv().await {
                    Some(c) => c,
                    None => break,
                },
            };
            if token.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let rows = match chunk {
                Chunk::Batch(b) => batch_rows(&b),
                Chunk::Rows(r) => r,
                Chunk::Docs(d) => d.iter().map(|d| document_to_row(d, &doc_cols)).collect(),
                Chunk::Table(_) | Chunk::Columns(_) => continue,
                Chunk::End => {
                    ended = true;
                    break;
                }
            };
            for r in rows {
                row_no += 1;
                buf.push(
                    map.row(&r)
                        .map_err(|e| Error::Other(format!("row {row_no}: {e}")))?,
                );
            }
            let limit = if fast.is_some() {
                BULK_ROWS
            } else {
                INSERT_ROWS * 4
            };
            if buf.len() >= limit {
                self.flush_rows(conn, sql, &table, &columns, &mut buf, &mut fast, progress)
                    .await?;
            }
        }
        if !ended {
            return Err(Error::Other("the source stopped".into()));
        }
        self.flush_rows(conn, sql, &table, &columns, &mut buf, &mut fast, progress)
            .await?;
        conn.commit().await?;
        report.items = progress.items;
        report.method = fast.unwrap_or("INSERT").to_owned();
        Ok(report)
    }

    #[allow(clippy::too_many_arguments)]
    async fn flush_rows(
        &self,
        conn: &mut dyn Connection,
        sql: &Sql,
        table: &str,
        columns: &[String],
        buf: &mut Vec<Vec<Value>>,
        fast: &mut Option<&'static str>,
        progress: &mut Progress,
    ) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        if let Some(method) = *fast {
            let mut data = Vec::with_capacity(buf.len() * 32);
            for r in buf.iter() {
                sql.copy_row(r, &mut data);
            }
            match conn.bulk_load(table, columns, data).await {
                Ok(_) => {
                    progress.add(buf.len() as u64);
                    buf.clear();
                    return Ok(());
                }
                Err(Error::Unsupported(why)) => {
                    progress.warn(format!("{method} is not available ({why}); using INSERT"));
                    *fast = None;
                }
                Err(e) => return Err(e),
            }
        }
        let names: Vec<String> = columns.to_vec();
        for part in buf.chunks(INSERT_ROWS) {
            let text = insert_quoted(sql, table, &names, part);
            collect(conn, text, CancellationToken::new()).await?;
            progress.add(part.len() as u64);
        }
        buf.clear();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_collection(
        &self,
        mut rx: mpsc::Receiver<Chunk>,
        ds: DataSourceId,
        container: Option<String>,
        name: String,
        truncate: bool,
        mapping: Vec<ColumnMapping>,
        token: CancellationToken,
        mut progress: Progress,
    ) -> Result<TransferReport> {
        let cfg = self.source_config(&ds)?;
        if cfg.read_only {
            return Err(Error::ReadOnly);
        }
        let mut conn = self.checkout(&ds).await?;
        let r = async {
            let db = match container.filter(|c| !c.is_empty()) {
                Some(c) => c,
                None => conn
                    .current_schema()
                    .await?
                    .ok_or_else(|| Error::Other("choose a database".into()))?,
            };
            let mut report = TransferReport {
                method: "insertMany".into(),
                ..Default::default()
            };
            if truncate {
                let cmd = Command::on(
                    &db,
                    Op::Delete {
                        collection: name.clone(),
                        filter: Document::new(),
                        many: true,
                    },
                );
                collect_request(conn.as_mut(), Request::Mql(cmd), CancellationToken::new()).await?;
                report.statements.push(format!(
                    "{}.deleteMany({{}})",
                    ferrodb_meta::mongo::collection_ref(&name)
                ));
            }
            let rename = |d: Document| -> Document {
                if mapping.is_empty() {
                    return d;
                }
                let mut out = Document::new();
                for m in &mapping {
                    if let Some(src) = &m.source
                        && let Some(v) = ferrodb_data::docs::lookup(&d, src)
                    {
                        out.insert(m.target.clone(), v.clone());
                    }
                }
                out
            };
            let mut names: Vec<String> = Vec::new();
            let mut buf: Vec<Document> = Vec::new();
            let mut ended = false;
            while let Some(chunk) = rx.recv().await {
                if token.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                match chunk {
                    Chunk::Table(s) => {
                        names = s.fields().iter().map(|f| f.name().clone()).collect();
                    }
                    Chunk::Columns(c) => names = c.into_iter().map(|c| c.name).collect(),
                    Chunk::Batch(b) => buf.extend(
                        batch_rows(&b)
                            .iter()
                            .map(|r| rename(row_to_document(&names, r))),
                    ),
                    Chunk::Rows(rows) => {
                        buf.extend(rows.iter().map(|r| rename(row_to_document(&names, r))))
                    }
                    Chunk::Docs(d) => buf.extend(d.into_iter().map(rename)),
                    Chunk::End => {
                        ended = true;
                        break;
                    }
                }
                while buf.len() >= INSERT_DOCS {
                    let rest = buf.split_off(INSERT_DOCS);
                    insert_many(conn.as_mut(), &db, &name, std::mem::replace(&mut buf, rest))
                        .await?;
                    progress.add(INSERT_DOCS as u64);
                }
            }
            if !ended {
                return Err(Error::Other("the source stopped".into()));
            }
            if !buf.is_empty() {
                let n = buf.len() as u64;
                insert_many(conn.as_mut(), &db, &name, buf).await?;
                progress.add(n);
            }
            report.items = progress.items;
            Ok(report)
        }
        .await;
        self.checkin(&ds, conn);
        r
    }

    async fn transfer(
        &self,
        req: TransferRequest,
        events: &async_channel::Sender<TransferEvent>,
        cancel: CancelHandle,
    ) -> Result<TransferReport> {
        let token = watch_cancel(self.handle(), cancel);
        let progress = Progress {
            tx: events.clone(),
            items: 0,
            reported: 0,
        };
        let want_docs = match &req.target {
            TransferTarget::Object { data_source, .. } => is_mql(&self.language_of(data_source)?),
            TransferTarget::File { .. } => false,
        };
        // SQL INSERT exports use the source's dialect and table name.
        let (dialect, default_table) = match &req.source {
            TransferSource::Query { data_source, .. } => (
                ferrodb_lang::service::dialect_of(&self.language_of(data_source)?).to_owned(),
                None,
            ),
            TransferSource::Object(n) => (
                ferrodb_lang::service::dialect_of(&self.language_of(&n.data_source)?).to_owned(),
                n.name().map(str::to_owned),
            ),
            TransferSource::File { path, .. } => (
                "sql".to_owned(),
                path.file_stem().and_then(|s| s.to_str()).map(str::to_owned),
            ),
        };
        let (rx, producer) = self
            .produce(req.source, want_docs, token.clone(), &progress)
            .await?;
        let consumer = match req.target {
            TransferTarget::File {
                path,
                format,
                options,
            } => {
                self.write_file(
                    rx,
                    path,
                    format,
                    options,
                    dialect,
                    default_table,
                    token.clone(),
                    progress,
                )
                .await
            }
            TransferTarget::Object {
                data_source,
                container,
                name,
                create,
                truncate,
                mapping,
            } => {
                if want_docs {
                    self.write_collection(
                        rx,
                        data_source,
                        container,
                        name,
                        truncate,
                        mapping,
                        token.clone(),
                        progress,
                    )
                    .await
                } else {
                    self.write_table(
                        rx,
                        data_source,
                        container,
                        name,
                        create,
                        truncate,
                        mapping,
                        token.clone(),
                        progress,
                    )
                    .await
                }
            }
        };
        if consumer.is_err() {
            token.cancel();
        }
        let produced = producer.await.map_err(err)?;
        token.cancel(); // stops the cancel watcher
        match (consumer, produced) {
            (Ok(r), Ok(())) => Ok(r),
            // The source's own error explains a stopped stream best.
            (Err(_), Err(e)) if !matches!(e, Error::Cancelled) => Err(e),
            (Err(e), _) => Err(e),
            (Ok(_), Err(e)) => Err(e),
        }
    }
}

fn insert_quoted(sql: &Sql, table: &str, columns: &[String], rows: &[Vec<Value>]) -> String {
    // `columns` are already quoted: build the statement without re-quoting.
    let mut s = format!("INSERT INTO {table} ({}) VALUES ", columns.join(", "));
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('(');
        for (j, v) in row.iter().enumerate() {
            if j > 0 {
                s.push_str(", ");
            }
            s.push_str(&sql.literal(v));
        }
        s.push(')');
    }
    s
}

async fn insert_many(
    conn: &mut dyn Connection,
    db: &str,
    coll: &str,
    docs: Vec<Document>,
) -> Result<()> {
    let cmd = Command::on(
        db,
        Op::Insert {
            collection: coll.to_owned(),
            documents: docs,
            many: true,
            ordered: true,
        },
    );
    collect_request(conn, Request::Mql(cmd), CancellationToken::new()).await?;
    Ok(())
}

impl TransferService for Inner {
    fn start(&self, request: TransferRequest) -> TransferHandle {
        let (tx, rx) = async_channel::bounded(256);
        let cancel = CancelHandle::new();
        let me = self.arc();
        let flag = cancel.clone();
        self.handle().spawn(async move {
            let started = std::time::Instant::now();
            let ev = match me.transfer(request, &tx, flag).await {
                Ok(mut r) => {
                    r.elapsed = started.elapsed();
                    TransferEvent::Finished(r)
                }
                Err(e) => TransferEvent::Failed(e.to_string()),
            };
            let _ = tx.send(ev).await;
        });
        TransferHandle { events: rx, cancel }
    }

    fn preview_file(
        &self,
        path: PathBuf,
        format: DataFormat,
        options: FormatOptions,
        rows: usize,
    ) -> BoxFuture<'_, Result<FilePreview>> {
        let h = self
            .handle()
            .spawn_blocking(move || import::preview(&path, format, &options, rows));
        Box::pin(async move { h.await.map_err(err)? })
    }

    fn target_columns(
        &self,
        data_source: DataSourceId,
        container: Option<String>,
        name: String,
    ) -> BoxFuture<'_, Result<Vec<String>>> {
        let me = self.arc();
        self.spawn(async move {
            let lang = me.language_of(&data_source)?;
            if is_mql(&lang) {
                let db = container.unwrap_or_default();
                return Ok(me.known_fields(&data_source, &db, &name));
            }
            let container = match container {
                Some(c) => c,
                None => {
                    let d =
                        ferrodb_lang::sql::Dialect::get(ferrodb_lang::service::dialect_of(&lang));
                    d.default_schema.clone().unwrap_or_else(|| "main".into())
                }
            };
            Ok(me
                .table_columns(&data_source, &container, &name)
                .await?
                .map(|(_, cols)| cols.into_iter().map(|c| c.0).collect())
                .unwrap_or_default())
        })
    }
}
