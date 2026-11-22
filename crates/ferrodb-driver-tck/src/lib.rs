//! # ferrodb-driver-tck
//!
//! The driver compatibility test suite (plan, sections 2.9 and 9; ADR-012).
//! One set of scenarios runs against every driver; expectations that differ
//! between databases come from a [`Profile`] (SQL snippets and the Arrow
//! types each database is expected to produce), never from `if driver ==`.
//!
//! v1 covers what phase 3 delivers: connection, statement execution with
//! faithful types, streaming with backpressure, cancellation, errors,
//! transactions, read-only sessions, schema switching and explorer listings.
//! Later phases add introspection, editing and export scenarios.
//!
//! v2 (phase 4b) adds the document model: [`run_documents`].
//!
//! v3 (phase 8) adds the key-value model: [`run_key_value`].

mod documents;
mod documents_sql;
mod key_value;

use std::time::{Duration, Instant};

pub use documents::run_documents;
pub use key_value::run_key_value;

use arrow_array::{Array, Int64Array, RecordBatch};
use arrow_schema::DataType;
use ferrodb_api::{DataModel, Error, ObjectKind};
use ferrodb_drivers::{
    CancellationToken, Connection, Driver, Request, ResolvedConfig, ResultSink, SinkEvent,
    sink::{Collected, collect},
};

/// What differs between databases, as data.
#[derive(Clone, Debug)]
pub struct Profile {
    /// `n` rows with a single integer column `i` from 1 to `n`.
    pub series: fn(u64) -> String,
    /// A statement that runs for about `secs` seconds.
    pub sleep: fn(u64) -> String,
    /// DDL of a table `name` with columns i, d, s, b, dt, ts.
    pub types_table: fn(&str) -> String,
    /// One row inserted into the types table.
    pub types_row: &'static str,
    /// Expected Arrow types of i, d, s, b, dt, ts.
    pub types: [DataType; 6],
    /// The driver reports error positions.
    pub error_positions: bool,
    /// The container level of the explorer tree (schema / database).
    pub container: ObjectKind,
}

/// What the generic SQL of the scenarios cannot express for a database
/// (CQL: keys required, no multi-row `VALUES`, no subqueries, no sleep), as
/// data. [`run`] uses the defaults (standard SQL).
#[derive(Clone, Debug)]
pub struct Adjustments {
    /// Statements run once before the scenarios (fixtures such as the
    /// table behind `series`).
    pub setup: fn() -> Vec<String>,
    /// `CREATE TABLE {t}` with one integer column `a`.
    pub int_table: fn(&str) -> String,
    /// Statements inserting rows with `a` = each value.
    pub insert_ints: fn(&str, &[i64]) -> Vec<String>,
    /// The `INSERT` of [`Profile::types_row`] into the types table.
    pub insert_types: fn(&str, &str) -> String,
    /// A query returning no rows and one integer column.
    pub empty_query: fn(&Profile) -> String,
    /// DML reports affected rows (Cassandra does not).
    pub affected_counts: bool,
    /// [`Profile::sleep`] really runs for a while; otherwise the cancel
    /// scenario cancels a stream stalled by backpressure.
    pub sleep: bool,
}

impl Default for Adjustments {
    fn default() -> Self {
        Self {
            setup: Vec::new,
            int_table: |t| format!("create table {t} (a integer)"),
            insert_ints: |t, v| {
                let rows: Vec<String> = v.iter().map(|i| format!("({i})")).collect();
                vec![format!("insert into {t} values {}", rows.join(", "))]
            },
            insert_types: |t, row| format!("insert into {t} values {row}"),
            empty_query: |p| format!("select i from ({}) s where i < 0", (p.series)(3)),
            affected_counts: true,
            sleep: true,
        }
    }
}

/// Outcome of one scenario.
#[derive(Clone, Debug)]
pub struct Outcome {
    pub scenario: &'static str,
    pub passed: bool,
    pub detail: String,
    pub elapsed: Duration,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub driver: String,
    pub server: String,
    pub outcomes: Vec<Outcome>,
}

impl Report {
    pub fn passed(&self) -> bool {
        !self.outcomes.is_empty() && self.outcomes.iter().all(|o| o.passed)
    }

    /// A Markdown table (used in `docs/phase-3.md`).
    pub fn markdown(&self) -> String {
        let mut s = format!(
            "**{}** — {}\n\n| Scenario | Result | Time | Detail |\n|---|---|---|---|\n",
            self.driver, self.server
        );
        for o in &self.outcomes {
            s.push_str(&format!(
                "| {} | {} | {} ms | {} |\n",
                o.scenario,
                if o.passed { "pass" } else { "**FAIL**" },
                o.elapsed.as_millis(),
                o.detail.replace('|', "\\|")
            ));
        }
        s
    }
}

