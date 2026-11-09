//! MySQL family (`mysql_async`): MySQL and MariaDB share the protocol code
//! ([`conn`]); what differs between the products goes through [`Product`]
//! and one [`FamilyVariant`] per product (ADR-008).
//!
//! Differences handled here:
//! - product name and dialect; the server version string (`8.4.3` vs
//!   `11.4.4-MariaDB-ubu2404`) decides the product actually connected to;
//! - MariaDB lists system-versioned tables (`table_type = 'SYSTEM
//!   VERSIONED'`) next to base tables;
//! - MariaDB `JSON` is an alias of `LONGTEXT` (reported as text), MySQL has a
//!   native `JSON` column type;
//! - the recursive CTE limit is `cte_max_recursion_depth` in MySQL and
//!   `max_recursive_iterations` in MariaDB ([`Product::recursion_limit_var`]).

mod conn;
mod types;

use std::sync::Arc;

use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, HierarchySpec, ObjectKind, SqlDialectId,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, Result,
    quote_literal,
    registry::{descriptor, network_properties, relational_capabilities, relational_hierarchy},
};

pub const DEFAULT_PORT: u16 = 3306;

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![
        Arc::new(MySqlDriver {
            product: Product::MySql,
        }),
        Arc::new(MySqlDriver {
            product: Product::MariaDb,
        }),
    ]
}

/// Database product of the MySQL family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Product {
    MySql,
    MariaDb,
}

impl Product {
    pub fn id(self) -> &'static str {
        match self {
            Self::MySql => "mysql",
            Self::MariaDb => "mariadb",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::MySql => "MySQL",
            Self::MariaDb => "MariaDB",
        }
    }

    /// Detects the product from `SELECT VERSION()`.
    pub fn from_version(version: &str) -> Self {
        if version.to_ascii_lowercase().contains("mariadb") {
            Self::MariaDb
        } else {
            Self::MySql
        }
    }

    /// `MySQL 8.4.3` / `MariaDB 11.4.4` from `SELECT VERSION()`.
    pub fn version_string(version: &str) -> String {
        let product = Self::from_version(version);
        // MariaDB used to prefix `5.5.5-` for old clients.
        let v = version.strip_prefix("5.5.5-").unwrap_or(version);
        let number = v.split(['-', ' ']).next().unwrap_or(v);
        format!("{} {number}", product.name())
    }

    /// Statement switching the session between read-only and read-write.
    pub fn read_only_sql(self, read_only: bool) -> &'static str {
        // Same syntax in both (MariaDB ≥ 10.0, MySQL ≥ 5.6.5).
        if read_only {
            "SET SESSION TRANSACTION READ ONLY"
        } else {
            "SET SESSION TRANSACTION READ WRITE"
        }
    }

    /// `information_schema.tables.table_type` values listed as tables.
    pub fn table_types(self) -> &'static str {
        match self {
            Self::MySql => "'BASE TABLE'",
            Self::MariaDb => "'BASE TABLE', 'SYSTEM VERSIONED'",
        }
    }

    /// Session variable bounding recursive CTEs.
    pub fn recursion_limit_var(self) -> &'static str {
        match self {
            Self::MySql => "cte_max_recursion_depth",
            Self::MariaDb => "max_recursive_iterations",
        }
    }
}

/// MySQL string literal: `'` doubled and `\` escaped (backslash escapes are
/// on unless `NO_BACKSLASH_ESCAPES`).
pub fn mysql_literal(s: &str) -> String {
    quote_literal(&s.replace('\\', "\\\\"))
}

fn list_queries(product: Product, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
    match path {
        [] => vec![ListQuery {
            sql: "select schema_name as name from information_schema.schemata \
                  order by schema_name = database() desc, schema_name"
                .into(),
            kind: ObjectKind::Database,
            has_children: true,
        }],
        [(ObjectKind::Database, d)] => {
            let db = mysql_literal(d);
            [
                (product.table_types(), ObjectKind::Table),
                ("'VIEW'", ObjectKind::View),
            ]
            .into_iter()
            .map(|(types, kind)| ListQuery {
                sql: format!(
                    "select table_name as name from information_schema.tables \
                     where table_schema = {db} and table_type in ({types}) order by table_name"
                ),
                kind,
                has_children: true,
            })
            .collect()
        }
        [
            (ObjectKind::Database, d),
            (ObjectKind::Table | ObjectKind::View, t),
        ] => {
            vec![ListQuery {
                sql: format!(
                    "select column_name as name, column_type as detail \
                     from information_schema.columns \
                     where table_schema = {} and table_name = {} order by ordinal_position",
                    mysql_literal(d),
                    mysql_literal(t)
                ),
                kind: ObjectKind::Column,
                has_children: false,
            }]
        }
        _ => Vec::new(),
    }
}

