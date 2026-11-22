//! The JDBC databases as data (ADR-008): descriptor, URL, explorer queries,
//! schema switching and quirks of each flavor. Ids, dialects, auth methods
//! and ports match `ferrodb-mock/src/catalog.rs`.

use ferrodb_api::{
    AccessLayer, AuthMethod, Capabilities, DataModel, DriverCategory, DriverDescriptor,
    DriverProperty, ExplainSupport, LanguageLevel, ObjectKind, QueryLanguage, Requirement,
    SchemaKind, TxSupport,
};
use ferrodb_driver_manager::{Catalog, MIN_JAVA};
use ferrodb_drivers::{
    ListQuery, ResolvedConfig, quote_ident, quote_literal,
    registry::{property, secret_property},
};

/// How a flavor can be reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reach {
    /// Client / server only.
    Server,
    /// Server, embedded file or in-memory database (H2, Derby, HyperSQL).
    Embeddable,
    /// The user gives the URL (custom drivers).
    Url,
}

#[derive(Debug)]
pub struct Flavor {
    pub id: &'static str,
    pub name: &'static str,
    pub dialect: &'static str,
    pub category: DriverCategory,
    pub level: LanguageLevel,
    pub tx: TxSupport,
    pub explain: ExplainSupport,
    pub auth: &'static [AuthMethod],
    pub port: Option<u16>,
    pub reach: Reach,
    /// The explorer's top level: schemas, or databases (catalogs) for Sybase
    /// and Hive.
    pub container: ObjectKind,
    /// The container is the JDBC catalog (`setCatalog`), not the schema.
    pub catalog_container: bool,
    /// Label of the `database` field.
    pub database_label: &'static str,
    pub default_database: Option<&'static str>,
    /// DDL commits implicitly (Oracle, H2…) or is transactional (Derby, Db2).
    pub transactional_ddl: bool,
}

const PW: &[AuthMethod] = &[AuthMethod::Password];
const PW_KRB: &[AuthMethod] = &[AuthMethod::Password, AuthMethod::Kerberos];

use DriverCategory::{AnalyticalCloud as Cloud, Relational as Rel};
use ExplainSupport::{Structured, Text};
use LanguageLevel::{A, B, C};
use TxSupport::{MultiStatement as Multi, None as NoTx};

pub const FLAVORS: &[Flavor] = &[
    Flavor {
        id: "oracle",
        name: "Oracle",
        dialect: "oracle",
        category: Rel,
        level: A,
        tx: Multi,
        explain: Structured,
        auth: PW_KRB,
        port: Some(1521),
        reach: Reach::Server,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Service name",
        default_database: Some("FREEPDB1"),
        transactional_ddl: false,
    },
    Flavor {
        id: "db2",
        name: "IBM Db2",
        dialect: "db2",
        category: Rel,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW,
        port: Some(50000),
        reach: Reach::Server,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Database",
        default_database: None,
        transactional_ddl: true,
    },
    Flavor {
        id: "sybase",
        name: "Sybase ASE",
        dialect: "sybase",
        category: Rel,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW,
        port: Some(5000),
        reach: Reach::Server,
        container: ObjectKind::Database,
        catalog_container: true,
        database_label: "Database",
        default_database: None,
        transactional_ddl: false,
    },
    Flavor {
        id: "h2",
        name: "H2",
        dialect: "h2",
        category: Rel,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW,
        port: Some(9092),
        reach: Reach::Embeddable,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Database (path or name)",
        default_database: None,
        transactional_ddl: false,
    },
    Flavor {
        id: "derby",
        name: "Apache Derby",
        dialect: "derby",
        category: Rel,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW,
        port: Some(1527),
        reach: Reach::Embeddable,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Database (path or name)",
        default_database: None,
        transactional_ddl: true,
    },
    Flavor {
        id: "hsqldb",
        name: "HyperSQL",
        dialect: "hsqldb",
        category: Rel,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW,
        port: Some(9001),
        reach: Reach::Embeddable,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Database (path or name)",
        default_database: None,
        transactional_ddl: false,
    },
    Flavor {
        id: "exasol",
        name: "Exasol",
        dialect: "exasol",
        category: Cloud,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW,
        port: Some(8563),
        reach: Reach::Server,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Schema",
        default_database: None,
        transactional_ddl: true,
    },
    Flavor {
        id: "vertica",
        name: "Vertica",
        dialect: "vertica",
        category: Cloud,
        level: B,
        tx: Multi,
        explain: Text,
        auth: PW_KRB,
        port: Some(5433),
        reach: Reach::Server,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Database",
        default_database: None,
        transactional_ddl: false,
    },
    Flavor {
        id: "hive",
        name: "Apache Hive",
        dialect: "hiveql",
        category: Cloud,
        level: B,
        tx: NoTx,
        explain: Text,
        auth: PW_KRB,
        port: Some(10000),
        reach: Reach::Server,
        container: ObjectKind::Database,
        catalog_container: false,
        database_label: "Database",
        default_database: Some("default"),
        transactional_ddl: false,
    },
    // Level C: any JDBC driver the user provides (class name + URL).
    Flavor {
        id: "jdbc",
        name: "Custom JDBC",
        dialect: "generic",
        category: Rel,
        level: C,
        tx: Multi,
        explain: ExplainSupport::None,
        auth: PW,
        port: None,
        reach: Reach::Url,
        container: ObjectKind::Schema,
        catalog_container: false,
        database_label: "Database",
        default_database: None,
        transactional_ddl: false,
    },
];

