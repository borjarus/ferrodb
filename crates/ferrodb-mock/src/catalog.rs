//! The 26 databases of version 1.0 (plan, section 2.8) as driver descriptors.

use ferrodb_api::*;

struct Spec {
    id: &'static str,
    name: &'static str,
    family: &'static str,
    category: DriverCategory,
    model: DataModel,
    language: QueryLanguage,
    access: AccessLayer,
    level: LanguageLevel,
    tx: TxSupport,
    joins: bool,
    explain: ExplainSupport,
    cost: bool,
    auth: &'static [AuthMethod],
    port: Option<u16>,
}

fn sql(d: &str) -> QueryLanguage {
    QueryLanguage::sql(d)
}

use AccessLayer::{Adbc, Jdbc, Native};
use AuthMethod as Au;
use DataModel::{Document, KeyValue, Tabular};
use DriverCategory::{AnalyticalCloud as Cloud, NoSql, Relational as Rel};
use ExplainSupport::{None as NoExplain, Structured, Text};
use LanguageLevel::{A, B};
use TxSupport::{MultiStatement as Multi, None as NoTx, SingleStatement as Single};

const PW: &[AuthMethod] = &[Au::Password];

fn specs() -> Vec<Spec> {
    #[rustfmt::skip]
    let v = vec![
        Spec { id: "postgresql", name: "PostgreSQL", family: "pgwire", category: Rel, model: Tabular, language: sql("postgresql"), access: Native, level: A, tx: Multi, joins: true, explain: Structured, cost: false, auth: PW, port: Some(5432) },
        Spec { id: "greenplum", name: "Greenplum", family: "pgwire", category: Cloud, model: Tabular, language: sql("greenplum"), access: Native, level: B, tx: Multi, joins: true, explain: Structured, cost: false, auth: PW, port: Some(5432) },
        Spec { id: "cockroachdb", name: "CockroachDB", family: "pgwire", category: Rel, model: Tabular, language: sql("cockroachdb"), access: Native, level: B, tx: Multi, joins: true, explain: Structured, cost: false, auth: PW, port: Some(26257) },
        Spec { id: "redshift", name: "Amazon Redshift", family: "pgwire", category: Cloud, model: Tabular, language: sql("redshift"), access: Native, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: &[Au::Password, Au::AwsIam], port: Some(5439) },
        Spec { id: "mysql", name: "MySQL", family: "mysql", category: Rel, model: Tabular, language: sql("mysql"), access: Native, level: A, tx: Multi, joins: true, explain: Structured, cost: false, auth: PW, port: Some(3306) },
        Spec { id: "mariadb", name: "MariaDB", family: "mysql", category: Rel, model: Tabular, language: sql("mariadb"), access: Native, level: A, tx: Multi, joins: true, explain: Structured, cost: false, auth: PW, port: Some(3306) },
        Spec { id: "sqlite", name: "SQLite", family: "sqlite", category: Rel, model: Tabular, language: sql("sqlite"), access: Native, level: A, tx: Multi, joins: true, explain: Text, cost: false, auth: &[Au::None], port: None },
        Spec { id: "sqlserver", name: "Microsoft SQL Server", family: "tds", category: Rel, model: Tabular, language: sql("tsql"), access: Native, level: A, tx: Multi, joins: true, explain: Structured, cost: false, auth: &[Au::Password, Au::Kerberos], port: Some(1433) },
        Spec { id: "azuresql", name: "Azure SQL Server", family: "tds", category: Cloud, model: Tabular, language: sql("tsql"), access: Native, level: A, tx: Multi, joins: true, explain: Structured, cost: false, auth: &[Au::Password, Au::EntraId], port: Some(1433) },
        Spec { id: "oracle", name: "Oracle", family: "jdbc", category: Rel, model: Tabular, language: sql("oracle"), access: Jdbc, level: A, tx: Multi, joins: true, explain: Structured, cost: false, auth: &[Au::Password, Au::Kerberos], port: Some(1521) },
        Spec { id: "db2", name: "IBM Db2", family: "jdbc", category: Rel, model: Tabular, language: sql("db2"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: PW, port: Some(50000) },
        Spec { id: "sybase", name: "Sybase ASE", family: "jdbc", category: Rel, model: Tabular, language: sql("sybase"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: PW, port: Some(5000) },
        Spec { id: "h2", name: "H2", family: "jdbc", category: Rel, model: Tabular, language: sql("h2"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: PW, port: Some(9092) },
        Spec { id: "derby", name: "Apache Derby", family: "jdbc", category: Rel, model: Tabular, language: sql("derby"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: PW, port: Some(1527) },
        Spec { id: "hsqldb", name: "HyperSQL", family: "jdbc", category: Rel, model: Tabular, language: sql("hsqldb"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: PW, port: Some(9001) },
        Spec { id: "exasol", name: "Exasol", family: "jdbc", category: Cloud, model: Tabular, language: sql("exasol"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: PW, port: Some(8563) },
        Spec { id: "vertica", name: "Vertica", family: "jdbc", category: Cloud, model: Tabular, language: sql("vertica"), access: Jdbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: &[Au::Password, Au::Kerberos], port: Some(5433) },
        Spec { id: "hive", name: "Apache Hive", family: "jdbc", category: Cloud, model: Tabular, language: sql("hiveql"), access: Jdbc, level: B, tx: NoTx, joins: true, explain: Text, cost: false, auth: &[Au::Password, Au::Kerberos], port: Some(10000) },
        Spec { id: "snowflake", name: "Snowflake", family: "adbc", category: Cloud, model: Tabular, language: sql("snowflake"), access: Adbc, level: B, tx: Multi, joins: true, explain: Text, cost: false, auth: &[Au::Password, Au::KeyPair, Au::Sso], port: None },
        Spec { id: "bigquery", name: "Google BigQuery", family: "adbc", category: Cloud, model: Tabular, language: sql("googlesql"), access: Adbc, level: B, tx: NoTx, joins: true, explain: Text, cost: true, auth: &[Au::GoogleServiceAccount], port: None },
        Spec { id: "clickhouse", name: "ClickHouse", family: "clickhouse", category: Cloud, model: Tabular, language: sql("clickhouse"), access: Native, level: B, tx: NoTx, joins: true, explain: Text, cost: false, auth: PW, port: Some(8123) },
        Spec { id: "cassandra", name: "Apache Cassandra", family: "cql", category: NoSql, model: Tabular, language: QueryLanguage::Cql, access: Native, level: B, tx: Single, joins: false, explain: NoExplain, cost: false, auth: PW, port: Some(9042) },
        Spec { id: "mongodb", name: "MongoDB", family: "mongodb", category: NoSql, model: Document, language: QueryLanguage::Mql, access: Native, level: A, tx: Multi, joins: false, explain: Structured, cost: false, auth: &[Au::Password, Au::X509], port: Some(27017) },
        Spec { id: "couchbase", name: "Couchbase", family: "couchbase", category: NoSql, model: Document, language: QueryLanguage::SqlPlusPlus, access: Native, level: B, tx: Multi, joins: true, explain: Structured, cost: false, auth: &[Au::Password, Au::X509], port: Some(11210) },
        Spec { id: "dynamodb", name: "Amazon DynamoDB", family: "dynamodb", category: NoSql, model: Document, language: QueryLanguage::PartiQl, access: Native, level: B, tx: Single, joins: false, explain: NoExplain, cost: false, auth: &[Au::AwsIam], port: None },
        Spec { id: "redis", name: "Redis", family: "redis", category: NoSql, model: KeyValue, language: QueryLanguage::RedisCommands, access: Native, level: B, tx: Single, joins: false, explain: NoExplain, cost: false, auth: &[Au::None, Au::Password], port: Some(6379) },
    ];
    v
}

fn schema_kind(model: DataModel) -> SchemaKind {
    match model {
        Tabular => SchemaKind::Strict,
        Document => SchemaKind::Inferred,
        KeyValue => SchemaKind::KeysOnly,
    }
}

fn properties(model: DataModel, port: Option<u16>, id: &str) -> Vec<DriverProperty> {
    let p = |key: &str, label: &str, default: Option<String>, required: bool, secret: bool| {
        DriverProperty {
            key: key.into(),
            label: label.into(),
            default,
            required,
            secret,
        }
    };
    if id == "sqlite" {
        return vec![p("file", "File", None, true, false)];
    }
    if model == Document && id == "mongodb" {
        return vec![
            p(
                "uri",
                "URI",
                Some("mongodb://localhost:27017".into()),
                true,
                false,
            ),
            p("user", "User", None, false, false),
            p("password", "Password", None, false, true),
        ];
    }
    vec![
        p("host", "Host", Some("localhost".into()), true, false),
        p("port", "Port", port.map(|p| p.to_string()), false, false),
        p("database", "Database", None, false, false),
        p("user", "User", None, false, false),
        p("password", "Password", None, false, true),
    ]
}

fn requirements(access: AccessLayer, id: &str) -> Vec<Requirement> {
    match access {
        Native => vec![],
        Jdbc => vec![
            Requirement::JavaRuntime { min_version: 21 },
            Requirement::JdbcArtifact {
                coordinates: format!("mock.jdbc:{id}"),
                version: "1.0.0".into(),
                sha256: "0".repeat(64),
                license: "vendor".into(),
            },
        ],
        Adbc => vec![Requirement::AdbcLibrary {
            name: format!("adbc_driver_{id}"),
            version: "1.0.0".into(),
            sha256: "0".repeat(64),
            license: "Apache-2.0".into(),
        }],
    }
}

/// All driver descriptors of the 1.0 catalog.
pub fn descriptors() -> Vec<DriverDescriptor> {
    specs()
        .into_iter()
        .map(|s| DriverDescriptor {
            id: DriverId::new(s.id),
            family: FamilyId::new(s.family),
            name: s.name.into(),
            category: s.category,
            capabilities: Capabilities {
                data_model: s.model,
                language: s.language,
                access: s.access,
                language_level: s.level,
                transactions: s.tx,
                schema: schema_kind(s.model),
                joins: s.joins,
                explain: s.explain,
                server_cancel: true,
                editable_results: true,
                cost_estimate: s.cost,
                auth_methods: s.auth.to_vec(),
            },
            requirements: requirements(s.access, s.id),
            properties: properties(s.model, s.port, s.id),
            default_port: s.port,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_26_databases_with_expected_layer_split() {
        let d = descriptors();
        assert_eq!(d.len(), 26);
        let count = |a| d.iter().filter(|x| x.access() == a).count();
        assert_eq!(count(Native), 15);
        assert_eq!(count(Adbc), 2);
        assert_eq!(count(Jdbc), 9);
        let mut ids: Vec<_> = d.iter().map(|x| x.id.clone()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 26, "driver ids must be unique");
    }
}
