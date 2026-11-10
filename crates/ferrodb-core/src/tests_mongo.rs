//! Phase 4b document flows through the whole backend on a real MongoDB
//! (`FERRODB_TCK_MONGODB`, see `cargo xtask db-up`): console script with
//! streaming and paging, explorer tree, schema inference, field
//! completion, editing through the operation preview, read-only mode,
//! risks, manual transactions, and the SSH tunnel (`FERRODB_TCK_SSH`).

#![allow(clippy::disallowed_methods)]

use std::time::Duration;

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

fn source(
    id: &str,
    props: Vec<(String, String)>,
    edit: impl FnOnce(&mut DataSourceConfig),
) -> DataSourceConfig {
    let mut c = DataSourceConfig {
        id: id.into(),
        name: id.into(),
        driver: "mongodb".into(),
        folder: vec![],
        environment: Environment::None,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: props,
        secret_ref: None,
    };
    edit(&mut c);
    c
}

fn events(core: &CoreBackend, s: SessionId, text: &str, opts: ExecOptions) -> Vec<QueryEvent> {
    let h = core.queries().execute(s, Script::new(text), opts);
    let mut out = Vec::new();
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            let done = matches!(ev, QueryEvent::Finished);
            out.push(ev);
            if done {
                break;
            }
        }
    });
    out
}

fn errors(evs: &[QueryEvent]) -> Vec<String> {
    evs.iter()
        .filter_map(|e| match e {
            QueryEvent::Error(q) => Some(q.to_string()),
            _ => None,
        })
        .collect()
}

fn docs(evs: &[QueryEvent]) -> Vec<bson::Document> {
    evs.iter()
        .flat_map(|e| match e {
            QueryEvent::Data {
                chunk: ResultChunk::Documents(d),
                ..
            } => d
                .iter()
                .filter_map(|x| bson::Document::try_from(x.clone()).ok())
                .collect(),
            _ => Vec::new(),
        })
        .collect()
}