fn quirks() -> Quirks {
    Quirks {
        transactional_ddl: false,
        read_only_sessions: true,
    }
}

pub struct MySqlVariant;
pub struct MariaDbVariant;

impl FamilyVariant for MySqlVariant {
    fn driver_id(&self) -> DriverId {
        Product::MySql.id().into()
    }
    fn dialect(&self) -> SqlDialectId {
        "mysql".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::Password]
    }
    fn quirks(&self) -> Quirks {
        quirks()
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Database)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        list_queries(Product::MySql, path)
    }
}

impl FamilyVariant for MariaDbVariant {
    fn driver_id(&self) -> DriverId {
        Product::MariaDb.id().into()
    }
    fn dialect(&self) -> SqlDialectId {
        "mariadb".into()
    }
    fn auth(&self) -> Vec<AuthMethod> {
        vec![AuthMethod::Password]
    }
    fn quirks(&self) -> Quirks {
        quirks()
    }
    fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(ObjectKind::Database)
    }
    fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        list_queries(Product::MariaDb, path)
    }
}

pub struct MySqlDriver {
    product: Product,
}

impl Driver for MySqlDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let id = self.product.id();
        descriptor(
            id,
            "mysql",
            self.product.name(),
            relational_capabilities(id, vec![AuthMethod::Password]),
            network_properties(DEFAULT_PORT, None),
            Some(DEFAULT_PORT),
            &format!("{id}://{{user}}@{{host}}:{{port}}/{{database}}"),
        )
    }

    fn variant(&self) -> Arc<dyn FamilyVariant> {
        match self.product {
            Product::MySql => Arc::new(MySqlVariant),
            Product::MariaDb => Arc::new(MariaDbVariant),
        }
    }

    fn connect<'a>(
        &'a self,
        cfg: &'a ResolvedConfig,
    ) -> BoxFuture<'a, Result<Box<dyn Connection>>> {
        Box::pin(async move {
            let c = conn::MySqlConnection::connect(cfg).await?;
            Ok(Box::new(c) as Box<dyn Connection>)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_and_variants() {
        let d = drivers();
        assert_eq!(d.len(), 2);
        let my = d[0].descriptor();
        assert_eq!((my.id.as_str(), my.name.as_str()), ("mysql", "MySQL"));
        assert_eq!(my.family.as_str(), "mysql");
        assert_eq!(my.default_port, Some(3306));
        assert_eq!(
            my.url_template.as_deref(),
            Some("mysql://{user}@{host}:{port}/{database}")
        );
        let ma = d[1].descriptor();
        assert_eq!((ma.id.as_str(), ma.name.as_str()), ("mariadb", "MariaDB"));
        assert_eq!(ma.family.as_str(), "mysql");
        assert_eq!(d[1].variant().dialect().as_str(), "mariadb");
        assert_eq!(d[0].variant().dialect().as_str(), "mysql");
        assert!(!d[0].variant().quirks().transactional_ddl);
    }

    #[test]
    fn versions() {
        assert_eq!(Product::version_string("8.4.3"), "MySQL 8.4.3");
        assert_eq!(
            Product::version_string("11.4.4-MariaDB-ubu2404"),
            "MariaDB 11.4.4"
        );
        assert_eq!(
            Product::version_string("5.5.5-10.6.12-MariaDB-log"),
            "MariaDB 10.6.12"
        );
        assert_eq!(Product::version_string("8.0.36-log"), "MySQL 8.0.36");
    }

    #[test]
    fn listing_queries_escape_literals() {
        let v = MariaDbVariant;
        let q = v.list_queries(&[(ObjectKind::Database, "a'b\\c".into())]);
        assert_eq!(q.len(), 2);
        assert!(q[0].sql.contains("'a''b\\\\c'"), "{}", q[0].sql);
        assert!(q[0].sql.contains("SYSTEM VERSIONED"));
        assert_eq!(q[1].kind, ObjectKind::View);
        let q = MySqlVariant.list_queries(&[
            (ObjectKind::Database, "d".into()),
            (ObjectKind::Table, "t".into()),
        ]);
        assert_eq!(q[0].kind, ObjectKind::Column);
        assert!(
            MySqlVariant
                .list_queries(&[(ObjectKind::Column, "x".into())])
                .is_empty()
        );
    }
}
