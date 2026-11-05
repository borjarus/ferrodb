use serde::{Deserialize, Serialize};

use crate::SqlDialectId;

/// The shape of data a source returns. Views are selected by model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DataModel {
    /// Rows and columns (SQL, CQL). Results are Arrow record batches.
    Tabular,
    /// Documents (MongoDB, Couchbase, DynamoDB). Results are raw BSON.
    Document,
    /// Keys with typed values (Redis).
    KeyValue,
}

/// Query language of a data source. Language support is a replaceable module.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum QueryLanguage {
    Sql(SqlDialectId),
    /// MongoDB shell syntax subset.
    Mql,
    /// Cassandra Query Language.
    Cql,
    /// Couchbase SQL++.
    SqlPlusPlus,
    /// DynamoDB PartiQL.
    PartiQl,
    RedisCommands,
}

impl QueryLanguage {
    pub fn sql(dialect: &str) -> Self {
        Self::Sql(SqlDialectId::new(dialect))
    }

    /// Short human-readable name (not localized: these are proper names).
    pub fn display_name(&self) -> String {
        match self {
            Self::Sql(d) => format!("SQL ({d})"),
            Self::Mql => "MQL".into(),
            Self::Cql => "CQL".into(),
            Self::SqlPlusPlus => "SQL++".into(),
            Self::PartiQl => "PartiQL".into(),
            Self::RedisCommands => "Redis".into(),
        }
    }

    /// Default console file extension (section 5.10 of the plan).
    pub fn file_extension(&self) -> &'static str {
        match self {
            Self::Sql(_) => "sql",
            Self::Mql => "mongo.js",
            Self::Cql => "cql",
            Self::SqlPlusPlus => "sqlpp",
            Self::PartiQl => "partiql",
            Self::RedisCommands => "redis",
        }
    }
}

/// How the driver reaches the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AccessLayer {
    /// Native Rust driver.
    Native,
    /// ADBC driver (dynamically loaded, Arrow results).
    Adbc,
    /// JDBC bridge (separate Java process).
    Jdbc,
}

/// Language intelligence level (section 2.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum LanguageLevel {
    A,
    B,
    C,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TxSupport {
    None,
    SingleStatement,
    MultiStatement,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SchemaKind {
    /// Declared schema (relational, Cassandra).
    Strict,
    /// Schema inferred by sampling (documents).
    Inferred,
    /// Only keys are known (Redis).
    KeysOnly,
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExplainSupport {
    None,
    Text,
    Structured,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AuthMethod {
    None,
    Password,
    /// AWS IAM (profiles, SSO, roles).
    AwsIam,
    /// Microsoft Entra ID.
    EntraId,
    /// Google service account / ADC.
    GoogleServiceAccount,
    /// Snowflake key pair.
    KeyPair,
    Sso,
    Kerberos,
    X509,
}

/// What a data source can do. The GUI enables features based on this, never
/// on the database name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub data_model: DataModel,
    pub language: QueryLanguage,
    pub access: AccessLayer,
    pub language_level: LanguageLevel,
    pub transactions: TxSupport,
    pub schema: SchemaKind,
    /// `false` e.g. for Cassandra and DynamoDB.
    pub joins: bool,
    pub explain: ExplainSupport,
    pub server_cancel: bool,
    pub editable_results: bool,
    /// Cost estimate before execution (BigQuery dry run).
    pub cost_estimate: bool,
    pub auth_methods: Vec<AuthMethod>,
}

impl Capabilities {
    pub fn supports_transactions(&self) -> bool {
        self.transactions == TxSupport::MultiStatement
    }
}
