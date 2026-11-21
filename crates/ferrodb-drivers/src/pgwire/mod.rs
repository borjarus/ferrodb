//! PostgreSQL wire-protocol family (`tokio-postgres`, rustls).
//!
//! Every product speaking the protocol is a [`Flavor`]: a static description
//! (id, name, defaults, quirks, product name in `server_version`) plus the
//! listing queries. PostgreSQL, Greenplum, CockroachDB and Amazon Redshift
//! are flavors with their own overrides (ADR-008): defaults, listing
//! queries, transaction retries (CockroachDB), IAM authentication and a
//! client-side read-only guard (Redshift).

mod conn;
mod decode;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_flavors;

use std::sync::Arc;

use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, DriverProperty, HierarchySpec, LanguageLevel,
    ObjectKind, Result, SqlDialectId,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, quote_literal,
    registry::{
        descriptor, network_properties, property, relational_capabilities, relational_hierarchy,
    },
};

/// The product behind a flavor: what the connection does differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    PostgreSql,
    /// Distributed tables (`DISTRIBUTED BY`), Motion nodes in plans.
    Greenplum,
    /// Serializable by default: statements may fail with SQLSTATE 40001
    /// and must be retried; `crdb_internal` catalog, `SHOW` statements.
    CockroachDb,
    /// PostgreSQL 8.0 dialect, IAM credentials, no read-only sessions.
    Redshift,
}

/// One product of the pgwire family.
pub struct Flavor {
    pub id: &'static str,
    pub name: &'static str,
    pub dialect: &'static str,
    /// Product name prefixed to the server version (`PostgreSQL 18.1`).
    pub product: &'static str,
    pub default_port: u16,
    pub default_database: Option<&'static str>,
    /// User when none is configured.
    pub default_user: &'static str,
    pub url_template: &'static str,
    pub auth: fn() -> Vec<AuthMethod>,
    pub quirks: Quirks,
    pub list_queries: fn(&[(ObjectKind, String)]) -> Vec<ListQuery>,
    pub kind: Product,
    /// Language level of the dialect (plan 2.10).
    pub level: LanguageLevel,
    /// Connection form fields beyond host / port / database / user / password.
    pub extra_properties: fn() -> Vec<DriverProperty>,
}

pub static POSTGRESQL: Flavor = Flavor {
    id: "postgresql",
    name: "PostgreSQL",
    dialect: "postgresql",
    product: "PostgreSQL",
    default_port: 5432,
    default_database: Some("postgres"),
    default_user: "postgres",
    url_template: "postgresql://{user}@{host}:{port}/{database}",
    auth: || vec![AuthMethod::Password],
    quirks: Quirks {
        transactional_ddl: true,
        read_only_sessions: true,
    },
    list_queries: catalog_list_queries,
    kind: Product::PostgreSql,
    level: LanguageLevel::A,
    extra_properties: Vec::new,
};

pub static GREENPLUM: Flavor = Flavor {
    id: "greenplum",
    name: "Greenplum",
    dialect: "greenplum",
    product: "Greenplum",
    default_port: 5432,
    default_database: Some("postgres"),
    default_user: "gpadmin",
    url_template: "postgresql://{user}@{host}:{port}/{database}",
    auth: || vec![AuthMethod::Password],
    quirks: Quirks {
        transactional_ddl: true,
        read_only_sessions: true,
    },
    list_queries: catalog_list_queries,
    kind: Product::Greenplum,
    level: LanguageLevel::B,
    extra_properties: Vec::new,
};

pub static COCKROACHDB: Flavor = Flavor {
    id: "cockroachdb",
    name: "CockroachDB",
    dialect: "cockroachdb",
    product: "CockroachDB",
    default_port: 26257,
    default_database: Some("defaultdb"),
    default_user: "root",
    url_template: "postgresql://{user}@{host}:{port}/{database}",
    auth: || vec![AuthMethod::Password],
    // Schema changes run asynchronously and a transaction mixing them with
    // other statements may not roll back cleanly: scripts run statement by
    // statement.
    quirks: Quirks {
        transactional_ddl: false,
        read_only_sessions: true,
    },
    list_queries: cockroach_list_queries,
    kind: Product::CockroachDb,
    level: LanguageLevel::B,
    extra_properties: || {
        vec![property(
            "option.retries",
            "Retries of a statement failing with SQLSTATE 40001 (auto-commit)",
            Some("3"),
            false,
        )]
    },
};

pub static REDSHIFT: Flavor = Flavor {
    id: "redshift",
    name: "Amazon Redshift",
    dialect: "redshift",
    product: "Redshift",
    default_port: 5439,
    default_database: Some("dev"),
    default_user: "awsuser",
    url_template: "jdbc:redshift://{host}:{port}/{database}",
    auth: || vec![AuthMethod::Password, AuthMethod::AwsIam],
    quirks: Quirks {
        transactional_ddl: true,
        read_only_sessions: false,
    },
    list_queries: redshift_list_queries,
    kind: Product::Redshift,
    level: LanguageLevel::B,
    extra_properties: crate::aws::properties,
};

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    [&POSTGRESQL, &GREENPLUM, &COCKROACHDB, &REDSHIFT]
        .into_iter()
        .map(|f| Arc::new(PgDriver(f)) as Arc<dyn Driver>)
        .collect()
}

