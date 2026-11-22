//! Editing tabular results (plan, section 5.6 and phase 6).
//!
//! Every tabular result keeps the record batches it streamed (the same
//! `Arc`ed arrays the UI holds, so no copy), up to [`MAX_ROWS`] rows. When
//! the user edits it, the statement is analysed: a `SELECT` from one table,
//! without `GROUP BY` / `DISTINCT` / set operations, is editable. The table
//! comes from the metadata catalog; rows are identified by its primary or
//! unique key, or — when the table has none or the result lacks the key
//! columns — by all their values. Statements run in one transaction (auto
//! mode, on a pooled connection) or inside the session's open transaction
//! under a savepoint (manual mode); every statement must change exactly one
//! row, otherwise everything is rolled back.
//!
//! The same analysis gives "related rows" navigation along foreign keys and
//! the quick "by value" filter.

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use ferrodb_api::{
    ApplyReport, ChangeSet, DataSourceId, EditInfo, Error, Navigation, NodeRef, ObjectKind, Result,
    ResultId, RowIdentity, Script, SessionId,
};
use ferrodb_data::{
    Value, ValueType,
    edit::{self, EditTable, Planned},
    sql::{Family, Sql},
    value::value_at,
};
use ferrodb_drivers::{CancellationToken, Connection, sink::collect};
use ferrodb_lang::sql::{
    document::Document,
    lexer::TokenKind,
    schema::SchemaModel,
    syntax::{Clause, SourceKind, StmtKind, names_eq},
};
use ferrodb_meta::{Table, tree::container_kind};

use crate::Inner;

/// Rows kept per result for editing.
pub(crate) const MAX_ROWS: usize = 1_000_000;
/// Tabular results kept per session (older ones are no longer editable).
const PER_SESSION: usize = 16;
/// Savepoint used for edits inside a manual transaction.
pub(crate) const SAVEPOINT: &str = "ferrodb_edit";

/// A streamed tabular result.
#[derive(Clone, Debug)]
pub(crate) struct TableResult {
    pub session: SessionId,
    pub data_source: DataSourceId,
    pub dialect: String,
    pub statement: String,
    /// The session's schema / database when the statement ran.
    pub schema: Option<String>,
    pub arrow: SchemaRef,
    pub batches: Vec<RecordBatch>,
    pub rows: usize,
}

impl TableResult {
    fn value(&self, mut row: usize, col: usize) -> Option<Value> {
        for b in &self.batches {
            if row < b.num_rows() {
                return Some(value_at(b.column(col).as_ref(), row));
            }
            row -= b.num_rows();
        }
        None
    }
}

/// What the analysis found.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub edit: EditTable,
    pub info: EditInfo,
    pub table: Table,
    pub container: String,
    pub sql: Sql,
}

impl Inner {
    pub(crate) fn register_table(&self, rid: ResultId, r: TableResult) {
        let mut st = self.state();
        let session = r.session;
        st.tables.insert(rid, r);
        let mut mine: Vec<ResultId> = st
            .tables
            .iter()
            .filter(|(_, t)| t.session == session)
            .map(|(id, _)| *id)
            .collect();
        if mine.len() > PER_SESSION {
            mine.sort();
            for id in &mine[..mine.len() - PER_SESSION] {
                st.tables.remove(id);
            }
        }
    }

    pub(crate) fn record_rows(&self, rid: ResultId, batch: &RecordBatch) {
        let mut st = self.state();
        if let Some(t) = st.tables.get_mut(&rid)
            && t.rows + batch.num_rows() <= MAX_ROWS
        {
            t.rows += batch.num_rows();
            t.batches.push(batch.clone());
        }
    }

    /// A table of the catalog; a container cached before the table existed
    /// is re-read once.
    pub(crate) async fn find_table(
        &self,
        ds: &DataSourceId,
        md: ferrodb_meta::Dialect,
        container: &str,
        name: &str,
    ) -> Result<Option<Table>> {
        let lookup = |cat: &ferrodb_meta::Catalog| {
            let content = cat.container(container)?.content.as_ref()?;
            content
                .table(name)
                .or_else(|| {
                    content
                        .tables
                        .iter()
                        .find(|t| t.name.eq_ignore_ascii_case(name))
                })
                .cloned()
        };
        let cat = self.ensure_content(ds, md, container).await?;
        if let Some(t) = lookup(&cat) {
            return Ok(Some(t));
        }
        let node = NodeRef::root(ds.clone()).child(container_kind(md), container);
        self.refresh_scope(ds, ferrodb_api::RefreshScope::Node(node))
            .await?;
        Ok(lookup(&self.catalog(ds)))
    }

    fn table_result(&self, rid: ResultId) -> Option<TableResult> {
        self.state().tables.get(&rid).cloned()
    }

