//! What Greenplum, CockroachDB and Amazon Redshift add to or take from the
//! PostgreSQL catalog. The product is recognized from `server_version()`
//! (the pgwire driver names it: `Greenplum 7.1.0 (PostgreSQL 12.12)`,
//! `CockroachDB 24.3.1`, `Redshift 1.0.77467`).
//!
//! - Greenplum: distribution policy (`pg_get_table_distributedby`), the
//!   append-optimized access method and storage options, kept in
//!   [`Table::table_options`] so DDL writes `… DISTRIBUTED BY (id)`.
//! - CockroachDB: the `CREATE` statements CockroachDB itself prints
//!   (`SHOW CREATE TABLE`: column families, hash-sharded and inverted
//!   indexes, `PRIMARY KEY` with its storage), kept in [`Table::server_sql`]
//!   and used verbatim by the DDL generator. (`crdb_internal` is restricted
//!   since CockroachDB 25.x.)
//! - Redshift: a PostgreSQL 8.0 catalog without arrays, `string_agg` or
//!   indexes; distribution style, `DISTKEY` / `SORTKEY` columns
//!   (`pg_class.reldiststyle`, `pg_attribute.attisdistkey` /
//!   `attsortkeyord`), `IDENTITY(seed,step)` columns, and external schemas
//!   (Spectrum, federated) read from `svv_external_tables` /
//!   `svv_external_columns`.

use std::collections::HashMap;

use ferrodb_api::Result;
use ferrodb_drivers::{Connection, quote_literal};

use crate::{
    model::*,
    rows::{Row, query},
};

/// The PostgreSQL-compatible product behind a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Product {
    PostgreSql,
    Greenplum,
    CockroachDb,
    Redshift,
}

impl Product {
    pub(crate) fn of(server_version: &str) -> Self {
        if server_version.starts_with("Greenplum") {
            Self::Greenplum
        } else if server_version.starts_with("CockroachDB") {
            Self::CockroachDb
        } else if server_version.starts_with("Redshift") {
            Self::Redshift
        } else {
            Self::PostgreSql
        }
    }
}

// --------------------------------------------------------------- Greenplum

fn greenplum_storage(s: &str, only: &str) -> String {
    format!(
        "select c.relname as name, pg_catalog.pg_get_table_distributedby(c.oid) as dist,
  case when am.amname in ('ao_row', 'ao_column') then am.amname end as am,
  array_to_string(c.reloptions, ', ') as opts
from pg_catalog.pg_class c
join pg_catalog.pg_namespace n on n.oid = c.relnamespace
left join pg_catalog.pg_am am on am.oid = c.relam
where n.nspname = {s} and c.relkind in ('r', 'p') and not c.relispartition{only}"
    )
}

