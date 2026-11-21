//! Phase 8 document sources through the whole backend on real servers:
//! DynamoDB Local (`FERRODB_TCK_DYNAMODB=http://host:56520`) and Couchbase
//! (`FERRODB_TCK_COUCHBASE=couchbase://user:pw@host:56503/tck?mgmt_port=56501&scope=app`):
//! console results with the cost warning, explorer tree, scripts, schema
//! inference, editing through the operation preview, execution plans.

#![allow(clippy::disallowed_methods)]

use ferrodb_api::*;
use futures::executor::block_on;

use crate::{CoreBackend, CoreOptions};

fn source(id: &str, driver: &str, props: Vec<(&str, String)>) -> DataSourceConfig {
    DataSourceConfig {
        id: id.into(),
        name: id.into(),
        driver: driver.into(),
        folder: vec![],
        environment: Environment::None,
        read_only: false,
        tx_mode: TxMode::Auto,
        properties: props.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        secret_ref: None,
    }
}

fn events(core: &CoreBackend, s: SessionId, text: &str) -> Vec<QueryEvent> {
    let h = core
        .queries()
        .execute(s, Script::new(text), ExecOptions::default());
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

fn notices(evs: &[QueryEvent]) -> Vec<ServerNotice> {
    evs.iter()
        .filter_map(|e| match e {
            QueryEvent::Notice(n) => Some(n.clone()),
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

fn last_result(evs: &[QueryEvent]) -> (ResultId, Option<NodeRef>) {
    evs.iter()
        .rev()
        .find_map(|e| match e {
            QueryEvent::ResultStarted {
                result,
                shape: ResultShape::Documents { node, .. },
                ..
            } => Some((*result, node.clone())),
            _ => None,
        })
        .expect("a document result")
}

#[test]
fn dynamodb_flows_on_dynamodb_local() {
    let Ok(endpoint) = std::env::var("FERRODB_TCK_DYNAMODB") else {
        return;
    };
    let core = CoreBackend::new(CoreOptions::in_memory()).unwrap();
    let cfg = source(
        "ddb",
        "dynamodb",
        vec![("region", "eu-central-1".into()), ("endpoint", endpoint)],
    );
    block_on(
        core.data_sources().save(
            cfg.clone(),
            SecretValues::new()
                .with("aws.access_key_id", "flows")
                .with("aws.secret_access_key", "flows"),
        ),
    )
    .unwrap();
    let ds = cfg.id.clone();
    let q = core.queries();
    let s = block_on(q.open_session(&ds)).unwrap();
    let table = format!("flows_{}", std::process::id());
    let create = format!(
        r#"CreateTable {{"TableName": "{table}", "BillingMode": "PAY_PER_REQUEST",
            "KeySchema": [{{"AttributeName": "pk", "KeyType": "HASH"}}, {{"AttributeName": "sk", "KeyType": "RANGE"}}],
            "AttributeDefinitions": [{{"AttributeName": "pk", "AttributeType": "S"}}, {{"AttributeName": "sk", "AttributeType": "N"}}, {{"AttributeName": "owner", "AttributeType": "S"}}],
            "GlobalSecondaryIndexes": [{{"IndexName": "byOwner", "KeySchema": [{{"AttributeName": "owner", "KeyType": "HASH"}}], "Projection": {{"ProjectionType": "ALL"}}}}]}}"#
    );
    let evs = events(&core, s, &create);
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    let evs = events(
        &core,
        s,
        &format!(
            "INSERT INTO \"{table}\" VALUE {{'pk': 'a', 'sk': 1, 'owner': 'o1', 'tags': <<'x', 'y'>>, 'n': 1.25}};\n\
             INSERT INTO \"{table}\" VALUE {{'pk': 'a', 'sk': 2, 'owner': 'o2'}};\n\
             SELECT * FROM \"{table}\" WHERE pk = 'a'"
        ),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    assert!(
        !notices(&evs)
            .iter()
            .any(|n| n.level == NoticeLevel::Warning),
        "a key condition is not a scan"
    );
    // An API call in the console; DynamoDB Local reports no consumed
    // capacity (AWS does: the driver turns it into a notice).
    let q_evs = events(
        &core,
        s,
        &format!(
            r#"Query {{"TableName": "{table}", "KeyConditionExpression": "pk = :p",
                "ExpressionAttributeValues": {{":p": {{"S": "a"}}}}, "Limit": 1}}"#
        ),
    );
    assert!(errors(&q_evs).is_empty(), "{:?}", errors(&q_evs));
    assert_eq!(docs(&q_evs).len(), 1);
    assert!(
        notices(&q_evs)
            .iter()
            .any(|n| n.message.contains("ExclusiveStartKey")),
        "{:?}",
        notices(&q_evs)
    );
    let found = docs(&evs);
    assert_eq!(found.len(), 2);
    assert_eq!(
        found[0].keys().take(2).collect::<Vec<_>>(),
        vec!["pk", "sk"]
    );
    // A scan gets the cost warning.
    let evs = events(
        &core,
        s,
        &format!("SELECT * FROM \"{table}\" WHERE owner = 'o1'"),
    );
    assert!(
        notices(&evs)
            .iter()
            .any(|n| n.level == NoticeLevel::Warning && n.message.contains("Scan")),
        "{:?}",
        notices(&evs)
    );
    let (rid, node) = last_result(&evs);
    assert_eq!(
        node.unwrap().path,
        vec![
            (ObjectKind::Database, "eu-central-1".into()),
            (ObjectKind::Table, table.clone())
        ]
    );

    // Explorer, script, inference.
    let md = core.metadata();
    let root = block_on(md.children(NodeRef::root(ds.clone()))).unwrap();
    assert_eq!(root[0].name, "eu-central-1");
    let tables = block_on(md.children(root[0].node.clone())).unwrap();
    let t = tables.iter().find(|t| t.name == table).unwrap();
    assert_eq!(t.detail.as_deref(), Some("PAY_PER_REQUEST"));
    let kids = block_on(md.children(t.node.clone())).unwrap();
    assert!(
        kids.iter()
            .any(|k| k.kind == ObjectKind::Index && k.name == "byOwner"),
        "{kids:?}"
    );
    assert!(kids.iter().any(|k| k.kind == ObjectKind::PrimaryKey));
    let script = block_on(md.definition(vec![t.node.clone()], DdlOptions::default())).unwrap();
    assert!(script.contains("CreateTable {"), "{script}");
    let schema = block_on(md.infer_schema(t.node.clone(), SampleOptions::default())).unwrap();
    let paths: Vec<&str> = schema.fields.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(&paths[..3], &["pk", "sk", "owner"], "{paths:?}");
    let tags = schema.fields.iter().find(|f| f.path == "tags").unwrap();
    assert_eq!(tags.types[0].0, "stringSet");
    let select = block_on(md.script(t.node.clone(), ScriptKind::Select)).unwrap();
    assert_eq!(
        select,
        format!("SELECT * FROM \"{table}\" WHERE \"pk\" = ?")
    );

    // Editing: the set stays a set.
    let changes = ChangeSet {
        changes: vec![Change::SetField {
            document: 0,
            path: "tags".into(),
            value: r#"{"$SS": ["x", "y", "z"]}"#.into(),
        }],
    };
    let preview = block_on(q.preview_changes(rid, changes.clone())).unwrap();
    assert_eq!(
        preview,
        vec![format!(
            "UPDATE \"{table}\" SET \"tags\" = <<'x', 'y', 'z'>> WHERE \"pk\" = 'a' AND \"sk\" = 1"
        )]
    );
    let report = block_on(q.apply_changes(rid, changes)).unwrap();
    assert_eq!(report.applied, 1);
    let evs = events(
        &core,
        s,
        &format!("SELECT tags FROM \"{table}\" WHERE pk = 'a' AND sk = 1"),
    );
    let mut tags = docs(&evs)[0]
        .get_document("tags")
        .unwrap()
        .get_array("$SS")
        .unwrap()
        .clone();
    tags.sort_by_key(|b| b.to_string());
    assert_eq!(tags.len(), 3);
    let evs = events(
        &core,
        s,
        &format!("DeleteTable {{\"TableName\": \"{table}\"}}"),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
}

#[test]
fn couchbase_flows_on_a_real_server() {
    let Ok(url) = std::env::var("FERRODB_TCK_COUCHBASE") else {
        return;
    };
    let r = ferrodb_drivers::ResolvedConfig::from_url("couchbase", &url).unwrap();
    let core = CoreBackend::new(CoreOptions::in_memory()).unwrap();
    let bucket = r.database().unwrap_or("tck").to_owned();
    let scope = r.prop("scope").unwrap_or("_default").to_owned();
    let cfg = source(
        "cb",
        "couchbase",
        vec![
            ("host", r.host().to_owned()),
            ("port", r.prop("port").unwrap_or("8093").to_owned()),
            (
                "mgmt_port",
                r.prop("mgmt_port").unwrap_or("8091").to_owned(),
            ),
            ("database", bucket.clone()),
            ("user", r.user().unwrap_or("Administrator").to_owned()),
            ("scan_consistency", "request_plus".into()),
        ],
    );
    block_on(core.data_sources().save(
        cfg.clone(),
        SecretValues::new().with("password", r.password().unwrap_or_default()),
    ))
    .unwrap();
    let ds = cfg.id.clone();
    let q = core.queries();
    let s = block_on(q.open_session(&ds)).unwrap();
    let coll = format!("flows_{}", std::process::id());
    let ks = format!("`{bucket}`.`{scope}`.`{coll}`");
    for stmt in [
        format!("CREATE COLLECTION {ks}"),
        format!("CREATE PRIMARY INDEX ON {ks}"),
        format!(
            "INSERT INTO {ks} (KEY, VALUE) VALUES (\"p1\", {{\"name\": \"Ala\", \"address\": {{\"city\": \"Kraków\"}}}}), (\"p2\", {{\"name\": \"Ola\", \"age\": 25}})"
        ),
    ] {
        let mut ok = false;
        for _ in 0..30 {
            if errors(&events(&core, s, &stmt)).is_empty() {
                ok = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        assert!(ok, "{stmt}");
    }
    // The explorer's template query: `SELECT * FROM scope.collection`.
    let evs = events(
        &core,
        s,
        &format!("SELECT * FROM {scope}.{coll} ORDER BY name LIMIT 100;"),
    );
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
    let found = docs(&evs);
    assert_eq!(found[0].get_str("_id").unwrap(), "p1");
    assert_eq!(found[0].get_str("name").unwrap(), "Ala");
    let (rid, node) = last_result(&evs);
    assert_eq!(
        node.unwrap().path,
        vec![
            (ObjectKind::Bucket, bucket.clone()),
            (ObjectKind::Scope, scope.clone()),
            (ObjectKind::Collection, coll.clone())
        ]
    );
    let changes = ChangeSet {
        changes: vec![Change::SetField {
            document: 0,
            path: "address.city".into(),
            value: "\"Gdańsk\"".into(),
        }],
    };
    let preview = block_on(q.preview_changes(rid, changes.clone())).unwrap();
    assert!(
        preview[0].contains("USE KEYS \"p1\" SET address.city = \"Gdańsk\""),
        "{preview:?}"
    );
    let report = block_on(q.apply_changes(rid, changes)).unwrap();
    assert_eq!(report.applied, 1);
    let evs = events(
        &core,
        s,
        &format!("SELECT RAW address.city FROM {ks} USE KEYS \"p1\""),
    );
    assert_eq!(docs(&evs)[0].get_str("value").unwrap(), "Gdańsk");

    // Explorer: bucket → scope → collection → index; scripts; INFER.
    let md = core.metadata();
    let root = block_on(md.children(NodeRef::root(ds.clone()))).unwrap();
    let b = root.iter().find(|b| b.name == bucket).unwrap();
    assert_eq!(b.kind, ObjectKind::Bucket);
    let scopes = block_on(md.children(b.node.clone())).unwrap();
    let sc = scopes.iter().find(|x| x.name == scope).unwrap();
    assert_eq!(sc.kind, ObjectKind::Scope);
    let colls = block_on(md.children(sc.node.clone())).unwrap();
    let c = colls.iter().find(|x| x.name == coll).unwrap();
    let idx = block_on(md.children(c.node.clone())).unwrap();
    assert!(idx.iter().any(|i| i.name == "#primary"), "{idx:?}");
    let script = block_on(md.definition(vec![c.node.clone()], DdlOptions::default())).unwrap();
    assert!(
        script.contains("CREATE COLLECTION") && script.contains("CREATE PRIMARY INDEX"),
        "{script}"
    );
    let schema = block_on(md.infer_schema(c.node.clone(), SampleOptions::default())).unwrap();
    let paths: Vec<&str> = schema.fields.iter().map(|f| f.path.as_str()).collect();
    assert!(
        paths.contains(&"address.city") && paths.contains(&"name"),
        "{paths:?}"
    );

    // Execution plan.
    let plan = block_on(q.explain(
        s,
        Script::new(format!("SELECT name FROM {ks} WHERE age > 1")),
        ExplainOptions::default(),
    ))
    .unwrap();
    assert!(plan.statement.starts_with("EXPLAIN"));
    assert!(
        plan.root
            .flatten()
            .iter()
            .any(|(_, n)| n.operation.starts_with("PrimaryScan")),
        "{:?}",
        plan.root
    );
    assert!(!plan.warnings().is_empty());
    let evs = events(&core, s, &format!("DROP COLLECTION {ks}"));
    assert!(errors(&evs).is_empty(), "{:?}", errors(&evs));
}