pub fn flavor(id: &str) -> Option<&'static Flavor> {
    FLAVORS.iter().find(|f| f.id == id)
}

impl Flavor {
    pub fn capabilities(&self) -> Capabilities {
        Capabilities {
            data_model: DataModel::Tabular,
            language: QueryLanguage::sql(self.dialect),
            access: AccessLayer::Jdbc,
            language_level: self.level,
            transactions: self.tx,
            schema: SchemaKind::Strict,
            joins: true,
            explain: self.explain,
            server_cancel: true,
            editable_results: true,
            cost_estimate: false,
            auth_methods: self.auth.to_vec(),
        }
    }

    pub fn properties(&self) -> Vec<DriverProperty> {
        let port = self.port.map(|p| p.to_string());
        let user = DriverProperty {
            auth: vec![AuthMethod::Password, AuthMethod::Kerberos],
            ..property("user", "User", None, false)
        };
        let password = secret_property("password", "Password", &[AuthMethod::Password]);
        let mut v = match self.reach {
            Reach::Url => vec![
                property("url", "URL", None, true),
                property("driver_class", "Driver class", None, false),
                user,
                password,
            ],
            Reach::Embeddable => vec![
                property(
                    "mode",
                    "Mode (embedded / memory / server)",
                    Some("embedded"),
                    true,
                ),
                property("database", self.database_label, None, true),
                property("host", "Host (server mode)", Some("localhost"), false),
                property("port", "Port (server mode)", port.as_deref(), false),
                user,
                password,
                property("url", "URL (overrides the fields above)", None, false),
            ],
            Reach::Server => vec![
                property("host", "Host", Some("localhost"), true),
                property("port", "Port", port.as_deref(), false),
                property(
                    "database",
                    self.database_label,
                    self.default_database,
                    false,
                ),
                user,
                password,
                property("url", "URL (overrides the fields above)", None, false),
            ],
        };
        if self.auth.contains(&AuthMethod::Kerberos) {
            v.push(DriverProperty {
                auth: vec![AuthMethod::Kerberos],
                ..property("principal", "Kerberos principal", None, false)
            });
        }
        v
    }