/// `USING ao_column WITH (compresstype=zstd) DISTRIBUTED BY (id)`.
pub(crate) fn greenplum_options(r: &Row) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(am) = r.text("am") {
        parts.push(format!("USING {am}"));
    }
    if let Some(o) = r.text("opts").filter(|o| !o.is_empty()) {
        parts.push(format!("WITH ({o})"));
    }
    if let Some(d) = r.text("dist").filter(|d| !d.is_empty()) {
        parts.push(d);
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(crate) async fn greenplum(
    conn: &mut dyn Connection,
    s: &str,
    only: &str,
    tables: &mut [Table],
) -> Result<()> {
    let opts: HashMap<String, Option<String>> = query(conn, greenplum_storage(s, only))
        .await?
        .iter()
        .map(|r| (r.string("name"), greenplum_options(r)))
        .collect();
    for t in tables {
        if let Some(o) = opts.get(&t.name) {
            t.table_options = o.clone();
        }
    }
    Ok(())
}

// ------------------------------------------------------------- CockroachDB

pub(crate) async fn cockroach(
    conn: &mut dyn Connection,
    container: &str,
    tables: &mut [Table],
) -> Result<()> {
    let q = |n: &str| crate::ddl::quote(Dialect::Postgres, n);
    // View definitions name the database (`shop.public.orders`): written
    // into another database they would refer back to this one.
    let db = query(conn, "select current_database() as db")
        .await?
        .first()
        .map(|r| r.string("db"))
        .unwrap_or_default();
    if !db.is_empty() {
        for v in tables.iter_mut().filter(|t| t.is_view()) {
            if let Some(d) = &v.view_definition {
                v.view_definition = Some(strip_database(d, &db));
            }
        }
    }
    for t in tables.iter_mut().filter(|t| t.kind == TableKind::Table) {
        let sql = format!("SHOW CREATE TABLE {}.{}", q(container), q(&t.name));
        if let Some(r) = query(conn, sql).await?.first() {
            t.server_sql = r
                .text("create_statement")
                .map(|d| d.trim_end_matches(';').to_owned());
        }
        // `rowid` is CockroachDB's hidden primary key, not a column.
        t.columns
            .retain(|c| !(c.name == "rowid" && c.default.as_deref() == Some("unique_rowid()")));
    }
    Ok(())
}

/// Removes the `db.` qualifier (plain or quoted) from names in `sql`.
pub(crate) fn strip_database(sql: &str, db: &str) -> String {
    let quoted = format!("\"{}\".", db.replace('"', "\"\""));
    let plain = format!("{db}.");
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    while !rest.is_empty() {
        let prev = out.chars().next_back();
        let boundary =
            !prev.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '"');
        if boundary && rest.starts_with(&quoted) {
            rest = &rest[quoted.len()..];
        } else if boundary && rest.starts_with(&plain) {
            rest = &rest[plain.len()..];
        } else {
            let c = rest.chars().next().expect("not empty");
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

// ---------------------------------------------------------------- Redshift

const REDSHIFT_EXTERNAL: &str = "select schemaname as name, databasename as db, esoptions as opts \
     from svv_external_schemas";

fn rs_relations(s: &str, only: &str) -> String {
    format!(
        "select c.relname as name, c.relkind as kind, obj_description(c.oid) as comment,
  c.reltuples::bigint as row_estimate,
  case when c.relkind = 'v' then pg_get_viewdef(c.oid) end as view_def,
  c.reldiststyle as diststyle
from pg_class c join pg_namespace n on n.oid = c.relnamespace
where n.nspname = {s} and c.relkind in ('r', 'v'){only}
order by c.relname"
    )
}

fn rs_columns(s: &str, only: &str) -> String {
    format!(
        "select c.relname as tbl, a.attname as name, format_type(a.atttypid, a.atttypmod) as type,
  a.attnotnull as notnull, pg_get_expr(d.adbin, d.adrelid) as def,
  col_description(c.oid, a.attnum) as comment,
  a.attisdistkey as distkey, a.attsortkeyord as sortkey
from pg_attribute a
join pg_class c on c.oid = a.attrelid
join pg_namespace n on n.oid = c.relnamespace
left join pg_attrdef d on d.adrelid = a.attrelid and d.adnum = a.attnum
where n.nspname = {s} and c.relkind in ('r', 'v') and a.attnum > 0 and not a.attisdropped{only}
order by c.relname, a.attnum"
    )
}

fn rs_constraints(s: &str, only: &str) -> String {
    format!(
        "select c.relname as tbl, con.conname as name, con.contype as type,
  pg_get_constraintdef(con.oid) as def
from pg_constraint con
join pg_class c on c.oid = con.conrelid
join pg_namespace n on n.oid = c.relnamespace
where n.nspname = {s} and con.contype in ('p', 'u', 'f', 'c'){only}
order by c.relname, con.contype, con.conname"
    )
}

fn rs_routines(s: &str) -> String {
    format!(
        "select p.proname as name, oidvectortypes(p.proargtypes) as args,
  format_type(p.prorettype, null) as returns, l.lanname as language, p.prosrc as src,
  obj_description(p.oid) as comment
from pg_proc p
join pg_namespace n on n.oid = p.pronamespace
join pg_language l on l.oid = p.prolang
where n.nspname = {s}
order by p.proname"
    )
}

fn rs_external_tables(s: &str, only: &str) -> String {
    format!(
        "select tablename as name, location, input_format, output_format, serialization_lib,
  serde_parameters, parameters
from svv_external_tables where schemaname = {s}{only} order by tablename"
    )
}

fn rs_external_columns(s: &str, only: &str) -> String {
    format!(
        "select tablename as tbl, columnname as name, external_type as type, part_key,
  is_nullable
from svv_external_columns where schemaname = {s}{only} order by tablename, columnnum"
    )
}

/// `"identity"(100269, 0, '1,1'::text)` → `IDENTITY(1,1)`.
pub(crate) fn redshift_identity(default: &str) -> Option<String> {
    let rest = default.trim().strip_prefix("\"identity\"(")?;
    let q = rest.find('\'')? + 1;
    let end = rest[q..].find('\'')? + q;
    let spec = rest[q..end].replace(' ', "");
    Some(format!("IDENTITY({spec})"))
}

/// Columns of `KEY (a, b)` in a constraint definition, after `prefix`.
fn paren_list(def: &str, prefix: &str) -> Vec<String> {
    let upper = def.to_ascii_uppercase();
    let Some(i) = upper.find(prefix) else {
        return Vec::new();
    };
    let rest = &def[i + prefix.len()..];
    let (Some(open), Some(close)) = (rest.find('('), rest.find(')')) else {
        return Vec::new();
    };
    rest[open + 1..close]
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_owned())
        .filter(|c| !c.is_empty())
        .collect()
}

/// `FOREIGN KEY (a) REFERENCES s.t(b)` → (schema, table, columns).
pub(crate) fn references(def: &str) -> Option<(Option<String>, String, Vec<String>)> {
    let upper = def.to_ascii_uppercase();
    let i = upper.find("REFERENCES ")? + "REFERENCES ".len();
    let rest = &def[i..];
    let end = rest.find('(').unwrap_or(rest.len());
    let target = rest[..end].trim();
    let (schema, table) = match target.rsplit_once('.') {
        Some((s, t)) => (
            Some(s.trim_matches('"').to_owned()),
            t.trim_matches('"').to_owned(),
        ),
        None => (None, target.trim_matches('"').to_owned()),
    };
    Some((schema, table, paren_list(rest, "")))
}

/// `DISTSTYLE KEY DISTKEY (id) COMPOUND SORTKEY (ts, id)`.
pub(crate) fn redshift_options(
    diststyle: Option<i64>,
    distkey: Option<&str>,
    sort: &[(i64, String)],
) -> Option<String> {
    let mut parts = Vec::new();
    match diststyle {
        Some(0) => parts.push("DISTSTYLE EVEN".to_owned()),
        Some(1) => parts.push(match distkey {
            Some(k) => format!("DISTSTYLE KEY DISTKEY ({k})"),
            None => "DISTSTYLE KEY".to_owned(),
        }),
        Some(8) => parts.push("DISTSTYLE ALL".to_owned()),
        Some(9..=11) => parts.push("DISTSTYLE AUTO".to_owned()),
        _ => {}
    }
    if !sort.is_empty() {
        let interleaved = sort.iter().any(|(o, _)| *o < 0);
        let mut keys: Vec<&(i64, String)> = sort.iter().collect();
        keys.sort_by_key(|(o, _)| o.abs());
        let names: Vec<&str> = keys.iter().map(|(_, n)| n.as_str()).collect();
        parts.push(format!(
            "{} SORTKEY ({})",
            if interleaved {
                "INTERLEAVED"
            } else {
                "COMPOUND"
            },
            names.join(", ")
        ));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

pub(crate) fn redshift_assemble(
    rels: Vec<Row>,
    cols: Vec<Row>,
    cons: Vec<Row>,
    container: &str,
) -> Vec<Table> {
    let q = |n: &str| crate::ddl::quote(Dialect::Postgres, n);
    let mut tables: Vec<Table> = rels
        .iter()
        .map(|r| Table {
            name: r.string("name"),
            kind: if r.get("kind") == Some("v") {
                TableKind::View
            } else {
                TableKind::Table
            },
            comment: r.text("comment"),
            rows: r.int("row_estimate").filter(|n| *n >= 0),
            view_definition: r
                .text("view_def")
                .map(|d| d.trim().trim_end_matches(';').trim_end().to_owned()),
            ..Default::default()
        })
        .collect();
    let pos: HashMap<String, usize> = tables
        .iter()
        .enumerate()
        .map(|(i, t)| (t.name.clone(), i))
        .collect();
    let mut dist: HashMap<usize, String> = HashMap::new();
    let mut sort: HashMap<usize, Vec<(i64, String)>> = HashMap::new();
    for r in &cols {
        let Some(&ti) = pos.get(r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let name = r.string("name");
        let def = r.text("def");
        let identity = def.as_deref().and_then(redshift_identity);
        if r.bool("distkey") {
            dist.insert(ti, q(&name));
        }
        if let Some(o) = r.int("sortkey").filter(|o| *o != 0) {
            sort.entry(ti).or_default().push((o, q(&name)));
        }
        tables[ti].columns.push(Column {
            name,
            data_type: r.string("type"),
            nullable: !r.bool("notnull"),
            default: if identity.is_some() { None } else { def },
            comment: r.text("comment"),
            identity,
            ..Default::default()
        });
    }
    let styles: HashMap<String, Option<i64>> = rels
        .iter()
        .map(|r| (r.string("name"), r.int("diststyle")))
        .collect();
    for (i, t) in tables.iter_mut().enumerate() {
        if t.kind == TableKind::Table {
            t.table_options = redshift_options(
                styles.get(&t.name).copied().flatten(),
                dist.get(&i).map(String::as_str),
                sort.get(&i).map(Vec::as_slice).unwrap_or_default(),
            );
        }
    }
    for r in &cons {
        let Some(&ti) = pos.get(r.get("tbl").unwrap_or_default()) else {
            continue;
        };
        let def = r.string("def");
        let name = r.text("name");
        match r.get("type").unwrap_or_default() {
            "p" => {
                tables[ti].primary_key = Some(Key {
                    name,
                    columns: paren_list(&def, "PRIMARY KEY"),
                })
            }
            "u" => tables[ti].uniques.push(Key {
                name,
                columns: paren_list(&def, "UNIQUE"),
            }),
            "c" => tables[ti].checks.push(Check {
                name,
                expression: def.strip_prefix("CHECK ").unwrap_or(&def).to_owned(),
            }),
            "f" => {
                if let Some((schema, table, ref_cols)) = references(&def) {
                    tables[ti].foreign_keys.push(ForeignKey {
                        name,
                        columns: paren_list(&def, "FOREIGN KEY"),
                        ref_container: schema.filter(|s| s != container),
                        ref_table: table,
                        ref_columns: ref_cols,
                        ..Default::default()
                    });
                }
            }
            _ => {}
        }
    }
    tables
}

/// `CREATE EXTERNAL TABLE` of a Spectrum table.
pub(crate) fn external_table_sql(schema: &str, t: &Row, cols: &[&Row]) -> String {
    let q = |n: &str| crate::ddl::quote(Dialect::Postgres, n);
    let (parts, plain): (Vec<&&Row>, Vec<&&Row>) = cols
        .iter()
        .partition(|c| c.int("part_key").unwrap_or(0) > 0);
    let col = |c: &Row| format!("{} {}", q(&c.string("name")), c.string("type"));
    let mut s = format!(
        "CREATE EXTERNAL TABLE {}.{} (\n    {}\n)",
        q(schema),
        q(&t.string("name")),
        plain
            .iter()
            .map(|c| col(c))
            .collect::<Vec<_>>()
            .join(",\n    ")
    );
    if !parts.is_empty() {
        let mut parts = parts;
        parts.sort_by_key(|c| c.int("part_key").unwrap_or(0));
        s.push_str(&format!(
            "\nPARTITIONED BY ({})",
            parts.iter().map(|c| col(c)).collect::<Vec<_>>().join(", ")
        ));
    }
    if let Some(serde) = t.text("serialization_lib") {
        s.push_str(&format!("\nROW FORMAT SERDE {}", quote_literal(&serde)));
    }
    if let (Some(i), Some(o)) = (t.text("input_format"), t.text("output_format")) {
        s.push_str(&format!(
            "\nSTORED AS INPUTFORMAT {} OUTPUTFORMAT {}",
            quote_literal(&i),
            quote_literal(&o)
        ));
    }
    if let Some(l) = t.text("location") {
        s.push_str(&format!("\nLOCATION {}", quote_literal(&l)));
    }
    s
}

pub(crate) async fn redshift(
    conn: &mut dyn Connection,
    container: &str,
    only: Option<&str>,
) -> Result<Content> {
    let s = quote_literal(container);
    let external = query(conn, REDSHIFT_EXTERNAL)
        .await?
        .into_iter()
        .any(|r| r.get("name") == Some(container));
    if external {
        let narrow = only
            .map(|t| format!(" and tablename = {}", quote_literal(t)))
            .unwrap_or_default();
        let ts = query(conn, rs_external_tables(&s, &narrow)).await?;
        let cs = query(conn, rs_external_columns(&s, &narrow)).await?;
        let tables = ts
            .iter()
            .map(|t| {
                let name = t.string("name");
                let mine: Vec<&Row> = cs
                    .iter()
                    .filter(|c| c.get("tbl") == Some(name.as_str()))
                    .collect();
                Table {
                    kind: TableKind::Foreign,
                    columns: mine
                        .iter()
                        .map(|c| Column {
                            name: c.string("name"),
                            data_type: c.string("type"),
                            nullable: !c
                                .get("is_nullable")
                                .is_some_and(|v| v.eq_ignore_ascii_case("false")),
                            ..Default::default()
                        })
                        .collect(),
                    server_sql: Some(external_table_sql(container, t, &mine)),
                    name,
                    ..Default::default()
                }
            })
            .collect();
        return Ok(Content {
            tables,
            ..Default::default()
        });
    }
    let narrow = only
        .map(|t| format!(" and c.relname = {}", quote_literal(t)))
        .unwrap_or_default();
    let rels = query(conn, rs_relations(&s, &narrow)).await?;
    let cols = query(conn, rs_columns(&s, &narrow)).await?;
    let cons = query(conn, rs_constraints(&s, &narrow)).await?;
    let mut content = Content {
        tables: redshift_assemble(rels, cols, cons, container),
        ..Default::default()
    };
    if only.is_some() {
        return Ok(content);
    }
    let q = |n: &str| crate::ddl::quote(Dialect::Postgres, n);
    content.routines = query(conn, rs_routines(&s))
        .await?
        .into_iter()
        .map(|r| {
            let name = r.string("name");
            let args = r.string("args");
            let returns = r.text("returns");
            let language = r.text("language");
            let procedure =
                returns.as_deref() == Some("void") && language.as_deref() == Some("plpgsql");
            let head = if procedure {
                format!(
                    "CREATE OR REPLACE PROCEDURE {}.{}({args})",
                    q(container),
                    q(&name)
                )
            } else {
                format!(
                    "CREATE OR REPLACE FUNCTION {}.{}({args}) RETURNS {} STABLE",
                    q(container),
                    q(&name),
                    returns.clone().unwrap_or_default()
                )
            };
            Routine {
                signature: format!("{name}({args})"),
                definition: Some(format!(
                    "{head} AS $$\n{}\n$$ LANGUAGE {};",
                    r.string("src").trim(),
                    language.clone().unwrap_or_else(|| "sql".into())
                )),
                kind: if procedure {
                    RoutineKind::Procedure
                } else {
                    RoutineKind::Function
                },
                returns: returns.filter(|_| !procedure),
                language,
                comment: r.text("comment"),
                arguments: args
                    .split(',')
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                    .map(|a| Argument {
                        name: None,
                        data_type: a.to_owned(),
                        mode: "IN".into(),
                    })
                    .collect(),
                name,
            }
        })
        .collect();
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::row;

    #[test]
    fn products_and_options() {
        assert_eq!(
            Product::of("Greenplum 7.1.0 (PostgreSQL 12.12)"),
            Product::Greenplum
        );
        assert_eq!(Product::of("CockroachDB 24.3.1"), Product::CockroachDb);
        assert_eq!(Product::of("Redshift 1.0.77467"), Product::Redshift);
        assert_eq!(Product::of("PostgreSQL 18.1"), Product::PostgreSql);
        let r = row(&[
            ("am", Some("ao_column")),
            ("opts", Some("compresstype=zstd, compresslevel=5")),
            ("dist", Some("DISTRIBUTED BY (id)")),
        ]);
        assert_eq!(
            greenplum_options(&r).as_deref(),
            Some("USING ao_column WITH (compresstype=zstd, compresslevel=5) DISTRIBUTED BY (id)")
        );
        let r = row(&[
            ("am", None),
            ("opts", None),
            ("dist", Some("DISTRIBUTED REPLICATED")),
        ]);
        assert_eq!(
            greenplum_options(&r).as_deref(),
            Some("DISTRIBUTED REPLICATED")
        );
        assert_eq!(
            redshift_identity("\"identity\"(100269, 0, '1,1'::text)").as_deref(),
            Some("IDENTITY(1,1)")
        );
        assert_eq!(redshift_identity("0"), None);
        assert_eq!(
            strip_database(
                "SELECT id FROM shop.public.orders JOIN \"shop\".public.x ON myshop.a = 1",
                "shop"
            ),
            "SELECT id FROM public.orders JOIN public.x ON myshop.a = 1"
        );
        assert_eq!(
            references("FOREIGN KEY (customer_id) REFERENCES sales.customers(id)"),
            Some((Some("sales".into()), "customers".into(), vec!["id".into()]))
        );
        assert_eq!(
            redshift_options(Some(1), Some("id"), &[(2, "id".into()), (1, "ts".into())]).as_deref(),
            Some("DISTSTYLE KEY DISTKEY (id) COMPOUND SORTKEY (ts, id)")
        );
        assert_eq!(
            redshift_options(Some(10), None, &[(-1, "a".into()), (-2, "b".into())]).as_deref(),
            Some("DISTSTYLE AUTO INTERLEAVED SORTKEY (a, b)")
        );
    }

    #[test]
    fn redshift_recorded_catalog() {
        let rels = vec![row(&[
            ("name", Some("events")),
            ("kind", Some("r")),
            ("row_estimate", Some("1000")),
            ("diststyle", Some("1")),
        ])];
        let cols = vec![
            row(&[
                ("tbl", Some("events")),
                ("name", Some("id")),
                ("type", Some("bigint")),
                ("notnull", Some("true")),
                ("def", Some("\"identity\"(108242, 0, '1,1'::text)")),
                ("distkey", Some("true")),
                ("sortkey", Some("0")),
            ]),
            row(&[
                ("tbl", Some("events")),
                ("name", Some("ts")),
                ("type", Some("timestamp without time zone")),
                ("notnull", Some("false")),
                ("distkey", Some("false")),
                ("sortkey", Some("1")),
            ]),
            row(&[
                ("tbl", Some("events")),
                ("name", Some("payload")),
                ("type", Some("super")),
                ("notnull", Some("false")),
                ("distkey", Some("false")),
                ("sortkey", Some("0")),
            ]),
        ];
        let cons = vec![row(&[
            ("tbl", Some("events")),
            ("name", Some("events_pkey")),
            ("type", Some("p")),
            ("def", Some("PRIMARY KEY (id)")),
        ])];
        let t = redshift_assemble(rels, cols, cons, "public");
        assert_eq!(t[0].columns[0].identity.as_deref(), Some("IDENTITY(1,1)"));
        assert_eq!(t[0].columns[0].default, None);
        assert_eq!(t[0].primary_key.as_ref().unwrap().columns, ["id"]);
        assert_eq!(
            t[0].table_options.as_deref(),
            Some("DISTSTYLE KEY DISTKEY (id) COMPOUND SORTKEY (ts)")
        );
        let ddl = crate::ddl::table_ddl(
            &crate::ddl::Target::new(Dialect::Postgres, "public"),
            &t[0],
            &[],
        );
        assert!(ddl.contains("id bigint IDENTITY(1,1) NOT NULL"), "{ddl}");
        assert!(
            ddl.contains(") DISTSTYLE KEY DISTKEY (id) COMPOUND SORTKEY (ts);"),
            "{ddl}"
        );
        let ext = row(&[
            ("name", Some("clicks")),
            ("location", Some("s3://bucket/clicks/")),
            (
                "input_format",
                Some("org.apache.hadoop.hive.ql.io.parquet.MapredParquetInputFormat"),
            ),
            (
                "output_format",
                Some("org.apache.hadoop.hive.ql.io.parquet.MapredParquetOutputFormat"),
            ),
            (
                "serialization_lib",
                Some("org.apache.hadoop.hive.ql.io.parquet.serde.ParquetHiveSerDe"),
            ),
        ]);
        let c1 = row(&[
            ("name", Some("url")),
            ("type", Some("varchar(256)")),
            ("part_key", Some("0")),
        ]);
        let c2 = row(&[
            ("name", Some("day")),
            ("type", Some("date")),
            ("part_key", Some("1")),
        ]);
        let sql = external_table_sql("spectrum", &ext, &[&c1, &c2]);
        assert!(
            sql.starts_with("CREATE EXTERNAL TABLE spectrum.clicks (\n    url varchar(256)\n)"),
            "{sql}"
        );
        assert!(sql.contains("PARTITIONED BY (day date)"));
        assert!(sql.contains("LOCATION 's3://bucket/clicks/'"));
    }
}
