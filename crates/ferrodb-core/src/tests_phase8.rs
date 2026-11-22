//! Phase 8 (analytical and cloud databases) through the whole backend.
//! Without servers: the ADBC drivers in the system registry. With
//! `FERRODB_TCK_CLICKHOUSE` (URL, see the TCK): ClickHouse plans, result
//! edits (mutations, no transaction), cost estimates (none) and the
//! explorer.

#![allow(clippy::disallowed_methods)]

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

fn core(dir: &std::path::Path) -> CoreBackend {
    CoreBackend::new(CoreOptions {
        config_dir: Some(dir.join("config")),
        data_dir: Some(dir.join("data")),
        ..CoreOptions::in_memory()
    })
    .unwrap()
}

fn run(core: &CoreBackend, s: SessionId, text: &str) -> Vec<QueryEvent> {
    let h = core
        .queries()
        .execute(s, Script::new(text), ExecOptions::default());
    let out = block_on(async {
        let mut out = Vec::new();
        while let Ok(ev) = h.events.recv().await {
            let done = matches!(ev, QueryEvent::Finished);
            out.push(ev);
            if done {
                break;
            }
        }
        out
    });
    let errs: Vec<String> = out
        .iter()
        .filter_map(|e| match e {
            QueryEvent::Error(q) => Some(q.to_string()),
            _ => None,
        })
        .collect();
    assert!(errs.is_empty(), "{text}: {errs:?}");
    out
}

fn query(core: &CoreBackend, s: SessionId, text: &str) -> (ResultId, Vec<Vec<Option<String>>>) {
    let mut rid = None;
    let mut rows = Vec::new();
    for e in run(core, s, text) {
        match e {
            QueryEvent::ResultStarted { result, .. } => rid = Some(result),
            QueryEvent::Data {
                chunk: ResultChunk::Rows(b),
                ..
            } => {
                for r in 0..b.num_rows() {
                    rows.push(
                        b.columns()
                            .iter()
                            .map(|c| ferrodb_data::value::value_at(c.as_ref(), r).to_text())
                            .collect(),
                    );
                }
            }
            _ => {}
        }
    }
    (rid.expect("a result"), rows)
}

#[test]
fn adbc_drivers_are_in_the_system_registry() {
    let dir = tempfile::tempdir().unwrap();
    let opts = CoreOptions::system(dir.path().join("config"), dir.path().join("data"));
    for id in ["snowflake", "bigquery"] {
        let d = opts.registry.get(&id.into()).expect(id);
        assert_eq!(d.capabilities().access, AccessLayer::Adbc);
    }
}

#[test]
fn clickhouse_plans_edits_and_explorer() {
    let Ok(url) = std::env::var("FERRODB_TCK_CLICKHOUSE") else {
        eprintln!("FERRODB_TCK_CLICKHOUSE not set: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let c = core(dir.path());
    let resolved = ferrodb_drivers::ResolvedConfig::from_url("clickhouse", &url).unwrap();
    let cfg = DataSourceConfig {
        id: "ch".into(),
        name: "ch".into(),
        driver: "clickhouse".into(),
        folder: vec![],
        environment: Environment::Development,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: resolved.properties.clone().into_iter().collect(),
        secret_ref: None,
    };
    block_on(c.data_sources().save(cfg, resolved.secrets.clone())).unwrap();
    let s = block_on(c.queries().open_session(&"ch".into())).unwrap();
    let t = format!("p8_edit_{}", std::process::id());
    run(&c, s, &format!("drop table if exists {t}"));
    run(
        &c,
        s,
        &format!(
            "create table {t} (id UInt32, name String, note Nullable(String)) \
             engine = MergeTree order by id"
        ),
    );
    run(
        &c,
        s,
        &format!("insert into {t} values (1, 'a', null), (2, 'b', 'x'), (3, 'c', null)"),
    );

    // Plan.
    let plan = block_on(c.queries().explain(
        s,
        Script::new(format!("select name from {t} where id = 2")),
        ExplainOptions::default(),
    ))
    .unwrap();
    assert!(plan.statement.starts_with("EXPLAIN json = 1"));
    let flat = plan.root.flatten();
    assert!(
        flat.iter().any(|(_, n)| n.operation == "ReadFromMergeTree"
            && n.properties.iter().any(|(k, _)| k == "Index PrimaryKey")),
        "{}",
        plan.raw
    );
    assert!(
        block_on(c.queries().explain(
            s,
            Script::new("select 1"),
            ExplainOptions {
                analyze: true,
                schema: None
            }
        ))
        .is_err()
    );

    // No cost estimate on ClickHouse.
    assert_eq!(
        block_on(c.queries().estimate_cost(s, Script::new("select 1"))).unwrap(),
        None
    );

    // Edits: mutations, applied without a transaction.
    let (rid, rows) = query(
        &c,
        s,
        &format!("select id, name, note from {t} order by id"),
    );
    assert_eq!(rows.len(), 3);
    let q = c.queries();
    let info = block_on(q.edit_info(rid)).unwrap();
    assert_eq!(info.identity, RowIdentity::Key(vec!["id".into()]));
    let changes = ChangeSet {
        changes: vec![
            Change::SetCell {
                row: 0,
                column: 2,
                value: "\"it's\"".into(),
            },
            Change::DeleteRow { row: 1 },
        ],
    };
    let preview = block_on(q.preview_changes(rid, changes.clone())).unwrap();
    assert!(
        preview[0].starts_with(&format!(
            "ALTER TABLE tck.{t} UPDATE note = 'it\\'s' WHERE id = 1"
        )),
        "{preview:?}"
    );
    assert_eq!(preview[1], format!("DELETE FROM tck.{t} WHERE id = 2;"));
    let report = block_on(q.apply_changes(rid, changes)).unwrap();
    assert_eq!((report.applied, report.pending_commit), (2, false));
    let (_, rows) = query(&c, s, &format!("select id, note from {t} order by id"));
    assert_eq!(
        rows,
        vec![
            vec![Some("1".into()), Some("it's".into())],
            vec![Some("3".into()), None]
        ]
    );
    run(&c, s, &format!("drop table {t}"));
}
