//! Amazon DynamoDB (`aws-sdk-dynamodb`, `aws-config`): region, optional
//! endpoint (DynamoDB Local), credentials from the AWS chain (profile,
//! environment, SSO, instance roles) or an explicit access key.
//!
//! - Console statements are PartiQL, run with `ExecuteStatement` and paged
//!   with `NextToken`; each page is sent to the sink before the next one is
//!   requested (backpressure). `Operation {json}` lines run an API call
//!   instead (`Scan {"TableName": "t", "Limit": 10}`), with the request in
//!   the AWS API / CLI shape.
//! - [`Request::Native`](crate::Request::Native) methods: `ListTables`,
//!   `DescribeTable`, `Query`, `Scan`, `GetItem`, `PutItem`, `UpdateItem`,
//!   `DeleteItem`, `TransactWriteItems`, `ExecuteStatement`,
//!   `ExecuteTransaction`, `CreateTable`, `DeleteTable` (see [`ops`]).
//! - Items are documents (BSON); see [`value`] for the type mapping.
//! - Every call asks for the consumed capacity, reported as a notice; a
//!   `SELECT` without a partition key condition and every `Scan` get a
//!   cost warning first (a scan reads and bills the whole table).
//! - Transactions are single calls (`ExecuteTransaction`,
//!   `TransactWriteItems`): [`TxSupport::SingleStatement`].
//!
//! HTTPS: the SDK's default client is rustls + aws-lc (cmake and NASM on
//! Windows); the smithy hyper client is built with rustls + ring instead.

mod conn;
pub mod ops;
pub mod partiql;
pub mod value;

use std::sync::Arc;

use ferrodb_api::{
    AccessLayer, AuthMethod, Capabilities, DataModel, DriverCategory, DriverDescriptor, DriverId,
    DriverProperty, ExplainSupport, HierarchyLevel, HierarchySpec, LanguageLevel, ObjectKind,
    QueryLanguage, SchemaKind, SqlDialectId, TxSupport,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, Result,
    registry::{property, secret_property},
};

pub use conn::DynamoConnection;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(DynamoDriver)]
}

pub fn capabilities() -> Capabilities {
    Capabilities {
        data_model: DataModel::Document,
        language: QueryLanguage::PartiQl,
        access: AccessLayer::Native,
        language_level: LanguageLevel::B,
        transactions: TxSupport::SingleStatement,
        schema: SchemaKind::Inferred,
        joins: false,
        explain: ExplainSupport::None,
        // Requests are short HTTP calls: cancelling drops the call; there
        // is no server-side operation to stop.
        server_cancel: false,
        editable_results: true,
        cost_estimate: false,
        auth_methods: vec![AuthMethod::AwsIam],
    }
}

pub struct DynamoDriver;

struct DynamoVariant;

impl FamilyVariant for DynamoVariant {
    fn driver_id(&self) -> DriverId {
        "dynamodb".into()
    }
    fn dialect(&self) -> SqlDialectId {
        "partiql".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        capabilities().auth_methods
    }
    fn quirks(&self) -> Quirks {
        Quirks::default()
    }
    fn hierarchy(&self) -> HierarchySpec {
        let level = |kind, children: &[ObjectKind]| HierarchyLevel {
            kind,
            children: children.to_vec(),
        };
        HierarchySpec {
            levels: vec![
                level(ObjectKind::Database, &[ObjectKind::Table]),
                level(
                    ObjectKind::Table,
                    &[
                        ObjectKind::Column,
                        ObjectKind::PrimaryKey,
                        ObjectKind::Index,
                    ],
                ),
            ],
            key_separator: None,
        }
    }
    fn list_queries(&self, _path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        // Not SQL: the explorer uses the DynamoDB introspector (ferrodb-meta).
        Vec::new()
    }
}

fn iam(p: DriverProperty) -> DriverProperty {
    DriverProperty {
        auth: vec![AuthMethod::AwsIam],
        ..p
    }
}

impl Driver for DynamoDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let iam_only = [AuthMethod::AwsIam];
        DriverDescriptor {
            id: "dynamodb".into(),
            family: "dynamodb".into(),
            name: "Amazon DynamoDB".into(),
            category: DriverCategory::NoSql,
            capabilities: capabilities(),
            requirements: Vec::new(),
            properties: vec![
                property("region", "Region", Some("eu-central-1"), true),
                property(
                    "endpoint",
                    "Endpoint (optional, e.g. http://localhost:8000 for DynamoDB Local)",
                    None,
                    false,
                ),
                iam(property(
                    "aws.profile",
                    "AWS profile",
                    Some("default"),
                    false,
                )),
                iam(property("aws.region", "AWS region", None, false)),
                secret_property("aws.access_key_id", "Access key ID", &iam_only),
                secret_property("aws.secret_access_key", "Secret access key", &iam_only),
                secret_property("aws.session_token", "Session token", &iam_only),
            ],
            default_port: None,
            url_template: None,
        }
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(DynamoVariant)
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = DynamoConnection::connect(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}
