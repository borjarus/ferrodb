//! Phase 0 ADBC spike (plan, section 7): load an ADBC driver via
//! `adbc_driver_manager`, execute a query and consume Arrow record batches.
//!
//! The driver library is not bundled (FerroDb downloads ADBC drivers on demand).
//! Get the SQLite driver, e.g. `pip download adbc-driver-sqlite` / conda
//! `libadbc-driver-sqlite`, or build it from apache/arrow-adbc, then:
//!
//! ```text
//! cargo xtask spike adbc -- <path-or-name-of-driver> [uri] [rows]
//! cargo xtask spike adbc -- adbc_driver_sqlite ":memory:" 1000000
//! ```
//!
//! With the SQLite driver the spike generates `rows` rows with a recursive CTE
//! and reports rows/s, batch count and schema — the same path Snowflake and
//! BigQuery will take (results arrive directly as Arrow).

use std::{path::Path, time::Instant};

use adbc_core::{
    Connection, Database, Driver, Statement,
    options::{AdbcVersion, OptionDatabase},
};
use adbc_driver_manager::ManagedDriver;
use arrow_array::RecordBatchReader;

fn main() {
    if let Err(e) = run() {
        eprintln!("spike-adbc: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(driver_arg) = args.first() else {
        return Err("usage: spike-adbc <driver library path or name> [uri] [rows]".into());
    };
    let uri = args.get(1).cloned().unwrap_or_else(|| ":memory:".into());
    let rows: u64 = args
        .get(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);

    let t = Instant::now();
    let mut driver = if Path::new(driver_arg).exists() {
        ManagedDriver::load_dynamic_from_filename(driver_arg, None, AdbcVersion::V110)?
    } else {
        ManagedDriver::load_dynamic_from_name(driver_arg, None, AdbcVersion::V110)?
    };
    println!("[adbc] driver loaded in {:?}", t.elapsed());

    let database = driver.new_database_with_opts([(OptionDatabase::Uri, uri.as_str().into())])?;
    let mut connection = database.new_connection()?;
    let mut statement = connection.new_statement()?;
    statement.set_sql_query(format!(
        "WITH RECURSIVE r(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM r WHERE i < {rows}) \
         SELECT i AS id, 'name ' || i AS name, i * 0.5 AS amount, i % 2 = 0 AS flag FROM r"
    ))?;

    let t = Instant::now();
    let reader = statement.execute()?;
    println!(
        "[adbc] schema: {:?}",
        reader
            .schema()
            .fields()
            .iter()
            .map(|f| f.to_string())
            .collect::<Vec<_>>()
    );
    let (mut total, mut batches, mut first) = (0usize, 0usize, None);
    for batch in reader {
        let batch = batch?;
        if first.is_none() {
            first = Some(t.elapsed());
        }
        total += batch.num_rows();
        batches += 1;
    }
    let elapsed = t.elapsed();
    println!(
        "[adbc] {total} rows in {batches} batches, first batch after {:?}, total {:?} → {:.0} rows/s",
        first.unwrap_or_default(),
        elapsed,
        total as f64 / elapsed.as_secs_f64()
    );
    Ok(())
}
