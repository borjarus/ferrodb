//! What differs between ADBC drivers (ADR-008 family variants): the
//! descriptor shown in the catalog, how connection properties become ADBC
//! database options, the explorer listings and session statements.
//!
//! Option names are those of the Apache Arrow ADBC Go drivers 1.11 (checked
//! against the Python bindings' option enums and the strings in the
//! libraries themselves).

use ferrodb_api::{
    AccessLayer, AuthMethod, Capabilities, DataModel, DriverCategory, DriverDescriptor,
    DriverProperty, Error, ExplainSupport, HierarchySpec, LanguageLevel, ObjectKind, QueryLanguage,
    Result, SchemaKind, TxSupport,
};
use ferrodb_drivers::{
    ListQuery, Quirks, ResolvedConfig, quote_literal,
    registry::{property, relational_hierarchy, secret_property},
};

/// A database reached through ADBC.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Flavor {
    Snowflake,
    BigQuery,
    /// Any other ADBC driver (SQLite, PostgreSQL, DuckDB… builds of the
    /// ADBC project, or a vendor's): generic descriptor, `uri` property.
    Generic(Generic),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Generic {
    pub id: String,
    pub name: String,
    /// SQL dialect id (`sqlite`, `postgresql`…).
    pub dialect: String,
    /// Library name without prefix / extension (`adbc_driver_sqlite`).
    pub library: String,
}

/// Snowflake option keys.
pub mod sf {
    pub const ACCOUNT: &str = "adbc.snowflake.sql.account";
    pub const WAREHOUSE: &str = "adbc.snowflake.sql.warehouse";
    pub const DATABASE: &str = "adbc.snowflake.sql.db";
    pub const SCHEMA: &str = "adbc.snowflake.sql.schema";
    pub const ROLE: &str = "adbc.snowflake.sql.role";
    pub const AUTH_TYPE: &str = "adbc.snowflake.sql.auth_type";
    pub const APP_NAME: &str = "adbc.snowflake.sql.client_option.app_name";
    pub const LOGIN_TIMEOUT: &str = "adbc.snowflake.sql.client_option.login_timeout";
    pub const JWT_KEY_FILE: &str = "adbc.snowflake.sql.client_option.jwt_private_key";
    pub const JWT_KEY_VALUE: &str = "adbc.snowflake.sql.client_option.jwt_private_key_pkcs8_value";
    pub const JWT_KEY_PASSWORD: &str =
        "adbc.snowflake.sql.client_option.jwt_private_key_pkcs8_password";
    pub const AUTH_PASSWORD: &str = "auth_snowflake";
    pub const AUTH_JWT: &str = "auth_jwt";
    pub const AUTH_EXTERNAL_BROWSER: &str = "auth_ext_browser";
}

/// BigQuery option keys.
pub mod bq {
    pub const PROJECT: &str = "adbc.bigquery.sql.project_id";
    pub const DATASET: &str = "adbc.bigquery.sql.dataset_id";
    pub const LOCATION: &str = "adbc.bigquery.sql.location";
    pub const AUTH_TYPE: &str = "adbc.bigquery.sql.auth_type";
    pub const AUTH_CREDENTIALS: &str = "adbc.bigquery.sql.auth_credentials";
    pub const AUTH_JSON_STRING: &str = "adbc.bigquery.sql.auth_type.json_credential_string";
    pub const AUTH_JSON_FILE: &str = "adbc.bigquery.sql.auth_type.json_credential_file";
    pub const AUTH_ADC: &str = "adbc.bigquery.sql.auth_type.app_default_credentials";
    /// Statement options.
    pub const DEFAULT_PROJECT: &str = "adbc.bigquery.sql.query.default_project_id";
    pub const DEFAULT_DATASET: &str = "adbc.bigquery.sql.query.default_dataset_id";
}

/// A database option: a standard key or a driver-specific one.
#[derive(Clone, PartialEq, Eq)]
pub enum DbOption {
    Uri(String),
    Username(String),
    Password(String),
    Other(String, String),
}