    /// Analyses the statement of a result against the catalog.
    pub(crate) async fn edit_target(&self, r: &TableResult) -> Result<Target> {
        let not = |why: &str| Error::Unsupported(format!("editing this result: {why}"));
        let model = SchemaModel::default();
        let doc = Document::analyze(&r.statement, &r.dialect, &model, r.schema.as_deref());
        let st = doc.stmts.first().ok_or_else(|| not("no statement"))?;
        let p = &st.parsed;
        let d = ferrodb_lang::sql::Dialect::get(&r.dialect);
        if p.kind != StmtKind::Select {
            return Err(not("only SELECT results can be edited"));
        }
        let top = p.scopes.first().ok_or_else(|| not("no query"))?;
        if p.scopes.iter().skip(1).any(|s| s.cte_owner == Some(0)) {
            return Err(not("the query combines results (UNION, INTERSECT, EXCEPT)"));
        }
        if p.marks
            .iter()
            .any(|m| m.scope == 0 && matches!(m.clause, Clause::GroupBy | Clause::Having))
        {
            return Err(not("the query groups rows"));
        }
        let distinct = st.toks.iter().any(|t| {
            t.kind == TokenKind::Word
                && t.text(&r.statement).eq_ignore_ascii_case("distinct")
                && p.at(t.range.start).0 == 0
        });
        if distinct {
            return Err(not("the query uses DISTINCT"));
        }
        let [source] = top.sources.as_slice() else {
            return Err(not("the query reads more than one table"));
        };
        let SourceKind::Table(parts) = &source.kind else {
            return Err(not("the query does not read a table"));
        };
        let name = parts.last().ok_or_else(|| not("no table"))?.name(d);
        let (_, meta_dialect) = self.source_dialect(&r.data_source)?;
        let md = meta_dialect.ok_or_else(|| not("no metadata for this source"))?;
        let explicit_container = (parts.len() >= 2).then(|| parts[parts.len() - 2].name(d));
        let container = match explicit_container.clone() {
            Some(c) => c,
            None => r
                .schema
                .clone()
                .or_else(|| d.default_schema.clone())
                .unwrap_or_else(|| "main".into()),
        };
        let table = self
            .find_table(&r.data_source, md, &container, &name)
            .await?
            .ok_or_else(|| not(&format!("table {container}.{name} is not in the catalog")))?;
        if table.is_view() {
            return Err(not(&format!("{} is a view", table.name)));
        }
        // Result column → table column.
        let fields = r.arrow.fields();
        let col_of = |n: &str| {
            table
                .columns
                .iter()
                .find(|c| names_eq(&c.name, n, d) || c.name == n)
                .map(|c| c.name.clone())
        };
        let star = top.outputs.iter().any(|o| o.star.is_some());
        let columns: Vec<Option<String>> = if star || top.outputs.len() != fields.len() {
            fields.iter().map(|f| col_of(f.name())).collect()
        } else {
            top.outputs
                .iter()
                .map(|o| {
                    o.column_ref
                        .and_then(|i| p.refs.get(i))
                        .and_then(|r| r.parts.last())
                        .and_then(|id| col_of(&id.name(d)))
                })
                .collect()
        };
        if columns.iter().all(Option::is_none) {
            return Err(not("no column of the result is a table column"));
        }
        let index_of = |c: &str| columns.iter().position(|x| x.as_deref() == Some(c));
        let (key, identity, warning) = match table.row_key() {
            Some(k) if k.iter().all(|c| index_of(c).is_some()) => (
                k.iter().filter_map(|c| index_of(c)).collect(),
                RowIdentity::Key(k.to_vec()),
                None,
            ),
            other => {
                let why = match other {
                    Some(k) => format!("the result lacks the key column(s) {}", k.join(", ")),
                    None => format!("{} has no primary or unique key", table.name),
                };
                let key: Vec<usize> = columns
                    .iter()
                    .enumerate()
                    .filter(|(i, c)| {
                        c.is_some()
                            && !matches!(
                                ValueType::from_arrow(fields[*i].data_type()),
                                ValueType::Float | ValueType::Bytes | ValueType::Json
                            )
                    })
                    .map(|(i, _)| i)
                    .collect();
                if key.is_empty() {
                    return Err(not(&format!("{why}, and no column can identify rows")));
                }
                (
                    key,
                    RowIdentity::AllColumns,
                    Some(format!(
                        "{why}: rows are matched by all their values; a change that would affect more than one row is refused"
                    )),
                )
            }
        };
        let types: Vec<ValueType> = columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                c.as_deref()
                    .and_then(|c| table.column(c))
                    .map(|c| ValueType::from_sql(&c.data_type))
                    .unwrap_or_else(|| ValueType::from_arrow(fields[i].data_type()))
            })
            .collect();
        let sql = Sql::new(&r.dialect);
        let qualified = if sql.family == Family::Sqlite && container == "main" {
            sql.ident(&table.name)
        } else {
            sql.qualified(Some(&container), &table.name)
        };
        let object = NodeRef::root(r.data_source.clone())
            .child(container_kind(md), &container)
            .child(ObjectKind::Table, &table.name);
        Ok(Target {
            edit: EditTable {
                table: qualified,
                columns: columns.clone(),
                types,
                key,
            },
            info: EditInfo {
                object,
                columns,
                identity,
                warning,
            },
            table,
            container,
            sql,
        })
    }

    async fn planned(
        &self,
        rid: ResultId,
        changes: &ChangeSet,
    ) -> Result<(TableResult, Target, Vec<Planned>)> {
        let r = self.table_result(rid).ok_or_else(|| {
            Error::Unsupported(
                "editing this result (it is no longer kept; run the query again)".into(),
            )
        })?;
        let t = self.edit_target(&r).await?;
        let planned = edit::plan(&t.sql, &t.edit, changes, |row, col| r.value(row, col))
            .map_err(Error::Other)?;
        Ok((r, t, planned))
    }

    pub(crate) fn is_table_result(&self, rid: ResultId) -> bool {
        self.state().tables.contains_key(&rid)
    }

    pub(crate) async fn preview_table(
        &self,
        rid: ResultId,
        changes: &ChangeSet,
    ) -> Result<Vec<String>> {
        let (_, t, planned) = self.planned(rid, changes).await?;
        let mut out = Vec::new();
        if let Some(w) = &t.info.warning {
            out.push(format!("-- {w}"));
        }
        out.extend(planned.into_iter().map(|p| format!("{};", p.sql)));
        Ok(out)
    }

    pub(crate) async fn apply_table(
        &self,
        rid: ResultId,
        changes: ChangeSet,
    ) -> Result<ApplyReport> {
        let (r, _, planned) = self.planned(rid, &changes).await?;
        let cfg = self.source_config(&r.data_source)?;
        if cfg.read_only {
            return Err(Error::ReadOnly);
        }
        let statements: Vec<String> = planned.iter().map(|p| format!("{};", p.sql)).collect();
        let manual = cfg.tx_mode == ferrodb_api::TxMode::Manual
            && self
                .driver_for(&cfg)?
                .capabilities()
                .supports_transactions();
        if manual {
            let applied = self.apply_in_session(r.session, planned).await?;
            return Ok(ApplyReport {
                applied,
                statements,
                pending_commit: true,
            });
        }
        let mut conn = self.checkout(&r.data_source).await?;
        if let Some(s) = r.schema.as_deref() {
            let _ = conn.set_schema(s).await;
        }
        // Sources without transactions (ClickHouse, BigQuery) apply the
        // statements one by one.
        let res =
            if self.driver_for(&cfg)?.capabilities().transactions == ferrodb_api::TxSupport::None {
                run_planned(conn.as_mut(), &planned).await
            } else {
                run_in_transaction(conn.as_mut(), &planned).await
            };
        self.checkin(&r.data_source, conn);
        Ok(ApplyReport {
            applied: res?,
            statements,
            pending_commit: false,
        })
    }

    pub(crate) async fn table_edit_info(&self, rid: ResultId) -> Result<EditInfo> {
        let r = self
            .table_result(rid)
            .ok_or_else(|| Error::Unsupported("editing this result".into()))?;
        Ok(self.edit_target(&r).await?.info)
    }

    pub(crate) async fn table_navigations(&self, rid: ResultId) -> Result<Vec<Navigation>> {
        let Some(r) = self.table_result(rid) else {
            return Ok(Vec::new());
        };
        let t = match self.edit_target(&r).await {
            Ok(t) => t,
            Err(_) => return Ok(Vec::new()),
        };
        let index_of = |c: &str| t.edit.columns.iter().position(|x| x.as_deref() == Some(c));
        let kind = t.info.object.path[0].0;
        let node = |container: &str, name: &str| {
            NodeRef::root(r.data_source.clone())
                .child(kind, container)
                .child(ObjectKind::Table, name)
        };
        let mut out = Vec::new();
        for fk in &t.table.foreign_keys {
            let cols: Option<Vec<usize>> = fk.columns.iter().map(|c| index_of(c)).collect();
            let Some(cols) = cols else { continue };
            let container = fk
                .ref_container
                .clone()
                .unwrap_or_else(|| t.container.clone());
            out.push(Navigation {
                label: format!(
                    "{} → {}({})",
                    fk.columns.join(", "),
                    fk.ref_table,
                    fk.ref_columns.join(", ")
                ),
                object: node(&container, &fk.ref_table),
                columns: cols,
                related_columns: fk.ref_columns.clone(),
                outgoing: true,
            });
        }
        // Tables referencing this one, in every loaded container.
        let cat = self.catalog(&r.data_source);
        for (c, content) in cat.loaded() {
            for other in &content.tables {
                for fk in &other.foreign_keys {
                    let refs_here = fk.ref_table == t.table.name
                        && fk.ref_container.as_deref().unwrap_or(&c.name) == t.container;
                    if !refs_here {
                        continue;
                    }
                    let cols: Option<Vec<usize>> =
                        fk.ref_columns.iter().map(|c| index_of(c)).collect();
                    let Some(cols) = cols else { continue };
                    out.push(Navigation {
                        label: format!(
                            "{}.{} → {}",
                            other.name,
                            fk.columns.join(", "),
                            t.table.name
                        ),
                        object: node(&c.name, &other.name),
                        columns: cols,
                        related_columns: fk.columns.clone(),
                        outgoing: false,
                    });
                }
            }
        }
        Ok(out)
    }

    pub(crate) fn table_related_script(
        &self,
        rid: ResultId,
        nav: &Navigation,
        values: &[String],
    ) -> Result<Script> {
        let r = self
            .table_result(rid)
            .ok_or_else(|| Error::NotFound(format!("result {rid}")))?;
        let sql = Sql::new(&r.dialect);
        let (container, name) = match nav.object.path.as_slice() {
            [(_, c), (_, n)] => (Some(c.as_str()), n.as_str()),
            [(_, n)] => (None, n.as_str()),
            _ => return Err(Error::Other("related table unknown".into())),
        };
        let table = if sql.family == Family::Sqlite && container == Some("main") {
            sql.ident(name)
        } else {
            sql.qualified(container, name)
        };
        if values.len() != nav.related_columns.len() {
            return Err(Error::Other(
                "one value per related column is needed".into(),
            ));
        }
        let conds: Vec<String> = nav
            .related_columns
            .iter()
            .zip(values)
            .map(|(c, v)| {
                Value::from_cell_json(v, ValueType::Any)
                    .map(|v| sql.condition(c, &v))
                    .map_err(Error::Other)
            })
            .collect::<Result<_>>()?;
        Ok(Script::new(format!(
            "SELECT * FROM {table} WHERE {}",
            conds.join(" AND ")
        )))
    }

    pub(crate) fn table_value_filter(
        &self,
        rid: ResultId,
        column: &str,
        value: &str,
    ) -> Option<Result<String>> {
        let dialect = self.state().tables.get(&rid)?.dialect.clone();
        let sql = Sql::new(&dialect);
        Some(
            Value::from_cell_json(value, ValueType::Any)
                .map(|v| sql.condition(column, &v))
                .map_err(Error::Other),
        )
    }
}