fn table_name(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("tck_{tag}_{}_{nanos}", std::process::id())
}

async fn run_sql(
    conn: &mut dyn Connection,
    sql: &str,
) -> Result<ferrodb_drivers::sink::CollectedOutput, String> {
    collect(conn, sql, CancellationToken::new())
        .await
        .map_err(|e| format!("`{sql}`: {e}"))
}

fn single_i64(r: &Collected) -> Option<i64> {
    let b = r.batches.iter().find(|b| b.num_rows() > 0)?;
    let c = b.column(0);
    if let Some(a) = c.as_any().downcast_ref::<Int64Array>() {
        return Some(a.value(0));
    }
    arrow_cast_i64(c.as_ref())
}

fn arrow_cast_i64(a: &dyn Array) -> Option<i64> {
    use arrow_array::{Decimal128Array, Int16Array, Int32Array, StringArray, UInt64Array};
    let any = a.as_any();
    if let Some(x) = any.downcast_ref::<Int32Array>() {
        return Some(x.value(0) as i64);
    }
    if let Some(x) = any.downcast_ref::<Int16Array>() {
        return Some(x.value(0) as i64);
    }
    if let Some(x) = any.downcast_ref::<UInt64Array>() {
        return i64::try_from(x.value(0)).ok();
    }
    if let Some(x) = any.downcast_ref::<Decimal128Array>() {
        return i64::try_from(x.value(0) / 10i128.pow(x.scale().max(0) as u32)).ok();
    }
    if let Some(x) = any.downcast_ref::<StringArray>() {
        return x.value(0).parse().ok();
    }
    None
}

struct Ctx<'a> {
    driver: &'a dyn Driver,
    cfg: &'a ResolvedConfig,
    profile: &'a Profile,
    adj: &'a Adjustments,
    conn: Box<dyn Connection>,
}

impl Ctx<'_> {
    async fn count(&mut self, table: &str) -> Result<i64, String> {
        let out = run_sql(self.conn.as_mut(), &format!("select count(*) from {table}")).await?;
        out.results
            .first()
            .and_then(single_i64)
            .ok_or_else(|| "count(*) returned no integer".to_owned())
    }
}

/// Runs every scenario. Never panics; failures are reported.
pub async fn run(driver: &dyn Driver, cfg: &ResolvedConfig, profile: &Profile) -> Report {
    run_adjusted(driver, cfg, profile, &Adjustments::default()).await
}

/// [`run`] with database-specific statements. Scenarios a database cannot
/// support by its capabilities (transactions, affected counts, listing
/// queries) pass as "skipped" with the reason.
pub async fn run_adjusted(
    driver: &dyn Driver,
    cfg: &ResolvedConfig,
    profile: &Profile,
    adj: &Adjustments,
) -> Report {
    run_with(driver, cfg, profile, adj, &[]).await
}

