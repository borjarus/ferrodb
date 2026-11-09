//! PostgreSQL wire-protocol family (`tokio-postgres`, rustls).
//!
//! Every product speaking the protocol is a [`Flavor`]: a static description
//! (id, name, defaults, quirks, product name in `server_version`) plus the
//! listing queries. PostgreSQL ships now; Greenplum, CockroachDB and
//! Redshift become further flavors with their own overrides (ADR-008).

mod conn;
mod decode;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use ferrodb_api::{
    AuthMethod, DriverDescriptor, DriverId, HierarchySpec, ObjectKind, Result, SqlDialectId,
};

use crate::{
    BoxFuture, Connection, Driver, FamilyVariant, ListQuery, Quirks, ResolvedConfig, quote_literal,
    registry::{descriptor, network_properties, relational_capabilities, relational_hierarchy},
};

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
};

pub fn drivers() -> Vec<Arc<dyn Driver>> {
    vec![Arc::new(PgDriver(&POSTGRESQL))]
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

impl Driver for PgDriver {
    fn descriptor(&self) -> DriverDescriptor {
        let f = self.0;
        descriptor(
            f.id,
            "pgwire",
            f.name,
            relational_capabilities(f.dialect, (f.auth)()),
            network_properties(f.default_port, f.default_database),
            Some(f.default_port),
            f.url_template,
        )
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