impl std::fmt::Debug for DbOption {
    // Secrets never reach logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Uri(u) => write!(f, "Uri({u})"),
            Self::Username(u) => write!(f, "Username({u})"),
            Self::Password(_) => f.write_str("Password(***)"),
            Self::Other(k, v) => {
                let secret = k.contains("password")
                    || k.contains("credentials")
                    || k.contains("private_key")
                    || k.contains("token");
                if secret {
                    write!(f, "Other({k}, ***)")
                } else {
                    write!(f, "Other({k}, {v})")
                }
            }
        }
    }
}

fn for_auth(mut p: DriverProperty, auth: &[AuthMethod]) -> DriverProperty {
    p.auth = auth.to_vec();
    p
}

/// The auth method chosen in the data source dialog (`auth` property,
/// `Debug` name of [`AuthMethod`]).
fn chosen_auth(cfg: &ResolvedConfig) -> Option<&str> {
    cfg.prop("auth")
}

impl Flavor {
    pub fn generic(id: &str, name: &str, dialect: &str, library: &str) -> Self {
        Self::Generic(Generic {
            id: id.into(),
            name: name.into(),
            dialect: dialect.into(),
            library: library.into(),
        })
    }

    pub fn id(&self) -> &str {
        match self {
            Self::Snowflake => "snowflake",
            Self::BigQuery => "bigquery",
            Self::Generic(g) => &g.id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Snowflake => "Snowflake",
            Self::BigQuery => "Google BigQuery",
            Self::Generic(g) => &g.name,
        }
    }

    pub fn dialect(&self) -> &str {
        match self {
            Self::Snowflake => "snowflake",
            Self::BigQuery => "googlesql",
            Self::Generic(g) => &g.dialect,
        }
    }

    /// Library name without prefix / extension.
    pub fn library(&self) -> &str {
        match self {
            Self::Snowflake => "adbc_driver_snowflake",
            Self::BigQuery => "adbc_driver_bigquery",
            Self::Generic(g) => &g.library,
        }
    }

    pub fn auth(&self) -> Vec<AuthMethod> {
        match self {
            Self::Snowflake => vec![AuthMethod::Password, AuthMethod::KeyPair, AuthMethod::Sso],
            Self::BigQuery => vec![AuthMethod::GoogleServiceAccount],
            Self::Generic(_) => vec![AuthMethod::None, AuthMethod::Password],
        }
    }

    pub fn transactions(&self) -> TxSupport {
        match self {
            Self::BigQuery => TxSupport::None,
            _ => TxSupport::MultiStatement,
        }
    }