pub struct PgDriver(pub &'static Flavor);

struct PgVariant(&'static Flavor);

impl FamilyVariant for PgVariant {
    fn driver_id(&self) -> DriverId {
        self.0.id.into()
    }
    fn dialect(&self) -> SqlDialectId {
        self.0.dialect.into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        (self.0.auth)()
    }
    fn quirks(&self) -> Quirks {
        self.0.quirks
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Schema)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        (self.0.list_queries)(path)
    }
}

/// Explorer listings from `pg_catalog` (PostgreSQL and close derivatives).
pub fn catalog_list_queries(path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
    match path {
        [] => vec![ListQuery {
            sql: "select nspname as name from pg_catalog.pg_namespace \
                  where nspname not like 'pg\\_toast%' and nspname not like 'pg\\_temp\\_%' \
                  order by (nspname = current_schema()) desc, nspname"
                .into(),
            kind: ObjectKind::Schema,
            has_children: true,
        }],
        [(ObjectKind::Schema, s)] => [
            ("'r', 'p', 'f'", ObjectKind::Table),
            ("'v', 'm'", ObjectKind::View),
        ]
        .into_iter()
        .map(|(kinds, kind)| ListQuery {
            sql: format!(
                "select c.relname as name from pg_catalog.pg_class c \
                 join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = {} and c.relkind in ({kinds}) order by c.relname",
                quote_literal(s)
            ),
            kind,
            has_children: true,
        })
        .collect(),
        [
            (ObjectKind::Schema, s),
            (ObjectKind::Table | ObjectKind::View, t),
        ] => vec![ListQuery {
            sql: format!(
                "select a.attname as name, pg_catalog.format_type(a.atttypid, a.atttypmod) as type \
                 from pg_catalog.pg_attribute a \
                 join pg_catalog.pg_class c on c.oid = a.attrelid \
                 join pg_catalog.pg_namespace n on n.oid = c.relnamespace \
                 where n.nspname = {} and c.relname = {} \
                 and a.attnum > 0 and not a.attisdropped order by a.attnum",
                quote_literal(s),
                quote_literal(t)
            ),
            kind: ObjectKind::Column,
            has_children: false,
        }],
        _ => Vec::new(),
    }
}

/// CockroachDB: its internal schemas (`crdb_internal`, `pg_extension`) after
/// the user's, and only visible columns (the implicit `rowid` is hidden).
pub fn cockroach_list_queries(path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
    match path {
        [] => vec![ListQuery {
            sql: "select nspname as name from pg_catalog.pg_namespace \
                  order by (nspname = current_schema()) desc, \
                  nspname in ('crdb_internal', 'pg_extension', 'pg_catalog', 'information_schema'), \
                  nspname"
                .into(),
            kind: ObjectKind::Schema,
            has_children: true,
        }],
        [
            (ObjectKind::Schema, s),
            (ObjectKind::Table | ObjectKind::View, t),
        ] => vec![ListQuery {
            sql: format!(
                "select column_name as name, crdb_sql_type as type from information_schema.columns \
                 where table_schema = {} and table_name = {} and is_hidden = 'NO' \
                 order by ordinal_position",
                quote_literal(s),
                quote_literal(t)
            ),
            kind: ObjectKind::Column,
            has_children: false,
        }],
        _ => catalog_list_queries(path),
    }
}

/// Redshift: local and external schemas (Spectrum, federated, datashares)
/// from `svv_all_schemas`; tables and columns from `svv_all_tables` /
/// `svv_all_columns`, which cover both.
pub fn redshift_list_queries(path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
    match path {
        [] => vec![ListQuery {
            sql: "select schema_name as name, schema_type as detail from svv_all_schemas \
                  where database_name = current_database() \
                  and schema_name not like 'pg\\_temp\\_%' and schema_name <> 'pg_toast' \
                  order by (schema_name = current_schema()) desc, schema_name"
                .into(),
            kind: ObjectKind::Schema,
            has_children: true,
        }],
        [(ObjectKind::Schema, s)] => [("TABLE", ObjectKind::Table), ("VIEW", ObjectKind::View)]
            .into_iter()
            .map(|(ty, kind)| ListQuery {
                sql: format!(
                    "select table_name as name from svv_all_tables \
                     where database_name = current_database() and schema_name = {} \
                     and table_type in ('{ty}', 'EXTERNAL {ty}') order by table_name",
                    quote_literal(s)
                ),
                kind,
                has_children: true,
            })
            .collect(),
        [
            (ObjectKind::Schema, s),
            (ObjectKind::Table | ObjectKind::View, t),
        ] => vec![ListQuery {
            sql: format!(
                "select column_name as name, data_type as type from svv_all_columns \
                 where database_name = current_database() and schema_name = {} \
                 and table_name = {} order by ordinal_position",
                quote_literal(s),
                quote_literal(t)
            ),
            kind: ObjectKind::Column,
            has_children: false,
        }],
        _ => Vec::new(),
    }
}

impl Driver for PgDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let f = self.0;
        let auth = (f.auth)();
        let mut caps = relational_capabilities(f.dialect, auth.clone());
        caps.language_level = f.level;
        let mut props = network_properties(f.default_port, f.default_database);
        if auth.contains(&AuthMethod::AwsIam) {
            // With IAM the user is the database user credentials are issued for.
            for p in props.iter_mut().filter(|p| p.key == "user") {
                p.auth.push(AuthMethod::AwsIam);
            }
        }
        props.extend((f.extra_properties)());
        let mut d = descriptor(
            f.id,
            "pgwire",
            f.name,
            caps,
            props,
            Some(f.default_port),
            f.url_template,
        );
        if matches!(f.kind, Product::Greenplum | Product::Redshift) {
            d.category = ferrodb_api::DriverCategory::AnalyticalCloud;
        }
        d
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        Arc::new(PgVariant(self.0))
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(conn::connect(self.0, cfg))
    }
}