/// [`run`] without the scenarios in `skip` (`(scenario, reason)`), for
/// databases where a scenario does not apply (ClickHouse reports no rows
/// affected by mutations). Skipped scenarios are listed as passed with the
/// reason.
pub async fn run_except(
    driver: &dyn Driver,
    cfg: &ResolvedConfig,
    profile: &Profile,
    skip: &[(&'static str, &str)],
) -> Report {
    run_with(driver, cfg, profile, &Adjustments::default(), skip).await
}

async fn run_with(
    driver: &dyn Driver,
    cfg: &ResolvedConfig,
    profile: &Profile,
    adj: &Adjustments,
    skip: &[(&'static str, &str)],
) -> Report {
    let mut report = Report {
        driver: driver.descriptor().name,
        ..Default::default()
    };
    let started = Instant::now();
    let conn = match driver.connect(cfg).await {
        Ok(c) => c,
        Err(e) => {
            report.outcomes.push(Outcome {
                scenario: "connect",
                passed: false,
                detail: e.to_string(),
                elapsed: started.elapsed(),
            });
            return report;
        }
    };
    report.server = conn.server_version();
    report.outcomes.push(Outcome {
        scenario: "connect",
        passed: !report.server.is_empty(),
        detail: report.server.clone(),
        elapsed: started.elapsed(),
    });
    let mut ctx = Ctx {
        driver,
        cfg,
        profile,
        adj,
        conn,
    };
    for st in (adj.setup)() {
        if let Err(e) = run_sql(ctx.conn.as_mut(), &st).await {
            report.outcomes.push(Outcome {
                scenario: "setup",
                passed: false,
                // Fixture statements are long: keep the error readable.
                detail: match e.char_indices().nth(160) {
                    Some((i, _)) => format!("{}… {}", &e[..i], e.rsplit(": ").next().unwrap_or("")),
                    None => e,
                },
                elapsed: started.elapsed(),
            });
            return report;
        }
    }
    macro_rules! scenario {
        ($name:literal, $fut:expr) => {{
            if let Some((_, why)) = skip.iter().find(|(n, _)| *n == $name) {
                report.outcomes.push(Outcome {
                    scenario: $name,
                    passed: true,
                    detail: format!("skipped: {why}"),
                    elapsed: Duration::ZERO,
                });
            } else {
                let t = Instant::now();
                let r = tokio::time::timeout(Duration::from_secs(60), $fut).await;
                let (passed, detail) = match r {
                    Ok(Ok(d)) => (true, d),
                    Ok(Err(e)) => (false, e),
                    Err(_) => (false, "timed out after 60 s".to_owned()),
                };
                report.outcomes.push(Outcome {
                    scenario: $name,
                    passed,
                    detail,
                    elapsed: t.elapsed(),
                });
                // A failed scenario may leave a transaction open.
                if !passed {
                    let _ = ctx.conn.rollback().await;
                }
            }
        }};
    }
    scenario!("descriptor", descriptor(&mut ctx));
    scenario!("ping", async {
        ctx.conn
            .ping()
            .await
            .map(|_| String::new())
            .map_err(|e| e.to_string())
    });
    scenario!("types", types(&mut ctx));
    scenario!("dml-affected", dml(&mut ctx));
    scenario!("empty-result-shape", empty_shape(&mut ctx));
    scenario!("streaming-100k", streaming(&mut ctx));
    scenario!("backpressure-truncate", truncate(&mut ctx));
    scenario!("cancel", cancel(&mut ctx));
    scenario!("errors", errors(&mut ctx));
    scenario!("transactions", transactions(&mut ctx));
    scenario!("schema", schema(&mut ctx));
    scenario!("explorer-listing", listing(&mut ctx));
    scenario!("read-only", read_only(&mut ctx));
    report
}

async fn descriptor(c: &mut Ctx<'_>) -> Result<String, String> {
    let d = c.driver.descriptor();
    if d.id != c.cfg.driver {
        return Err(format!("descriptor id {} != {}", d.id, c.cfg.driver));
    }
    if d.capabilities.data_model != DataModel::Tabular {
        return Err("phase-3 drivers are tabular".into());
    }
    if c.driver.variant().driver_id() != d.id {
        return Err("variant belongs to another driver".into());
    }
    Ok(format!("{} / {}", d.family, c.driver.variant().dialect()))
}

async fn types(c: &mut Ctx<'_>) -> Result<String, String> {
    let t = table_name("types");
    run_sql(c.conn.as_mut(), &(c.profile.types_table)(&t)).await?;
    let r = async {
        run_sql(
            c.conn.as_mut(),
            &(c.adj.insert_types)(&t, c.profile.types_row),
        )
        .await?;
        let out = run_sql(
            c.conn.as_mut(),
            &format!("select i, d, s, b, dt, ts from {t}"),
        )
        .await?;
        let res = out.results.first().ok_or("no result set")?;
        let got: Vec<DataType> = res
            .schema
            .fields()
            .iter()
            .map(|f| f.data_type().clone())
            .collect();
        if got != c.profile.types {
            return Err(format!("types {got:?}, expected {:?}", c.profile.types));
        }
        if res.num_rows() != 1 {
            return Err(format!("{} rows", res.num_rows()));
        }
        let b: &RecordBatch = res
            .batches
            .iter()
            .find(|b| b.num_rows() > 0)
            .ok_or("no batch")?;
        let nulls: Vec<usize> = (0..b.num_columns())
            .filter(|&i| b.column(i).is_null(0))
            .collect();
        if !nulls.is_empty() {
            return Err(format!("values lost (NULL) in columns {nulls:?}: {b:?}"));
        }
        let metadata = res
            .schema
            .field(1)
            .metadata()
            .get(ferrodb_drivers::columns::DB_TYPE_KEY)
            .cloned();
        Ok(format!(
            "{got:?}; d declared as {}",
            metadata.unwrap_or_default()
        ))
    }
    .await;
    let _ = run_sql(c.conn.as_mut(), &format!("drop table {t}")).await;
    r
}

async fn dml(c: &mut Ctx<'_>) -> Result<String, String> {
    if !c.adj.affected_counts {
        return Ok("skipped: the database reports no affected row counts".into());
    }
    let t = table_name("dml");
    run_sql(c.conn.as_mut(), &format!("create table {t} (a integer)")).await?;
    let r = async {
        let ins = run_sql(
            c.conn.as_mut(),
            &format!("insert into {t} values (1), (2), (3)"),
        )
        .await?;
        let upd = run_sql(
            c.conn.as_mut(),
            &format!("update {t} set a = a + 1 where a >= 2"),
        )
        .await?;
        let del = run_sql(c.conn.as_mut(), &format!("delete from {t} where a = 1")).await?;
        let got = (
            ins.summary.affected,
            upd.summary.affected,
            del.summary.affected,
        );
        if got != (Some(3), Some(2), Some(1)) {
            return Err(format!("affected {got:?}"));
        }
        if !ins.results.is_empty() {
            return Err("INSERT produced a result set".into());
        }
        Ok("3 / 2 / 1".into())
    }
    .await;
    let _ = run_sql(c.conn.as_mut(), &format!("drop table {t}")).await;
    r
}

async fn empty_shape(c: &mut Ctx<'_>) -> Result<String, String> {
    let out = run_sql(c.conn.as_mut(), &(c.adj.empty_query)(c.profile)).await?;
    let r = out
        .results
        .first()
        .ok_or("no result set for an empty query")?;
    if r.num_rows() != 0 || r.schema.fields().len() != 1 {
        return Err(format!(
            "{} rows, {} columns",
            r.num_rows(),
            r.schema.fields().len()
        ));
    }
    Ok("1 column, 0 rows".into())
}

async fn streaming(c: &mut Ctx<'_>) -> Result<String, String> {
    let n = 100_000u64;
    let t = Instant::now();
    let out = run_sql(c.conn.as_mut(), &(c.profile.series)(n)).await?;
    let r = out.results.first().ok_or("no result")?;
    if r.num_rows() as u64 != n {
        return Err(format!("{} rows, expected {n}", r.num_rows()));
    }
    let max = r
        .batches
        .iter()
        .map(RecordBatch::num_rows)
        .max()
        .unwrap_or(0);
    if r.batches.len() < 2 || max > ResultSink::BATCH_ROWS {
        return Err(format!("{} batches, largest {max} rows", r.batches.len()));
    }
    Ok(format!(
        "{} batches in {} ms",
        r.batches.len(),
        t.elapsed().as_millis()
    ))
}

async fn truncate(c: &mut Ctx<'_>) -> Result<String, String> {
    let (sink, mut rx) = ResultSink::channel(2, 100);
    let sql = (c.profile.series)(1_000_000);
    let exec = c
        .conn
        .execute(Request::Text { text: sql }, sink, CancellationToken::new());
    let reader = async move {
        let mut rows = 0;
        while let Some(ev) = rx.recv().await {
            if let SinkEvent::Batch(b) = ev {
                rows += b.num_rows();
                if rows >= 500 {
                    break; // the consumer goes away
                }
            }
        }
        rows
    };
    let t = Instant::now();
    let (summary, rows) = futures::join!(exec, reader);
    let summary = summary.map_err(|e| e.to_string())?;
    if !summary.truncated {
        return Err("summary not marked truncated".into());
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable after truncation: {e}"))?;
    Ok(format!(
        "stopped after {rows} rows in {} ms",
        t.elapsed().as_millis()
    ))
}

async fn cancel(c: &mut Ctx<'_>) -> Result<String, String> {
    if !c.adj.sleep {
        return cancel_stalled(c).await;
    }
    let token = CancellationToken::new();
    let t2 = token.clone();
    let sql = (c.profile.sleep)(30);
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        t2.cancel();
        Instant::now()
    };
    let (r, at) = futures::join!(collect(c.conn.as_mut(), sql, token), canceller);
    let took = at.elapsed();
    match r {
        Err(Error::Cancelled) => {}
        other => return Err(format!("expected Cancelled, got {other:?}")),
    }
    if took > Duration::from_secs(1) {
        return Err(format!("cancel took {} ms", took.as_millis()));
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable after cancel: {e}"))?;
    Ok(format!("{} ms", took.as_millis()))
}

/// Cancels a statement whose consumer stopped reading (databases without a
/// sleep function): the driver must give up within a second.
async fn cancel_stalled(c: &mut Ctx<'_>) -> Result<String, String> {
    let token = CancellationToken::new();
    let t2 = token.clone();
    let (sink, rx) = ResultSink::channel(1, 100);
    let sql = (c.profile.series)(1_000_000);
    let exec = c.conn.execute(Request::Text { text: sql }, sink, token);
    let canceller = async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        t2.cancel();
        Instant::now()
    };
    let (r, at) = futures::join!(exec, canceller);
    let took = at.elapsed();
    drop(rx);
    match r {
        Err(Error::Cancelled) => {}
        other => return Err(format!("expected Cancelled, got {other:?}")),
    }
    if took > Duration::from_secs(1) {
        return Err(format!("cancel took {} ms", took.as_millis()));
    }
    c.conn
        .ping()
        .await
        .map_err(|e| format!("connection unusable after cancel: {e}"))?;
    Ok(format!(
        "stalled stream cancelled in {} ms",
        took.as_millis()
    ))
}

async fn errors(c: &mut Ctx<'_>) -> Result<String, String> {
    let sql = "select * from tck_no_such_table";
    match collect(c.conn.as_mut(), sql, CancellationToken::new()).await {
        Err(Error::Query(q)) => {
            if !q.message.to_ascii_lowercase().contains("tck_no_such_table")
                && !q.message.contains("exist")
            {
                return Err(format!("unhelpful message: {}", q.message));
            }
            if c.profile.error_positions {
                let r = q.range.clone().ok_or("no error position")?;
                if !sql[r.start.min(sql.len())..].starts_with("tck_no_such_table") {
                    return Err(format!("position {r:?} does not point at the table"));
                }
            }
            c.conn.ping().await.map_err(|e| e.to_string())?;
            Ok(format!("{} ({:?})", q.message, q.code))
        }
        other => Err(format!("expected a query error, got {other:?}")),
    }
}

async fn transactions(c: &mut Ctx<'_>) -> Result<String, String> {
    // Databases without transactions (ClickHouse, BigQuery, Cassandra)
    // must refuse BEGIN instead of pretending.
    if !c.driver.capabilities().supports_transactions() {
        return match c.conn.begin().await {
            Err(e) => Ok(format!(
                "no transactions ({:?}); BEGIN refused: {e}",
                c.driver.capabilities().transactions
            )),
            Ok(()) => Err("a database without transactions accepted BEGIN".into()),
        };
    }
    let t = table_name("tx");
    run_sql(c.conn.as_mut(), &(c.adj.int_table)(&t)).await?;
    let r = async {
        c.conn.begin().await.map_err(|e| e.to_string())?;
        for st in (c.adj.insert_ints)(&t, &[1]) {
            run_sql(c.conn.as_mut(), &st).await?;
        }
        c.conn.rollback().await.map_err(|e| e.to_string())?;
        let after_rollback = c.count(&t).await?;
        c.conn.begin().await.map_err(|e| e.to_string())?;
        for st in (c.adj.insert_ints)(&t, &[2]) {
            run_sql(c.conn.as_mut(), &st).await?;
        }
        c.conn.commit().await.map_err(|e| e.to_string())?;
        let after_commit = c.count(&t).await?;
        if (after_rollback, after_commit) != (0, 1) {
            return Err(format!(
                "rows after rollback {after_rollback}, after commit {after_commit}"
            ));
        }
        Ok("rollback discards, commit keeps".into())
    }
    .await;
    let _ = run_sql(c.conn.as_mut(), &format!("drop table {t}")).await;
    r
}

async fn schema(c: &mut Ctx<'_>) -> Result<String, String> {
    let cur = c
        .conn
        .current_schema()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no current schema")?;
    c.conn.set_schema(&cur).await.map_err(|e| e.to_string())?;
    let again = c.conn.current_schema().await.map_err(|e| e.to_string())?;
    if again.as_deref() != Some(cur.as_str()) {
        return Err(format!("{again:?} after switching to {cur}"));
    }
    Ok(cur)
}

async fn listing(c: &mut Ctx<'_>) -> Result<String, String> {
    let v = c.driver.variant();
    if v.list_queries(&[]).is_empty() {
        return Ok("skipped: the explorer uses the introspector, not listing queries".into());
    }
    if v.hierarchy().levels.first().map(|l| l.kind) != Some(c.profile.container) {
        return Err("unexpected first hierarchy level".into());
    }
    let cur = c
        .conn
        .current_schema()
        .await
        .map_err(|e| e.to_string())?
        .ok_or("no current schema")?;
    let t = table_name("list");
    run_sql(
        c.conn.as_mut(),
        &format!("create table {t} (col_a integer)"),
    )
    .await?;
    let r = async {
        let mut found = Vec::new();
        for q in v.list_queries(&[]) {
            let out = run_sql(c.conn.as_mut(), &q.sql).await?;
            if out.results.first().map(Collected::num_rows).unwrap_or(0) == 0 {
                return Err(format!("no {:?} listed", q.kind));
            }
        }
        let path = vec![(c.profile.container, cur.clone())];
        for q in v.list_queries(&path) {
            let out = run_sql(c.conn.as_mut(), &q.sql).await?;
            let names = names(&out);
            // The name as listed (upper case where the database folds to it).
            if q.kind == ObjectKind::Table
                && let Some(n) = names.iter().find(|n| n.eq_ignore_ascii_case(&t))
            {
                found.push(n.clone());
            }
        }
        if found.is_empty() {
            return Err(format!("{t} not listed under {cur}"));
        }
        let path = vec![
            (c.profile.container, cur.clone()),
            (ObjectKind::Table, found[0].clone()),
        ];
        let cols: Vec<String> = {
            let mut v2 = Vec::new();
            for q in v.list_queries(&path) {
                v2.extend(names(&run_sql(c.conn.as_mut(), &q.sql).await?));
            }
            v2
        };
        // Databases folding to upper case (Oracle, Db2, H2, Derby…) list `COL_A`.
        if cols.len() != 1 || !cols[0].eq_ignore_ascii_case("col_a") {
            return Err(format!("columns {cols:?}"));
        }
        Ok(format!("{cur} → {t} → col_a"))
    }
    .await;
    let _ = run_sql(c.conn.as_mut(), &format!("drop table {t}")).await;
    r
}

fn names(out: &ferrodb_drivers::sink::CollectedOutput) -> Vec<String> {
    use arrow_array::StringArray;
    let mut v = Vec::new();
    for r in &out.results {
        for b in &r.batches {
            if let Some(s) = b.column(0).as_any().downcast_ref::<StringArray>() {
                v.extend((0..s.len()).map(|i| s.value(i).to_owned()));
            }
        }
    }
    v
}

async fn read_only(c: &mut Ctx<'_>) -> Result<String, String> {
    let t = table_name("ro");
    run_sql(c.conn.as_mut(), &(c.adj.int_table)(&t)).await?;
    let r = async {
        let cfg = ResolvedConfig {
            read_only: true,
            ..c.cfg.clone()
        };
        let mut ro = c.driver.connect(&cfg).await.map_err(|e| e.to_string())?;
        ro.set_read_only(true).await.map_err(|e| e.to_string())?;
        let reads = run_sql(ro.as_mut(), &format!("select count(*) from {t}")).await;
        let insert = (c.adj.insert_ints)(&t, &[1]).remove(0);
        let writes = collect(ro.as_mut(), insert, CancellationToken::new()).await;
        match (reads, writes) {
            (Ok(_), Err(e)) => Ok(format!("write rejected: {e}")),
            (Err(e), _) => Err(format!("read failed: {e}")),
            (Ok(_), Ok(_)) => Err("write accepted on a read-only session".into()),
        }
    }
    .await;
    let _ = run_sql(c.conn.as_mut(), &format!("drop table {t}")).await;
    r
}

/// Built-in profiles of the phase-3 databases.
pub mod profiles {
    use arrow_schema::{DataType, TimeUnit};
    use ferrodb_api::ObjectKind;

    use super::Profile;

    fn recursive_series(n: u64) -> String {
        format!(
            "with recursive s(i) as (select 1 union all select i + 1 from s where i < {n}) select i from s"
        )
    }

    pub fn sqlite() -> Profile {
        Profile {
            series: recursive_series,
            // No sleep function: an endless computation stands in.
            sleep: |_| {
                "with recursive r(i) as (select 1 union all select i + 1 from r) select count(*) from r".into()
            },
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', 1, '2026-11-09', '2026-11-09 10:11:12')",
            // Dates are stored as text in SQLite; decimals are REAL.
            types: [
                DataType::Int64,
                DataType::Float64,
                DataType::Utf8,
                DataType::Boolean,
                DataType::Utf8,
                DataType::Utf8,
            ],
            error_positions: false,
            container: ObjectKind::Schema,
        }
    }

    pub fn postgresql() -> Profile {
        Profile {
            series: |n| format!("select i::int8 as i from generate_series(1, {n}) as g(i)"),
            sleep: |s| format!("select pg_sleep({s})"),
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d numeric(10,2), s varchar(20), b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
            types: [
                DataType::Int32,
                DataType::Decimal128(10, 2),
                DataType::Utf8,
                DataType::Boolean,
                DataType::Date32,
                DataType::Timestamp(TimeUnit::Microsecond, None),
            ],
            error_positions: true,
            container: ObjectKind::Schema,
        }
    }

    /// Apache Cassandra: `series` reads the `tck_series` fixture table
    /// filled by [`cassandra_adjustments`] (CQL has no row generator).
    pub fn cassandra() -> Profile {
        Profile {
            series: |n| format!("select i from tck_series limit {n}"),
            // No sleep: the cancel scenario stalls a stream instead.
            sleep: |_| "select i from tck_series".into(),
            types_table: |t| {
                format!(
                    "create table {t} (i int primary key, d decimal, s text, b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12+0000')",
            // decimal: arbitrary precision and scale per value, as text.
            types: [
                DataType::Int32,
                DataType::Utf8,
                DataType::Utf8,
                DataType::Boolean,
                DataType::Date32,
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            ],
            error_positions: false,
            container: ObjectKind::Keyspace,
        }
    }

    /// Rows of the `tck_series` fixture.
    pub const CASSANDRA_SERIES_ROWS: i64 = 100_000;

    pub fn cassandra_adjustments() -> super::Adjustments {
        super::Adjustments {
            setup: || {
                let mut v =
                    vec!["create table if not exists tck_series (i bigint primary key)".to_owned()];
                let per = 250;
                for chunk in 0..(CASSANDRA_SERIES_ROWS / per) {
                    let mut b = String::from("begin unlogged batch ");
                    for i in chunk * per..(chunk + 1) * per {
                        b.push_str(&format!("insert into tck_series (i) values ({}); ", i + 1));
                    }
                    b.push_str("apply batch");
                    v.push(b);
                }
                v
            },
            int_table: |t| format!("create table {t} (a int primary key)"),
            insert_ints: |t, v| {
                v.iter()
                    .map(|i| format!("insert into {t} (a) values ({i})"))
                    .collect()
            },
            insert_types: |t, row| format!("insert into {t} (i, d, s, b, dt, ts) values {row}"),
            empty_query: |_| "select i from tck_series where i = -1".into(),
            affected_counts: false,
            sleep: false,
        }
    }

    /// CockroachDB: PostgreSQL's profile, but `integer` is 64-bit and errors
    /// carry no position.
    pub fn cockroachdb() -> Profile {
        let mut p = postgresql();
        p.types[0] = DataType::Int64;
        p.error_positions = false;
        p
    }

    /// Greenplum and Redshift: PostgreSQL's profile without positions.
    pub fn postgresql_derivative() -> Profile {
        Profile {
            error_positions: false,
            ..postgresql()
        }
    }

    /// SQL Server and Azure SQL (T-SQL).
    pub fn sqlserver() -> Profile {
        Profile {
            series: |n| {
                format!(
                    "select top ({n}) cast(row_number() over (order by (select null)) as bigint) as i \
                     from sys.all_columns a cross join sys.all_columns b cross join sys.all_columns c"
                )
            },
            sleep: |s| {
                format!(
                    "waitfor delay '{:02}:{:02}:{:02}'",
                    s / 3600,
                    (s / 60) % 60,
                    s % 60
                )
            },
            types_table: |t| {
                format!(
                    "create table {t} (i int, d decimal(10,2), s nvarchar(20), b bit, dt date, ts datetime2)"
                )
            },
            types_row: "(42, 12.5, N'zażółć', 1, '2026-11-09', '2026-11-09 10:11:12')",
            types: [
                DataType::Int32,
                DataType::Decimal128(10, 2),
                DataType::Utf8,
                DataType::Boolean,
                DataType::Date32,
                DataType::Timestamp(TimeUnit::Microsecond, None),
            ],
            // Errors point at the line, not the token.
            error_positions: false,
            container: ObjectKind::Schema,
        }
    }

    /// H2 through the JDBC bridge (embedded, in memory).
    pub fn h2() -> Profile {
        Profile {
            series: |n| format!("select x as i from system_range(1, {n})"),
            sleep: |_| {
                "select count(*) from system_range(1, 1000000000) a, system_range(1, 1000) b".into()
            },
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
            types: jdbc_types(),
            error_positions: true,
            container: ObjectKind::Schema,
        }
    }

    /// Apache Derby through the JDBC bridge (embedded, in memory). Derby
    /// has no row generator and no statement cancel: the series numbers a
    /// cross join of a system table, and the long statement streams rows so
    /// the bridge can stop between them.
    pub fn derby() -> Profile {
        Profile {
            series: |n| {
                format!(
                    "select row_number() over () as i from sys.syscolumns a, sys.syscolumns b, sys.syscolumns c                      fetch first {n} rows only"
                )
            },
            sleep: |_| {
                "select a.columnname from sys.syscolumns a, sys.syscolumns b, sys.syscolumns c, sys.syscolumns d"
                    .into()
            },
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
            types: jdbc_types(),
            error_positions: true,
            container: ObjectKind::Schema,
        }
    }

    /// HyperSQL through the JDBC bridge (embedded, in memory).
    pub fn hsqldb() -> Profile {
        Profile {
            series: |n| format!("select c as i from unnest(sequence_array(1, {n}, 1)) as t(c)"),
            sleep: |_| {
                "select count(*) from unnest(sequence_array(1, 100000, 1)) as a(x),                  unnest(sequence_array(1, 100000, 1)) as b(y)"
                    .into()
            },
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
            types: jdbc_types(),
            error_positions: true,
            container: ObjectKind::Schema,
        }
    }

    /// Oracle (23ai Free) through the JDBC bridge. `DATE` carries a time
    /// in Oracle: the driver reports it as a timestamp.
    pub fn oracle() -> Profile {
        Profile {
            series: |n| {
                format!("select cast(level as number(10)) as i from dual connect by level <= {n}")
            },
            sleep: |s| format!("begin dbms_session.sleep({s}); end;"),
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d number(10,2), s varchar2(20), b boolean, dt date, ts timestamp)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, date '2026-11-09', timestamp '2026-11-09 10:11:12')",
            types: [
                DataType::Decimal128(38, 0),
                DataType::Decimal128(10, 2),
                DataType::Utf8,
                DataType::Boolean,
                DataType::Timestamp(TimeUnit::Microsecond, None),
                DataType::Timestamp(TimeUnit::Microsecond, None),
            ],
            error_positions: true,
            container: ObjectKind::Schema,
        }
    }

    /// What the JDBC bridge produces for the types table.
    fn jdbc_types() -> [DataType; 6] {
        [
            DataType::Int32,
            DataType::Decimal128(10, 2),
            DataType::Utf8,
            DataType::Boolean,
            DataType::Date32,
            DataType::Timestamp(TimeUnit::Microsecond, None),
        ]
    }

    /// ClickHouse: `MergeTree` tables (the server's default engine);
    /// `DateTime` arrives as a timestamp in the server time zone.
    pub fn clickhouse() -> Profile {
        Profile {
            series: |n| format!("select toInt64(number + 1) as i from numbers({n})"),
            // `sleep` stops at 3 s per block: one-row blocks of 0.5 s each.
            sleep: |s| {
                format!(
                    "select sleepEachRow(0.5) from numbers({}) settings max_block_size = 1",
                    s * 2
                )
            },
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts timestamp) \
                     engine = MergeTree order by tuple()"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
            types: [
                DataType::Int32,
                DataType::Decimal128(10, 2),
                DataType::Utf8,
                DataType::Boolean,
                DataType::Date32,
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            ],
            error_positions: false,
            container: ObjectKind::Database,
        }
    }

    pub fn mysql() -> Profile {
        Profile {
            series: |n| {
                // Recursion depth defaults to 1000; a cross join avoids it.
                format!(
                    "select cast(a.n * 1000 + b.n + 1 as signed) as i from \
                     (with recursive t(n) as (select 0 union all select n + 1 from t where n < 999) select n from t) a, \
                     (with recursive t(n) as (select 0 union all select n + 1 from t where n < 999) select n from t) b \
                     where a.n * 1000 + b.n < {n}"
                )
            },
            sleep: |s| format!("select sleep({s})"),
            types_table: |t| {
                format!(
                    "create table {t} (i integer, d decimal(10,2), s varchar(20), b boolean, dt date, ts datetime)"
                )
            },
            types_row: "(42, 12.5, 'zażółć', true, '2026-11-09', '2026-11-09 10:11:12')",
            types: [
                DataType::Int64,
                DataType::Decimal128(10, 2),
                DataType::Utf8,
                DataType::Int64,
                DataType::Date32,
                DataType::Timestamp(TimeUnit::Microsecond, None),
            ],
            error_positions: false,
            container: ObjectKind::Database,
        }
    }
}