    pub fn quirks(&self) -> Quirks {
        Quirks {
            // Snowflake commits DDL implicitly.
            transactional_ddl: matches!(self, Self::Generic(_)),
            // Enforced by the driver (statements that change data are
            // refused) when the database has no read-only session.
            read_only_sessions: true,
        }
    }

    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            data_model: DataModel::Tabular,
            language: QueryLanguage::sql(self.dialect()),
            access: AccessLayer::Adbc,
            language_level: if matches!(self, Self::Generic(_)) {
                LanguageLevel::C
            } else {
                LanguageLevel::B
            },
            transactions: self.transactions(),
            schema: SchemaKind::Strict,
            joins: true,
            explain: match self {
                Self::Snowflake | Self::BigQuery => ExplainSupport::Structured,
                Self::Generic(_) => ExplainSupport::None,
            },
            server_cancel: true,
            editable_results: true,
            cost_estimate: matches!(self, Self::BigQuery),
            auth_methods: self.auth(),
        }
    }

    pub fn properties(&self) -> Vec<DriverProperty> {
        use AuthMethod as A;
        match self {
            Self::Snowflake => vec![
                property("account", "Account", None, true),
                property("warehouse", "Warehouse", None, false),
                property("database", "Database", None, false),
                property("schema", "Schema", None, false),
                property("role", "Role", None, false),
                for_auth(
                    property("user", "User", None, true),
                    &[A::Password, A::KeyPair, A::Sso],
                ),
                secret_property("password", "Password", &[A::Password]),
                secret_property("private_key", "Private key (PEM)", &[A::KeyPair]),
                for_auth(
                    property("private_key_file", "Private key file", None, false),
                    &[A::KeyPair],
                ),
                secret_property(
                    "private_key_passphrase",
                    "Private key passphrase",
                    &[A::KeyPair],
                ),
            ],
            Self::BigQuery => vec![
                property("project", "Project", None, true),
                property("dataset", "Default dataset", None, false),
                property("location", "Location", Some("EU"), false),
                secret_property(
                    "credentials",
                    "Service account key",
                    &[A::GoogleServiceAccount],
                ),
                for_auth(
                    property(
                        "credentials_file",
                        "Service account key file (empty = application default credentials)",
                        None,
                        false,
                    ),
                    &[A::GoogleServiceAccount],
                ),
            ],
            Self::Generic(_) => vec![
                property("uri", "URI", None, true),
                property(
                    crate::locate::LIBRARY_PROPERTY,
                    "Driver library",
                    None,
                    false,
                ),
                for_auth(property("user", "User", None, false), &[A::Password]),
                secret_property("password", "Password", &[A::Password]),
            ],
        }
    }

    pub fn descriptor(&self) -> DriverDescriptor {
        DriverDescriptor {
            id: self.id().into(),
            family: "adbc".into(),
            name: self.name().into(),
            category: match self {
                Self::Generic(_) => DriverCategory::Relational,
                _ => DriverCategory::AnalyticalCloud,
            },
            capabilities: self.capabilities(),
            requirements: crate::artifacts::requirement(self.id())
                .into_iter()
                .collect(),
            properties: self.properties(),
            default_port: None,
            url_template: None,
        }
    }

    /// ADBC database options for `cfg`.
    pub fn database_options(&self, cfg: &ResolvedConfig) -> Result<Vec<DbOption>> {
        let o = |k: &str, v: &str| DbOption::Other(k.to_owned(), v.to_owned());
        let mut v = Vec::new();
        match self {
            Self::Snowflake => {
                let account = cfg.prop("account").ok_or_else(|| {
                    Error::Connection("Snowflake: the account is required".into())
                })?;
                v.push(o(sf::ACCOUNT, account));
                for (prop, key) in [
                    ("warehouse", sf::WAREHOUSE),
                    ("database", sf::DATABASE),
                    ("schema", sf::SCHEMA),
                    ("role", sf::ROLE),
                ] {
                    if let Some(x) = cfg.prop(prop) {
                        v.push(o(key, x));
                    }
                }
                if let Some(u) = cfg.user() {
                    v.push(DbOption::Username(u.to_owned()));
                }
                v.push(o(sf::APP_NAME, &cfg.application_name));
                v.push(o(
                    sf::LOGIN_TIMEOUT,
                    &format!("{}s", cfg.connect_timeout.as_secs().max(1)),
                ));
                let key = cfg.secret("private_key");
                let key_file = cfg.prop("private_key_file");
                let auth = chosen_auth(cfg).unwrap_or(if key.is_some() || key_file.is_some() {
                    "KeyPair"
                } else if cfg.password().is_some() {
                    "Password"
                } else {
                    "Sso"
                });
                match auth {
                    "KeyPair" => {
                        v.push(o(sf::AUTH_TYPE, sf::AUTH_JWT));
                        match (key, key_file) {
                            (Some(pem), _) if pem.contains("-----BEGIN") => {
                                v.push(o(sf::JWT_KEY_VALUE, pem));
                            }
                            (Some(path), _) | (None, Some(path)) => {
                                v.push(o(sf::JWT_KEY_FILE, path));
                            }
                            (None, None) => {
                                return Err(Error::Connection(
                                    "Snowflake key pair: a private key is required".into(),
                                ));
                            }
                        }
                        if let Some(p) = cfg.secret("private_key_passphrase") {
                            v.push(o(sf::JWT_KEY_PASSWORD, p));
                        }
                    }
                    "Sso" => v.push(o(sf::AUTH_TYPE, sf::AUTH_EXTERNAL_BROWSER)),
                    _ => {
                        v.push(o(sf::AUTH_TYPE, sf::AUTH_PASSWORD));
                        if let Some(p) = cfg.password() {
                            v.push(DbOption::Password(p.to_owned()));
                        }
                    }
                }
            }
            Self::BigQuery => {
                let project = cfg
                    .prop("project")
                    .ok_or_else(|| Error::Connection("BigQuery: the project is required".into()))?;
                v.push(o(bq::PROJECT, project));
                if let Some(d) = cfg.prop("dataset") {
                    v.push(o(bq::DATASET, d));
                }
                if let Some(l) = cfg.prop("location") {
                    v.push(o(bq::LOCATION, l));
                }
                match (cfg.secret("credentials"), cfg.prop("credentials_file")) {
                    (Some(json), _) => {
                        v.push(o(bq::AUTH_TYPE, bq::AUTH_JSON_STRING));
                        v.push(o(bq::AUTH_CREDENTIALS, json));
                    }
                    (None, Some(path)) => {
                        v.push(o(bq::AUTH_TYPE, bq::AUTH_JSON_FILE));
                        v.push(o(bq::AUTH_CREDENTIALS, path));
                    }
                    (None, None) => v.push(o(bq::AUTH_TYPE, bq::AUTH_ADC)),
                }
            }
            Self::Generic(_) => {
                let uri = cfg
                    .prop("uri")
                    .or_else(|| cfg.prop("file"))
                    .or_else(|| cfg.database())
                    .ok_or_else(|| Error::Connection("ADBC: a URI is required".into()))?;
                v.push(DbOption::Uri(uri.to_owned()));
                if let Some(u) = cfg.user() {
                    v.push(DbOption::Username(u.to_owned()));
                }
                if let Some(p) = cfg.password() {
                    v.push(DbOption::Password(p.to_owned()));
                }
            }
        }
        // Raw driver options from the Options tab: `option.adbc.…=value`.
        for (k, val) in cfg.options() {
            if k.starts_with("adbc.") {
                v.push(o(k, val));
            }
        }
        Ok(v)
    }

    /// Statement options set on every statement.
    pub fn statement_options(&self, cfg: &ResolvedConfig) -> Vec<(String, String)> {
        match self {
            Self::BigQuery => {
                let mut v = Vec::new();
                if let Some(p) = cfg.prop("project") {
                    v.push((bq::DEFAULT_PROJECT.to_owned(), p.to_owned()));
                }
                if let Some(d) = cfg.prop("dataset") {
                    v.push((bq::DEFAULT_DATASET.to_owned(), d.to_owned()));
                }
                v
            }
            _ => Vec::new(),
        }
    }

    pub fn hierarchy(&self) -> HierarchySpec {
        relational_hierarchy(self.container())
    }

    /// Kind of the explorer's top level.
    pub fn container(&self) -> ObjectKind {
        match self {
            Self::BigQuery => ObjectKind::Dataset,
            Self::Snowflake => ObjectKind::Schema,
            Self::Generic(g) if matches!(g.dialect.as_str(), "mysql" | "mariadb") => {
                ObjectKind::Database
            }
            Self::Generic(_) => ObjectKind::Schema,
        }
    }

    /// The query giving the current schema / dataset, when there is one.
    pub fn current_schema_sql(&self) -> Option<&'static str> {
        match self {
            Self::Snowflake => Some("select current_schema()"),
            Self::BigQuery => None,
            Self::Generic(g) => match g.dialect.as_str() {
                "sqlite" => Some("select 'main'"),
                "mysql" | "mariadb" => Some("select database()"),
                _ => Some("select current_schema()"),
            },
        }
    }

    /// Statement switching the schema, when the driver has no
    /// `adbc.connection.db_schema` option.
    pub fn use_schema_sql(&self, schema: &str) -> Option<String> {
        let dq = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
        match self {
            Self::Snowflake => Some(format!("use schema {}", dq(schema))),
            Self::Generic(g) => match g.dialect.as_str() {
                "sqlite" => None,
                "mysql" | "mariadb" => Some(format!("use `{}`", schema.replace('`', "``"))),
                _ => Some(format!("set search_path to {}", dq(schema))),
            },
            Self::BigQuery => None,
        }
    }

    pub fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        let container = self.container();
        let lq = |sql: String, kind, has_children| ListQuery {
            sql,
            kind,
            has_children,
        };
        match self {
            Self::BigQuery => match path {
                [] => vec![lq(
                    "select schema_name as name from INFORMATION_SCHEMA.SCHEMATA order by schema_name"
                        .into(),
                    ObjectKind::Dataset,
                    true,
                )],
                [(ObjectKind::Dataset, d)] => {
                    let view = format!("`{}`.INFORMATION_SCHEMA.TABLES", d.replace('`', ""));
                    [("table_type <> 'VIEW'", ObjectKind::Table), ("table_type = 'VIEW'", ObjectKind::View)]
                        .into_iter()
                        .map(|(cond, kind)| {
                            lq(
                                format!(
                                    "select table_name as name, table_type as detail from {view} \
                                     where {cond} order by table_name"
                                ),
                                kind,
                                true,
                            )
                        })
                        .collect()
                }
                [(ObjectKind::Dataset, d), (_, t)] => vec![lq(
                    format!(
                        "select column_name as name, data_type as detail from `{}`.INFORMATION_SCHEMA.COLUMNS \
                         where table_name = '{}' order by ordinal_position",
                        d.replace('`', ""),
                        t.replace('\\', "\\\\").replace('\'', "\\'")
                    ),
                    ObjectKind::Column,
                    false,
                )],
                _ => Vec::new(),
            },
            Self::Generic(g) if g.dialect == "sqlite" => match path {
                [] => vec![lq(
                    "select name from pragma_database_list order by seq".into(),
                    container,
                    true,
                )],
                [(_, s)] => {
                    let master = format!("\"{}\".sqlite_master", s.replace('"', "\"\""));
                    [("table", ObjectKind::Table), ("view", ObjectKind::View)]
                        .into_iter()
                        .map(|(ty, kind)| {
                            lq(
                                format!(
                                    "select name from {master} where type = '{ty}' \
                                     and name not like 'sqlite\\_%' escape '\\' order by name"
                                ),
                                kind,
                                true,
                            )
                        })
                        .collect()
                }
                [(_, s), (_, t)] => vec![lq(
                    format!(
                        "select name, type from pragma_table_info({}, {}) order by cid",
                        quote_literal(t),
                        quote_literal(s)
                    ),
                    ObjectKind::Column,
                    false,
                )],
                _ => Vec::new(),
            },
            // INFORMATION_SCHEMA (Snowflake: of the current database).
            _ => match path {
                [] => vec![lq(
                    "select schema_name as name from information_schema.schemata \
                     where schema_name not in ('INFORMATION_SCHEMA', 'information_schema') \
                     order by schema_name"
                        .into(),
                    container,
                    true,
                )],
                [(_, s)] => [
                    ("table_type <> 'VIEW'", ObjectKind::Table),
                    ("table_type = 'VIEW'", ObjectKind::View),
                ]
                .into_iter()
                .map(|(cond, kind)| {
                    lq(
                        format!(
                            "select table_name as name from information_schema.tables \
                             where table_schema = {} and {cond} order by table_name",
                            quote_literal(s)
                        ),
                        kind,
                        true,
                    )
                })
                .collect(),
                [(_, s), (_, t)] => vec![lq(
                    format!(
                        "select column_name as name, data_type as detail from information_schema.columns \
                         where table_schema = {} and table_name = {} order by ordinal_position",
                        quote_literal(s),
                        quote_literal(t)
                    ),
                    ObjectKind::Column,
                    false,
                )],
                _ => Vec::new(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_match_the_catalog() {
        let sf = Flavor::Snowflake.descriptor();
        assert_eq!((sf.id.as_str(), sf.family.as_str()), ("snowflake", "adbc"));
        assert_eq!(sf.capabilities.language, QueryLanguage::sql("snowflake"));
        assert_eq!(
            sf.capabilities.auth_methods,
            vec![AuthMethod::Password, AuthMethod::KeyPair, AuthMethod::Sso]
        );
        assert_eq!(sf.capabilities.access, AccessLayer::Adbc);
        for key in [
            "account",
            "warehouse",
            "database",
            "user",
            "password",
            "private_key",
        ] {
            assert!(sf.properties.iter().any(|p| p.key == key), "{key}");
        }
        let bq = Flavor::BigQuery.descriptor();
        assert_eq!(bq.capabilities.language, QueryLanguage::sql("googlesql"));
        assert!(bq.capabilities.cost_estimate);
        assert_eq!(bq.capabilities.transactions, TxSupport::None);
        assert_eq!(
            bq.capabilities.auth_methods,
            vec![AuthMethod::GoogleServiceAccount]
        );
        for key in ["project", "dataset", "location", "credentials"] {
            assert!(bq.properties.iter().any(|p| p.key == key), "{key}");
        }
        assert!(matches!(
            bq.requirements.first(),
            Some(ferrodb_api::Requirement::AdbcLibrary { license, .. }) if license == "Apache-2.0"
        ));
    }

    #[test]
    fn snowflake_options_per_auth_method() {
        let base = ResolvedConfig::new("snowflake")
            .with("account", "xy12345.eu-central-1")
            .with("warehouse", "WH")
            .with("user", "ME");
        let opts = Flavor::Snowflake
            .database_options(&base.clone().with_secret("password", "pw"))
            .unwrap();
        assert!(opts.contains(&DbOption::Other(
            sf::AUTH_TYPE.into(),
            sf::AUTH_PASSWORD.into()
        )));
        assert!(opts.contains(&DbOption::Password("pw".into())));
        assert!(!format!("{opts:?}").contains("pw)"));
        let pem = "-----BEGIN ENCRYPTED PRIVATE KEY-----\nAAA\n-----END ENCRYPTED PRIVATE KEY-----";
        let opts = Flavor::Snowflake
            .database_options(
                &base
                    .clone()
                    .with("auth", "KeyPair")
                    .with_secret("private_key", pem)
                    .with_secret("private_key_passphrase", "secret"),
            )
            .unwrap();
        assert!(opts.contains(&DbOption::Other(sf::AUTH_TYPE.into(), sf::AUTH_JWT.into())));
        assert!(opts.contains(&DbOption::Other(sf::JWT_KEY_VALUE.into(), pem.into())));
        assert!(opts.contains(&DbOption::Other(
            sf::JWT_KEY_PASSWORD.into(),
            "secret".into()
        )));
        assert!(!format!("{opts:?}").contains("secret"));
        let opts = Flavor::Snowflake
            .database_options(&base.clone().with("auth", "Sso"))
            .unwrap();
        assert!(opts.contains(&DbOption::Other(
            sf::AUTH_TYPE.into(),
            sf::AUTH_EXTERNAL_BROWSER.into()
        )));
        assert!(
            Flavor::Snowflake
                .database_options(&ResolvedConfig::new("snowflake"))
                .is_err()
        );
    }

    #[test]
    fn bigquery_options() {
        let base = ResolvedConfig::new("bigquery")
            .with("project", "my-project")
            .with("location", "EU");
        let opts = Flavor::BigQuery.database_options(&base).unwrap();
        assert!(opts.contains(&DbOption::Other(bq::AUTH_TYPE.into(), bq::AUTH_ADC.into())));
        let opts = Flavor::BigQuery
            .database_options(
                &base
                    .clone()
                    .with_secret("credentials", "{\"type\":\"service_account\"}"),
            )
            .unwrap();
        assert!(opts.contains(&DbOption::Other(
            bq::AUTH_TYPE.into(),
            bq::AUTH_JSON_STRING.into()
        )));
        let st = Flavor::BigQuery.statement_options(&base.with("dataset", "ds"));
        assert!(st.contains(&(bq::DEFAULT_DATASET.to_owned(), "ds".to_owned())));
    }
}