#[test]
fn document_flows_on_a_real_server() {
    let Ok(url) = std::env::var("FERRODB_TCK_MONGODB") else {
        return;
    };
    let core = CoreBackend::new(CoreOptions::in_memory()).unwrap();
    let props = vec![
        ("uri".to_owned(), url.clone()),
        ("database".to_owned(), "flows".to_owned()),
    ];
    let cfg = source("mongo", props.clone(), |_| {});
    block_on(core.data_sources().save(cfg.clone(), SecretValues::new())).unwrap();
    let ds = cfg.id.clone();
    let caps = core.data_sources().get(&ds).unwrap().capabilities;
    assert_eq!(caps.data_model, DataModel::Document);
    let q = core.queries();
    let s = block_on(q.open_session(&ds)).unwrap();

    // Script with several statements, a title and write results.
    let evs = events(
        &core,
        s,
        "db.dropDatabase()\n\
         db.people.insertMany([{name: 'Ala', age: 30, address: {city: 'Kraków'}}, {name: 'Ola', age: 25, tags: ['x']}])\n\
         db.people.createIndex({name: 1})\n\
         // adults\n\
         db.people.find({age: {$gte: 18}}).sort({age: 1})",
        ExecOptions::default(),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    let affected: Vec<Option<u64>> = evs
        .iter()
        .filter_map(|e| match e {
            QueryEvent::StatementFinished { affected, .. } => Some(*affected),
            _ => None,
        })
        .collect();
    assert_eq!(affected[1], Some(2));
    let (rid, node, title) = evs
        .iter()
        .rev()
        .find_map(|e| match e {
            QueryEvent::ResultStarted {
                result,
                shape: ResultShape::Documents { node, .. },
                title,
            } => Some((*result, node.clone(), title.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(title.as_deref(), Some("adults"));
    let node = node.expect("the collection node is known");
    assert_eq!(
        node.path,
        vec![
            (ObjectKind::Database, "flows".into()),
            (ObjectKind::Collection, "people".into())
        ]
    );
    let found = docs(&evs);
    let last = &found[found.len() - 2..];
    assert_eq!(last[0].get_str("name").unwrap(), "Ola");

    // Editing through the operation preview.
    let changes = ChangeSet {
        changes: vec![Change::SetField {
            document: 0,
            path: "address.city".into(),
            value: "\"Gdańsk\"".into(),
        }],
    };
    let preview = block_on(q.preview_changes(rid, changes.clone())).unwrap();
    assert!(
        preview[0].starts_with("db.people.updateOne({ _id: ObjectId("),
        "{preview:?}"
    );
    let report = block_on(q.apply_changes(rid, changes)).unwrap();
    assert_eq!(report.applied, 1);
    let evs = events(
        &core,
        s,
        "db.people.findOne({name: 'Ola'})",
        ExecOptions::default(),
    );
    assert_eq!(
        docs(&evs)[0]
            .get_document("address")
            .unwrap()
            .get_str("city")
            .unwrap(),
        "Gdańsk"
    );

    // Paging: 3000 documents, first page 500, then fetch_more.
    let many: Vec<String> = (0..3000).map(|i| format!("{{n: {i}}}")).collect();
    events(
        &core,
        s,
        &format!("db.many.insertMany([{}])", many.join(",")),
        ExecOptions::default(),
    );
    let h = q.execute(s, Script::new("db.many.find({})"), ExecOptions::default());
    let mut got = 0;
    let mut paused = None;
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            match ev {
                QueryEvent::Data { chunk, .. } => got += chunk.len(),
                QueryEvent::ResultPaused {
                    result,
                    has_more: true,
                } => paused = Some(result),
                QueryEvent::Finished => break,
                _ => {}
            }
        }
    });
    assert_eq!(got, 500);
    block_on(q.fetch_more(paused.unwrap(), 5000)).unwrap();
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            match ev {
                QueryEvent::Data { chunk, .. } => got += chunk.len(),
                QueryEvent::ResultPaused {
                    has_more: false, ..
                } => break,
                _ => {}
            }
        }
    });
    assert_eq!(got, 3000);

    // Explorer: databases → collections → indexes; inference; completion.
    let md = core.metadata();
    let root = block_on(md.children(NodeRef::root(ds.clone()))).unwrap();
    assert_eq!(root[0].name, "flows");
    let colls = block_on(md.children(root[0].node.clone())).unwrap();
    let people = colls.iter().find(|c| c.name == "people").unwrap();
    let idx = block_on(md.children(people.node.clone())).unwrap();
    assert!(idx.iter().any(|i| i.name == "name_1"), "{idx:?}");
    let schema = block_on(md.infer_schema(people.node.clone(), SampleOptions::default())).unwrap();
    let paths: Vec<&str> = schema.fields.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"address.city") && paths.contains(&"age"),
        "{paths:?}"
    );
    let script = block_on(md.definition(vec![people.node.clone()], DdlOptions::default())).unwrap();
    assert!(
        script.contains("db.people.createIndex({ name: 1 }"),
        "{script}"
    );
    let doc = DocSnapshot {
        text: "db.people.find({ ag".into(),
        language: QueryLanguage::Mql,
        data_source: Some(ds.clone()),
        version: 1,
    };
    let items = block_on(core.language().complete(doc.clone(), doc.text.len()));
    assert_eq!(
        items.first().map(|i| i.label.as_str()),
        Some("age"),
        "{items:?}"
    );
    let doc = DocSnapshot {
        text: "db.pe".into(),
        ..doc
    };
    let items = block_on(core.language().complete(doc.clone(), 5));
    assert_eq!(items[0].label, "people");
    assert_eq!(md.resolve(&ds, &["people".into()])[0].node, people.node);

    // Risks, then the read-only source refuses writes.
    let risks = core
        .language()
        .risks("db.people.deleteMany({})", &QueryLanguage::Mql, false);
    assert_eq!(risks[0].kind, RiskKind::DeleteAll);
    let ro = source("mongo-ro", props.clone(), |c| c.read_only = true);
    block_on(core.data_sources().save(ro.clone(), SecretValues::new())).unwrap();
    let s2 = block_on(q.open_session(&ro.id)).unwrap();
    let evs = events(
        &core,
        s2,
        "db.people.find({})\ndb.people.deleteOne({})",
        ExecOptions::default(),
    );
    assert_eq!(errors(&evs).len(), 1);
    assert!(errors(&evs)[0].contains("READ-ONLY"));
    assert_eq!(docs(&evs).len(), 2);

    // Manual transactions (replica set).
    let tx = source("mongo-tx", props.clone(), |c| c.tx_mode = TxMode::Manual);
    block_on(core.data_sources().save(tx.clone(), SecretValues::new())).unwrap();
    let s3 = block_on(q.open_session(&tx.id)).unwrap();
    let evs = events(
        &core,
        s3,
        "db.people.insertOne({name: 'tx'})",
        ExecOptions::default(),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    assert!(q.sessions().iter().any(|i| i.id == s3 && i.in_transaction));
    block_on(q.rollback(s3)).unwrap();
    let evs = events(
        &core,
        s,
        "db.people.countDocuments({name: 'tx'})",
        ExecOptions::default(),
    );
    let n = match docs(&evs)[0].get("count") {
        Some(bson::Bson::Int32(n)) => *n as i64,
        Some(bson::Bson::Int64(n)) => *n,
        other => panic!("count {other:?}"),
    };
    assert_eq!(n, 0, "the rolled back insert is gone");

    // Cancel a server-side operation from the UI handle.
    let h = q.execute(
        s,
        Script::new("db.many.find({$where: 'sleep(50) || true'})"),
        ExecOptions::default(),
    );
    std::thread::sleep(Duration::from_millis(300));
    let t = std::time::Instant::now();
    h.cancel.cancel();
    block_on(async {
        while let Ok(ev) = h.events.recv().await {
            if matches!(ev, QueryEvent::Finished) {
                break;
            }
        }
    });
    assert!(t.elapsed() < Duration::from_secs(1), "{:?}", t.elapsed());

    // Through an SSH tunnel to the container on the Docker network.
    if let Ok(ssh) = std::env::var("FERRODB_TCK_SSH") {
        let s = ssh.trim_start_matches("ssh://");
        let (auth, host) = s.split_once('@').unwrap();
        let (user, password) = auth.split_once(':').unwrap();
        let dir = tempfile::tempdir().unwrap();
        let tunneled = source(
            "mongo-ssh",
            vec![
                ("host".into(), "ferrodb-tck-mongo".into()),
                ("port".into(), "27017".into()),
                ("database".into(), "flows".into()),
                ("ssh.host".into(), host.into()),
                ("ssh.user".into(), user.into()),
                (
                    "ssh.known_hosts".into(),
                    dir.path().join("kh").to_string_lossy().into_owned(),
                ),
            ],
            |_| {},
        );
        block_on(core.data_sources().save(
            tunneled.clone(),
            SecretValues::new().with("ssh.password", password),
        ))
        .unwrap();
        let st = block_on(q.open_session(&tunneled.id)).unwrap();
        let evs = events(
            &core,
            st,
            "db.people.countDocuments({})",
            ExecOptions::default(),
        );
        assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
        assert_eq!(docs(&evs).len(), 1);
    }
}