/// Runs `planned` on `conn` in its own transaction.
pub(crate) async fn run_in_transaction(
    conn: &mut dyn Connection,
    planned: &[Planned],
) -> Result<usize> {
    conn.begin().await?;
    match run_planned(conn, planned).await {
        Ok(n) => {
            conn.commit().await?;
            Ok(n)
        }
        Err(e) => {
            let _ = conn.rollback().await;
            Err(e)
        }
    }
}

/// Runs `planned` statement by statement, checking that each changes
/// exactly one row. Returns the rows changed.
pub(crate) async fn run_planned(conn: &mut dyn Connection, planned: &[Planned]) -> Result<usize> {
    let mut applied = 0usize;
    for (i, p) in planned.iter().enumerate() {
        let out = collect(conn, p.sql.clone(), CancellationToken::new()).await?;
        // ClickHouse mutations report no count: nothing to check.
        let n = out.summary.affected.unwrap_or(u64::from(p.expect_one));
        if p.expect_one && n != 1 {
            return Err(Error::Other(format!(
                "statement {} would change {n} rows instead of 1; nothing was applied: {}",
                i + 1,
                p.sql
            )));
        }
        applied += n as usize;
    }
    Ok(applied)
}

/// Statements within an open transaction, under a savepoint.
pub(crate) async fn run_with_savepoint(
    conn: &mut dyn Connection,
    planned: &[Planned],
) -> Result<usize> {
    collect(
        conn,
        format!("SAVEPOINT {SAVEPOINT}"),
        CancellationToken::new(),
    )
    .await?;
    match run_planned(conn, planned).await {
        Ok(n) => {
            let _ = collect(
                conn,
                format!("RELEASE SAVEPOINT {SAVEPOINT}"),
                CancellationToken::new(),
            )
            .await;
            Ok(n)
        }
        Err(e) => {
            let _ = collect(
                conn,
                format!("ROLLBACK TO SAVEPOINT {SAVEPOINT}"),
                CancellationToken::new(),
            )
            .await;
            Err(e)
        }
    }
}
