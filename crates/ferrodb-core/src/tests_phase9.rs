//! Phase 9 extension scripts through the whole backend on SQLite: export
//! of a query result through a built-in and a user extractor (transfer
//! service), aggregators and in-memory extraction via `Backend::scripts`.

#![allow(clippy::disallowed_methods)]

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

struct Fx {
    core: CoreBackend,
    dir: tempfile::TempDir,
}

const USER_SCRIPT: &str = r##"// name: Tagged lines
// description: <id>=<name> per line
// extension: tag

fn header(columns) {
    "# " + join(columns.map(|c| c.name + ":" + c.type), ", ") + "\n"
}

fn row(values, columns, index) {
    `${index}: ${values[0]}=${text(values[1], options().null_text)}` + "\n"
}

fn footer(count) {
    `# ${count} rows`
}
"##;

fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let ex = dir.path().join("config").join("scripts").join("extractors");
    std::fs::create_dir_all(&ex).unwrap();
    std::fs::write(ex.join("tagged.rhai"), USER_SCRIPT).unwrap();
    let core = CoreBackend::new(CoreOptions {
        config_dir: Some(dir.path().join("config")),
        data_dir: Some(dir.path().join("data")),
        ..CoreOptions::in_memory()
    })
    .unwrap();
    let cfg = DataSourceConfig {
        id: "a".into(),
        name: "a".into(),
        driver: "sqlite".into(),
        folder: vec![],
        environment: Environment::Development,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: vec![(
            "file".into(),
            dir.path().join("a.sqlite").to_string_lossy().into_owned(),
        )],
        secret_ref: None,
    };
    block_on(core.data_sources().save(cfg, SecretValues::new())).unwrap();
    let f = Fx { core, dir };
    let s = block_on(f.core.queries().open_session(&"a".into())).unwrap();
    let h = f.core.queries().execute(
        s,
        Script::new(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT, price NUMERIC);\n\
             INSERT INTO items VALUES (1, 'apple', 1.5), (2, NULL, 2), (3, 'it''s', 10.25);",
        ),
        ExecOptions::default(),
    );
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            if let QueryEvent::Error(e) = &ev {
                panic!("{e}");
            }
            if matches!(ev, QueryEvent::Finished) {
                break;
            }
        }
    });
    f
}

impl Fx {
    fn export(
        &self,
        file: &str,
        options: FormatOptions,
    ) -> Result<(TransferReport, String), String> {
        let path = self.dir.path().join(file);
        let h = self.core.transfers().start(TransferRequest {
            source: TransferSource::Query {
                data_source: "a".into(),
                text: "SELECT id, name, price FROM items ORDER BY id".into(),
                schema: None,
            },
            target: TransferTarget::File {
                path: path.clone(),
                format: DataFormat::Script,
                options,
            },
        });
        let r = block_on(async {
            while let Ok(ev) = h.events.recv().await {
                match ev {
                    TransferEvent::Finished(r) => return Ok(r),
                    TransferEvent::Failed(e) => return Err(e),
                    _ => {}
                }
            }
            Err("no outcome".into())
        })?;
        Ok((r, std::fs::read_to_string(&path).unwrap()))
    }
}

fn script(id: &str) -> FormatOptions {
    FormatOptions {
        script: Some(id.into()),
        ..Default::default()
    }
}

#[test]
fn export_through_builtin_and_user_extractors() {
    let f = fx();
    let (r, text) = f.export("ids.sql", script("builtin:sql-in-list")).unwrap();
    assert_eq!(text, "(1, 2, 3)");
    assert_eq!(r.items, 3);
    assert_eq!(r.method, "SQL IN list (script)");
    assert_eq!(r.bytes, 9);

    let (_, text) = f.export("rows.sql", script("builtin:sql-values")).unwrap();
    assert_eq!(
        text,
        "(1, 'apple', 1.5),\n(2, NULL, 2),\n(3, 'it''s', 10.25)\n"
    );

    // The user script from <config>/scripts, with the export options.
    let opts = FormatOptions {
        null_text: "?".into(),
        ..script("tagged")
    };
    let (r, text) = f.export("items.tag", opts).unwrap();
    assert_eq!(r.method, "Tagged lines (script)");
    assert!(
        text.starts_with("# id:integer, name:text, price:"),
        "{text}"
    );
    assert!(
        text.ends_with("\n0: 1=apple\n1: 2=?\n2: 3=it's\n# 3 rows"),
        "{text}"
    );

    // Unknown or missing scripts fail clearly and leave no file.
    let e = f.export("x.txt", script("nope")).unwrap_err();
    assert!(e.contains("extractor script `nope`"), "{e}");
    let e = f.export("y.txt", FormatOptions::default()).unwrap_err();
    assert!(e.contains("no script"), "{e}");
    // A script that fails mid-export.
    std::fs::write(
        f.dir.path().join("config/scripts/extractors/broken.rhai"),
        "fn row(values) { values[7] }",
    )
    .unwrap();
    let e = f.export("z.txt", script("broken")).unwrap_err();
    assert!(e.contains("script `broken`"), "{e}");
    assert!(!f.dir.path().join("z.txt").exists());
}

#[test]
fn scripts_service_through_the_backend() {
    let f = fx();
    let s = f.core.scripts();
    assert!(s.available());
    assert_eq!(
        s.directory(),
        Some(f.dir.path().join("config").join("scripts"))
    );
    let ex = s.list(ExtScriptKind::Extractor);
    assert!(ex.iter().any(|i| i.id == "builtin:json-keyed"));
    let user = ex.iter().find(|i| i.id == "tagged").unwrap();
    assert_eq!(user.extension, "tag");
    assert!(!user.builtin);

    let cells = ScriptTable {
        columns: vec!["n".into()],
        types: vec!["integer".into()],
        rows: (1..=5).map(|i| vec![ScriptValue::Int(i)]).collect(),
    };
    assert_eq!(
        block_on(s.aggregate("builtin:median", cells.clone())).unwrap(),
        "3"
    );
    assert_eq!(
        block_on(s.aggregate("builtin:concat", cells.clone())).unwrap(),
        "1, 2, 3, 4, 5"
    );
    assert_eq!(
        block_on(s.extract("builtin:sql-in-list", cells.clone())).unwrap(),
        "(1, 2, 3, 4, 5)"
    );
    assert!(block_on(s.aggregate("builtin:sql-in-list", cells.clone())).is_err());

    // A new aggregator from the template runs right away.
    let p = block_on(s.create(ExtScriptKind::Aggregator, "Average")).unwrap();
    assert!(p.ends_with("scripts/aggregators/average.rhai"));
    assert_eq!(block_on(s.aggregate("average", cells)).unwrap(), "3");
    block_on(s.reload()).unwrap();
    assert!(
        s.list(ExtScriptKind::Aggregator)
            .iter()
            .any(|i| i.id == "average" && i.error.is_none())
    );
}