    pub fn url_template(&self) -> &'static str {
        match self.id {
            "oracle" => "jdbc:oracle:thin:@//{host}:{port}/{database}",
            "db2" => "jdbc:db2://{host}:{port}/{database}",
            "sybase" => "jdbc:jtds:sybase://{host}:{port}/{database}",
            "h2" => "jdbc:h2:{database}",
            "derby" => "jdbc:derby:{database};create=true",
            "hsqldb" => "jdbc:hsqldb:file:{database}",
            "exasol" => "jdbc:exa:{host}:{port};schema={database}",
            "vertica" => "jdbc:vertica://{host}:{port}/{database}",
            "hive" => "jdbc:hive2://{host}:{port}/{database}",
            _ => "{url}",
        }
    }

    /// The descriptor; requirements come from the driver manager catalog.
    pub fn descriptor(&self, catalog: &Catalog) -> DriverDescriptor {
        let mut requirements = vec![Requirement::JavaRuntime {
            min_version: MIN_JAVA,
        }];
        if let Some(a) = catalog.get(self.id)
            && let Some(v) = a.versions.first()
        {
            for f in &v.files {
                requirements.push(Requirement::JdbcArtifact {
                    coordinates: maven_coordinates(&f.url).unwrap_or_else(|| a.coordinates.clone()),
                    version: v.version.clone(),
                    sha256: f.sha256.clone(),
                    license: a.license.clone(),
                });
            }
        }
        DriverDescriptor {
            id: self.id.into(),
            family: "jdbc".into(),
            name: self.name.into(),
            category: self.category,
            capabilities: self.capabilities(),
            requirements,
            properties: self.properties(),
            default_port: self.port,
            url_template: Some(self.url_template().into()),
        }
    }

    /// The JDBC URL for `cfg`; `host` / `port` are the endpoint (possibly an
    /// SSH tunnel) for server modes.
    pub fn url(&self, cfg: &ResolvedConfig, host: &str, port: u16) -> Result<String, String> {
        if let Some(u) = cfg.prop("url") {
            return Ok(u.to_owned());
        }
        let db = cfg.database().unwrap_or_default();
        let mode = cfg.prop("mode").unwrap_or(match self.reach {
            Reach::Embeddable => "embedded",
            _ => "server",
        });
        let need_db = || {
            if db.is_empty() {
                Err(format!("{}: no database given", self.name))
            } else {
                Ok(db)
            }
        };
        let fwd = |p: &str| p.replace('\\', "/");
        Ok(match (self.id, mode) {
            ("h2", "memory") => format!("jdbc:h2:mem:{};DB_CLOSE_DELAY=-1", need_db()?),
            ("h2", "server") => format!("jdbc:h2:tcp://{host}:{port}/{}", need_db()?),
            ("h2", _) => format!("jdbc:h2:file:{}", fwd(need_db()?)),
            ("derby", "memory") => format!("jdbc:derby:memory:{};create=true", need_db()?),
            ("derby", "server") => format!("jdbc:derby://{host}:{port}/{};create=true", need_db()?),
            ("derby", _) => format!("jdbc:derby:{};create=true", fwd(need_db()?)),
            ("hsqldb", "memory") => format!("jdbc:hsqldb:mem:{}", need_db()?),
            ("hsqldb", "server") => format!("jdbc:hsqldb:hsql://{host}:{port}/{}", need_db()?),
            ("hsqldb", _) => format!("jdbc:hsqldb:file:{}", fwd(need_db()?)),
            ("oracle", _) => format!(
                "jdbc:oracle:thin:@//{host}:{port}/{}",
                if db.is_empty() { "FREEPDB1" } else { db }
            ),
            ("db2", _) => format!("jdbc:db2://{host}:{port}/{}", need_db()?),
            ("sybase", _) => format!("jdbc:jtds:sybase://{host}:{port}/{db}"),
            ("exasol", _) if db.is_empty() => format!("jdbc:exa:{host}:{port}"),
            ("exasol", _) => format!("jdbc:exa:{host}:{port};schema={db}"),
            ("vertica", _) => format!("jdbc:vertica://{host}:{port}/{db}"),
            ("hive", _) => format!(
                "jdbc:hive2://{host}:{port}/{}",
                if db.is_empty() { "default" } else { db }
            ),
            _ => return Err(format!("{}: a JDBC URL is required", self.name)),
        })
    }

    /// The flavor connects to a server (an endpoint / SSH tunnel is needed).
    pub fn uses_network(&self, cfg: &ResolvedConfig) -> bool {
        if cfg.prop("url").is_some() {
            return false;
        }
        match self.reach {
            Reach::Server => true,
            Reach::Embeddable => cfg.prop("mode") == Some("server"),
            Reach::Url => false,
        }
    }

    fn q(&self, ident: &str) -> String {
        match self.dialect {
            "hiveql" => quote_ident(ident, '`'),
            _ => quote_ident(ident, '"'),
        }
    }

    /// Statement switching the default schema; `None` = JDBC `setCatalog`.
    pub fn set_schema_sql(&self, schema: &str) -> Option<String> {
        let q = self.q(schema);
        Some(match self.id {
            "oracle" => format!("ALTER SESSION SET CURRENT_SCHEMA = {q}"),
            "vertica" => format!("SET SEARCH_PATH TO {q}"),
            "exasol" => format!("OPEN SCHEMA {q}"),
            "hive" => format!("USE {q}"),
            "sybase" | "jdbc" => return None,
            _ => format!("SET SCHEMA {q}"),
        })
    }

    /// Query returning the current schema; `None` = JDBC `getSchema` /
    /// `getCatalog`.
    pub fn current_schema_sql(&self) -> Option<&'static str> {
        Some(match self.id {
            "oracle" => "SELECT SYS_CONTEXT('USERENV', 'CURRENT_SCHEMA') FROM DUAL",
            "db2" | "derby" => "VALUES CURRENT SCHEMA",
            "h2" | "exasol" => "SELECT CURRENT_SCHEMA",
            "hsqldb" => "VALUES CURRENT_SCHEMA",
            "vertica" => "SELECT CURRENT_SCHEMA()",
            "hive" => "SELECT current_database()",
            _ => return None,
        })
    }

    /// Liveness query for drivers without `Connection.isValid` (jTDS).
    pub fn ping_sql(&self) -> Option<&'static str> {
        match self.id {
            "sybase" => Some("select 1"),
            _ => None,
        }
    }

    /// Explorer listings (phase-3 level; full introspection uses
    /// `ferrodb-meta`).
    pub fn list_queries(&self, path: &[(ObjectKind, String)]) -> Vec<ListQuery> {
        let lit = |s: &str| quote_literal(s);
        let one = |sql: String, kind, has_children| {
            vec![ListQuery {
                sql,
                kind,
                has_children,
            }]
        };
        let tables_views = |t: String, v: String| {
            vec![
                ListQuery {
                    sql: t,
                    kind: ObjectKind::Table,
                    has_children: true,
                },
                ListQuery {
                    sql: v,
                    kind: ObjectKind::View,
                    has_children: true,
                },
            ]
        };
        let c = self.container;
        match (self.id, path) {
            (_, []) => {
                let sql = match self.id {
                    "h2" | "hsqldb" => {
                        "select schema_name from information_schema.schemata order by schema_name"
                    }
                    "derby" => "select schemaname from sys.sysschemas order by schemaname",
                    "oracle" => "select username from all_users order by username",
                    "db2" => "select rtrim(schemaname) from syscat.schemata order by schemaname",
                    "sybase" => "select name from master..sysdatabases order by name",
                    "vertica" => "select schema_name from v_catalog.schemata order by schema_name",
                    "exasol" => "select schema_name from exa_schemas order by schema_name",
                    "hive" => "show databases",
                    _ => return Vec::new(),
                };
                one(sql.into(), c, true)
            }
            (_, [(k, s)]) if *k == c => {
                let s_lit = lit(s);
                match self.id {
                    "h2" | "hsqldb" => tables_views(
                        format!(
                            "select table_name from information_schema.tables where table_schema = {s_lit} \
                             and table_type in ('BASE TABLE', 'TABLE') order by table_name"
                        ),
                        format!(
                            "select table_name from information_schema.tables where table_schema = {s_lit} \
                             and table_type = 'VIEW' order by table_name"
                        ),
                    ),
                    "derby" => {
                        let q = |ty: &str| {
                            format!(
                                "select t.tablename from sys.systables t join sys.sysschemas s \
                                 on t.schemaid = s.schemaid where s.schemaname = {s_lit} \
                                 and t.tabletype = '{ty}' order by t.tablename"
                            )
                        };
                        tables_views(q("T"), q("V"))
                    }
                    "oracle" => tables_views(
                        format!(
                            "select table_name from all_tables where owner = {s_lit} order by table_name"
                        ),
                        format!(
                            "select view_name from all_views where owner = {s_lit} order by view_name"
                        ),
                    ),
                    "db2" => {
                        let q = |ty: &str| {
                            format!(
                                "select tabname from syscat.tables where tabschema = {s_lit} \
                                 and type = '{ty}' order by tabname"
                            )
                        };
                        tables_views(q("T"), q("V"))
                    }
                    "sybase" => {
                        let q = |ty: &str| {
                            format!(
                                "select name from {s}..sysobjects where type = '{ty}' order by name"
                            )
                        };
                        tables_views(q("U"), q("V"))
                    }
                    "vertica" => tables_views(
                        format!(
                            "select table_name from v_catalog.tables where table_schema = {s_lit} order by table_name"
                        ),
                        format!(
                            "select table_name from v_catalog.views where table_schema = {s_lit} order by table_name"
                        ),
                    ),
                    "exasol" => tables_views(
                        format!(
                            "select table_name from exa_all_tables where table_schema = {s_lit} order by table_name"
                        ),
                        format!(
                            "select view_name from exa_all_views where view_schema = {s_lit} order by view_name"
                        ),
                    ),
                    "hive" => tables_views(
                        format!("show tables in {}", self.q(s)),
                        format!("show views in {}", self.q(s)),
                    ),
                    _ => Vec::new(),
                }
            }
            (_, [(k, s), (ObjectKind::Table | ObjectKind::View, t)]) if *k == c => {
                let (s_lit, t_lit) = (lit(s), lit(t));
                let sql = match self.id {
                    "h2" | "hsqldb" => format!(
                        "select column_name, data_type from information_schema.columns \
                         where table_schema = {s_lit} and table_name = {t_lit} order by ordinal_position"
                    ),
                    "derby" => format!(
                        "select c.columnname from sys.syscolumns c join sys.systables t on c.referenceid = t.tableid \
                         join sys.sysschemas s on t.schemaid = s.schemaid \
                         where s.schemaname = {s_lit} and t.tablename = {t_lit} order by c.columnnumber"
                    ),
                    "oracle" => format!(
                        "select column_name, data_type from all_tab_columns \
                         where owner = {s_lit} and table_name = {t_lit} order by column_id"
                    ),
                    "db2" => format!(
                        "select colname, typename from syscat.columns \
                         where tabschema = {s_lit} and tabname = {t_lit} order by colno"
                    ),
                    "sybase" => format!(
                        "select c.name, t.name from {s}..syscolumns c join {s}..systypes t on c.usertype = t.usertype \
                         where c.id = object_id({}) order by c.colid",
                        lit(&format!("{s}..{t}"))
                    ),
                    "vertica" => format!(
                        "select column_name, data_type from v_catalog.columns \
                         where table_schema = {s_lit} and table_name = {t_lit} order by ordinal_position"
                    ),
                    "exasol" => format!(
                        "select column_name, column_type from exa_all_columns \
                         where column_schema = {s_lit} and column_table = {t_lit} order by column_ordinal_position"
                    ),
                    "hive" => format!("describe {}.{}", self.q(s), self.q(t)),
                    _ => return Vec::new(),
                };
                one(sql, ObjectKind::Column, false)
            }
            _ => Vec::new(),
        }
    }
}

/// `group:artifact` from a Maven Central URL.
fn maven_coordinates(url: &str) -> Option<String> {
    let path = url.strip_prefix("https://repo1.maven.org/maven2/")?;
    let parts: Vec<&str> = path.split('/').collect();
    // group/…/artifact/version/file
    if parts.len() < 4 {
        return None;
    }
    let artifact = parts[parts.len() - 3];
    let group = parts[..parts.len() - 3].join(".");
    Some(format!("{group}:{artifact}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_match_the_catalog_of_26_databases() {
        let c = Catalog::builtin();
        for (id, dialect, port) in [
            ("oracle", "oracle", Some(1521)),
            ("db2", "db2", Some(50000)),
            ("sybase", "sybase", Some(5000)),
            ("h2", "h2", Some(9092)),
            ("derby", "derby", Some(1527)),
            ("hsqldb", "hsqldb", Some(9001)),
            ("exasol", "exasol", Some(8563)),
            ("vertica", "vertica", Some(5433)),
            ("hive", "hiveql", Some(10000)),
        ] {
            let d = flavor(id).unwrap().descriptor(&c);
            assert_eq!(d.family.as_str(), "jdbc");
            assert_eq!(d.default_port, port);
            assert_eq!(d.capabilities.language, QueryLanguage::sql(dialect));
            assert_eq!(d.capabilities.access, AccessLayer::Jdbc);
            assert!(matches!(
                d.requirements[0],
                Requirement::JavaRuntime { min_version: 17 }
            ));
            assert!(
                d.requirements.iter().any(
                    |r| matches!(r, Requirement::JdbcArtifact { sha256, .. } if sha256.len() == 64)
                ),
                "{id}"
            );
        }
        assert_eq!(flavor("hive").unwrap().tx, TxSupport::None);
        let d = flavor("derby").unwrap().descriptor(&c);
        let coords: Vec<String> = d
            .requirements
            .iter()
            .filter_map(|r| match r {
                Requirement::JdbcArtifact { coordinates, .. } => Some(coordinates.clone()),
                _ => None,
            })
            .collect();
        assert!(coords.contains(&"org.apache.derby:derbyclient".to_owned()));
    }

    #[test]
    fn urls_per_flavor_and_mode() {
        let url = |id: &str, cfg: ResolvedConfig| {
            let f = flavor(id).unwrap();
            f.url(&cfg, "db.example", f.port.unwrap_or(0)).unwrap()
        };
        let base = |id: &str| ResolvedConfig::new(id);
        assert_eq!(
            url(
                "h2",
                base("h2").with("mode", "memory").with("database", "t")
            ),
            "jdbc:h2:mem:t;DB_CLOSE_DELAY=-1"
        );
        assert_eq!(
            url("h2", base("h2").with("database", r"C:\data\x")),
            "jdbc:h2:file:C:/data/x"
        );
        assert_eq!(
            url(
                "derby",
                base("derby").with("mode", "server").with("database", "d")
            ),
            "jdbc:derby://db.example:1527/d;create=true"
        );
        assert_eq!(
            url(
                "hsqldb",
                base("hsqldb").with("mode", "memory").with("database", "m")
            ),
            "jdbc:hsqldb:mem:m"
        );
        assert_eq!(
            url("oracle", base("oracle")),
            "jdbc:oracle:thin:@//db.example:1521/FREEPDB1"
        );
        assert_eq!(
            url("sybase", base("sybase").with("database", "pubs2")),
            "jdbc:jtds:sybase://db.example:5000/pubs2"
        );
        assert_eq!(
            url("hive", base("hive")),
            "jdbc:hive2://db.example:10000/default"
        );
        assert_eq!(url("exasol", base("exasol")), "jdbc:exa:db.example:8563");
        assert_eq!(
            url("vertica", base("vertica").with("url", "jdbc:vertica://x/y")),
            "jdbc:vertica://x/y"
        );
        let f = flavor("db2").unwrap();
        assert!(f.url(&base("db2"), "h", 1).is_err(), "Db2 needs a database");
        assert!(!flavor("h2").unwrap().uses_network(&base("h2")));
        assert!(
            flavor("h2")
                .unwrap()
                .uses_network(&base("h2").with("mode", "server"))
        );
    }

    #[test]
    fn schema_statements_and_listings() {
        assert_eq!(
            flavor("oracle").unwrap().set_schema_sql("HR").unwrap(),
            "ALTER SESSION SET CURRENT_SCHEMA = \"HR\""
        );
        assert_eq!(
            flavor("hive").unwrap().set_schema_sql("db").unwrap(),
            "USE `db`"
        );
        assert!(flavor("sybase").unwrap().set_schema_sql("pubs2").is_none());
        for f in FLAVORS.iter().filter(|f| f.id != "jdbc") {
            let root = f.list_queries(&[]);
            assert_eq!(root.len(), 1, "{}", f.id);
            let s = f.list_queries(&[(f.container, "S".into())]);
            assert_eq!(s.len(), 2, "{}", f.id);
            let cols =
                f.list_queries(&[(f.container, "S".into()), (ObjectKind::Table, "T".into())]);
            assert_eq!(cols[0].kind, ObjectKind::Column, "{}", f.id);
        }
        assert_eq!(
            maven_coordinates(
                "https://repo1.maven.org/maven2/com/ibm/db2/jcc/12.1.5.0/jcc-12.1.5.0.jar"
            )
            .as_deref(),
            Some("com.ibm.db2:jcc")
        );
    }
}
