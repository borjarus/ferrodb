//! The catalog as the explorer sees it: hierarchy per database, children
//! of a node, object lookup by node path, name resolution ("Go to
//! declaration"), quick documentation and Search Everywhere entries.
//!
//! Node paths: `[(Schema|Database, c)]` → `[(Table|View|MaterializedView|
//! Function|Procedure|Sequence|Collection, name)]` → `[(Column|PrimaryKey|
//! ForeignKey|Index|Trigger, name)]`. Routines are named by signature
//! (`f(integer, text)`) so overloads stay distinct.

use ferrodb_api::{
    DataSourceId, DbObject, HierarchyLevel, HierarchySpec, NodeRef, ObjectDoc, ObjectKind,
    SearchEntry, SearchTarget,
};

use crate::{ddl::Object, model::*};

/// Kind of the top-level containers.
pub fn container_kind(d: Dialect) -> ObjectKind {
    match d {
        Dialect::Postgres | Dialect::Sqlite => ObjectKind::Schema,
        Dialect::MySql | Dialect::MariaDb | Dialect::Mongo => ObjectKind::Database,
    }
}

pub fn hierarchy(d: Dialect) -> HierarchySpec {
    use ObjectKind::*;
    let level = |kind, children: &[ObjectKind]| HierarchyLevel {
        kind,
        children: children.to_vec(),
    };
    let table_children = [Column, PrimaryKey, ForeignKey, Index, Trigger];
    let levels = match d {
        Dialect::Postgres => vec![
            level(
                Schema,
                &[Table, View, MaterializedView, Function, Procedure, Sequence],
            ),
            level(Table, &table_children),
            level(View, &[Column, Trigger]),
            level(MaterializedView, &[Column, Index]),
        ],
        Dialect::MySql | Dialect::MariaDb => vec![
            level(Database, &[Table, View, Function, Procedure, Sequence]),
            level(Table, &table_children),
            level(View, &[Column]),
        ],
        Dialect::Sqlite => vec![
            level(Schema, &[Table, View]),
            level(Table, &table_children),
            level(View, &[Column, Trigger]),
        ],
        Dialect::Mongo => vec![
            level(Database, &[Collection, View]),
            level(Collection, &[Index]),
            level(View, &[]),
        ],
    };
    HierarchySpec {
        levels,
        key_separator: None,
    }
}

/// Include / exclude patterns of the `schema.filter` property:
/// `public, sales_*, !tmp_*` (case-insensitive, `*` and `?` wildcards).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    include: Vec<String>,
    exclude: Vec<String>,
}

impl Filter {
    pub fn parse(spec: &str) -> Self {
        let mut f = Self::default();
        for p in spec
            .split([',', ';', '\n'])
            .map(|p| p.trim().to_lowercase())
            .filter(|p| !p.is_empty())
        {
            match p.strip_prefix('!') {
                Some(x) => f.exclude.push(x.trim().to_owned()),
                None => f.include.push(p),
            }
        }
        f
    }

    pub fn matches(&self, name: &str) -> bool {
        let n = name.to_lowercase();
        (self.include.is_empty() || self.include.iter().any(|p| glob(p, &n)))
            && !self.exclude.iter().any(|p| glob(p, &n))
    }
}

fn glob(p: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = p.chars().collect();
    let s: Vec<char> = s.chars().collect();
    go(&p, &s)
}

fn table_kind(t: &Table) -> ObjectKind {
    match t.kind {
        TableKind::View => ObjectKind::View,
        TableKind::MaterializedView => ObjectKind::MaterializedView,
        TableKind::Table | TableKind::Foreign => ObjectKind::Table,
    }
}

fn routine_kind(r: &Routine) -> ObjectKind {
    match r.kind {
        RoutineKind::Function => ObjectKind::Function,
        RoutineKind::Procedure => ObjectKind::Procedure,
    }
}

/// Tree name of a routine: its signature where overloads exist.
fn routine_name(r: &Routine) -> &str {
    if r.signature.is_empty() {
        &r.name
    } else {
        &r.signature
    }
}

pub fn pk_label(k: &Key) -> String {
    k.name.clone().unwrap_or_else(|| "PRIMARY KEY".into())
}

pub fn unique_label(k: &Key) -> String {
    k.name
        .clone()
        .unwrap_or_else(|| format!("UNIQUE ({})", k.columns.join(", ")))
}

pub fn fk_label(f: &ForeignKey) -> String {
    f.name
        .clone()
        .unwrap_or_else(|| format!("FK ({}) → {}", f.columns.join(", "), f.ref_table))
}

fn obj(
    node: NodeRef,
    kind: ObjectKind,
    name: &str,
    detail: Option<String>,
    children: bool,
) -> DbObject {
    DbObject {
        node,
        kind,
        name: name.to_owned(),
        detail: detail.filter(|d| !d.is_empty()),
        has_children: children,
    }
}

/// Containers as explorer objects.
pub fn containers(
    ds: &DataSourceId,
    d: Dialect,
    list: &[Container],
    filter: &Filter,
) -> Vec<DbObject> {
    let root = NodeRef::root(ds.clone());
    let kind = container_kind(d);
    list.iter()
        .filter(|c| filter.matches(&c.name))
        .map(|c| {
            obj(
                root.child(kind, &c.name),
                kind,
                &c.name,
                c.comment.clone(),
                true,
            )
        })
        .collect()
}

/// Objects directly in a container.
pub fn container_children(node: &NodeRef, content: &Content) -> Vec<DbObject> {
    let mut out = Vec::new();
    for t in &content.tables {
        let kind = table_kind(t);
        out.push(obj(
            node.child(kind, &t.name),
            kind,
            &t.name,
            t.comment.clone(),
            true,
        ));
    }
    for c in &content.collections {
        let (kind, detail) = match c.kind {
            CollectionKind::View => (ObjectKind::View, None),
            CollectionKind::TimeSeries => (ObjectKind::Collection, Some("time series".to_owned())),
            CollectionKind::Collection => (ObjectKind::Collection, None),
        };
        out.push(obj(
            node.child(kind, &c.name),
            kind,
            &c.name,
            detail,
            kind == ObjectKind::Collection,
        ));
    }
    let mut routines: Vec<&Routine> = content.routines.iter().collect();
    routines.sort_by_key(|r| (r.kind == RoutineKind::Procedure, r.signature.clone()));
    for r in routines {
        let kind = routine_kind(r);
        out.push(obj(
            node.child(kind, routine_name(r)),
            kind,
            routine_name(r),
            r.returns.clone(),
            false,
        ));
    }
    for s in &content.sequences {
        out.push(obj(
            node.child(ObjectKind::Sequence, &s.name),
            ObjectKind::Sequence,
            &s.name,
            s.data_type.clone(),
            false,
        ));
    }
    out
}

/// Columns, keys, indexes and triggers of a table.
pub fn table_children(node: &NodeRef, t: &Table) -> Vec<DbObject> {
    let mut out: Vec<DbObject> = t
        .columns
        .iter()
        .map(|c| {
            obj(
                node.child(ObjectKind::Column, &c.name),
                ObjectKind::Column,
                &c.name,
                Some(c.data_type.clone()),
                false,
            )
        })
        .collect();
    if let Some(pk) = &t.primary_key {
        let n = pk_label(pk);
        out.push(obj(
            node.child(ObjectKind::PrimaryKey, &n),
            ObjectKind::PrimaryKey,
            &n,
            Some(pk.columns.join(", ")),
            false,
        ));
    }
    for f in &t.foreign_keys {
        let n = fk_label(f);
        out.push(obj(
            node.child(ObjectKind::ForeignKey, &n),
            ObjectKind::ForeignKey,
            &n,
            // `→ table(columns)`: the ER diagram reads the target from it.
            Some(format!("→ {}({})", f.ref_table, f.ref_columns.join(", "))),
            false,
        ));
    }
    for u in &t.uniques {
        let n = unique_label(u);
        out.push(obj(
            node.child(ObjectKind::Index, &n),
            ObjectKind::Index,
            &n,
            Some(format!("unique ({})", u.columns.join(", "))),
            false,
        ));
    }
    for i in &t.indexes {
        out.push(obj(
            node.child(ObjectKind::Index, &i.name),
            ObjectKind::Index,
            &i.name,
            Some(format!(
                "{}({})",
                if i.unique { "unique " } else { "" },
                i.columns.join(", ")
            )),
            false,
        ));
    }
    for tr in &t.triggers {
        out.push(obj(
            node.child(ObjectKind::Trigger, &tr.name),
            ObjectKind::Trigger,
            &tr.name,
            tr.event.clone(),
            false,
        ));
    }
    out
}

pub fn collection_children(node: &NodeRef, c: &Collection) -> Vec<DbObject> {
    c.indexes
        .iter()
        .map(|i| {
            obj(
                node.child(ObjectKind::Index, &i.name),
                ObjectKind::Index,
                &i.name,
                Some(if i.unique {
                    format!("unique {}", i.keys)
                } else {
                    i.keys.clone()
                }),
                false,
            )
        })
        .collect()
}

/// What a node path points to inside a loaded container.
#[derive(Clone, Copy, Debug)]
pub enum Found<'a> {
    Container(&'a Container, &'a Content),
    Object(Object<'a>),
    Collection(&'a Collection),
    MongoIndex(&'a Collection, &'a MongoIndex),
}

pub fn find_table<'a>(content: &'a Content, name: &str) -> Option<&'a Table> {
    content.tables.iter().find(|t| t.name == name)
}

/// Looks up `path` (below the container) in `content`.
pub fn find<'a>(
    container: &'a Container,
    content: &'a Content,
    path: &[(ObjectKind, String)],
) -> Option<Found<'a>> {
    use ObjectKind::*;
    let Some(((kind, name), rest)) = path.split_first() else {
        return Some(Found::Container(container, content));
    };
    match kind {
        Table | View | MaterializedView => {
            if let Some(t) = find_table(content, name) {
                return find_in_table(t, rest);
            }
            let c = content.collections.iter().find(|c| &c.name == name)?;
            find_in_collection(c, rest)
        }
        Collection => {
            let c = content.collections.iter().find(|c| &c.name == name)?;
            find_in_collection(c, rest)
        }
        Function | Procedure => content
            .routines
            .iter()
            .find(|r| routine_name(r) == name || r.name == *name)
            .map(|r| Found::Object(Object::Routine(r))),
        Sequence => content
            .sequences
            .iter()
            .find(|s| &s.name == name)
            .map(|s| Found::Object(Object::Sequence(s))),
        _ => None,
    }
}

fn find_in_collection<'a>(c: &'a Collection, rest: &[(ObjectKind, String)]) -> Option<Found<'a>> {
    match rest {
        [] => Some(Found::Collection(c)),
        [(ObjectKind::Index, n)] => c
            .indexes
            .iter()
            .find(|i| &i.name == n)
            .map(|i| Found::MongoIndex(c, i)),
        _ => None,
    }
}

fn find_in_table<'a>(t: &'a Table, rest: &[(ObjectKind, String)]) -> Option<Found<'a>> {
    use ObjectKind::*;
    let o = match rest {
        [] => Object::Table(t),
        [(Column, n)] => Object::Column(t, t.column(n)?),
        [(PrimaryKey, _)] => Object::PrimaryKey(t, t.primary_key.as_ref()?),
        [(ForeignKey, n)] => {
            Object::ForeignKey(t, t.foreign_keys.iter().find(|f| &fk_label(f) == n)?)
        }
        [(Index, n)] => match t.indexes.iter().find(|i| &i.name == n) {
            Some(i) => Object::Index(t, i),
            None => Object::Unique(t, t.uniques.iter().find(|u| &unique_label(u) == n)?),
        },
        [(Trigger, n)] => Object::Trigger(t, t.triggers.iter().find(|x| &x.name == n)?),
        _ => return None,
    };
    Some(Found::Object(o))
}

/// Objects named by `parts`, best matches first: exact names before
/// case-insensitive ones, containers in listing order (current schema first).
pub fn resolve(
    ds: &DataSourceId,
    d: Dialect,
    catalog: &Catalog,
    parts: &[String],
) -> Vec<DbObject> {
    let ck = container_kind(d);
    let root = NodeRef::root(ds.clone());
    let mut out = Vec::new();
    for exact in [true, false] {
        let eq = |a: &str, b: &str| {
            if exact {
                a == b
            } else {
                a.eq_ignore_ascii_case(b)
            }
        };
        for (c, content) in catalog.loaded() {
            let cnode = root.child(ck, &c.name);
            let tops = container_children(&cnode, content);
            let named = |n: &str| -> Vec<DbObject> {
                tops.iter()
                    .filter(|o| {
                        eq(&o.name, n) || eq(o.name.split('(').next().unwrap_or_default(), n)
                    })
                    .cloned()
                    .collect()
            };
            let column = |t: &str, col: &str| {
                content
                    .tables
                    .iter()
                    .filter(|x| eq(&x.name, t))
                    .flat_map(|x| {
                        let tnode = cnode.child(table_kind(x), &x.name);
                        table_children(&tnode, x)
                            .into_iter()
                            .filter(|o| o.kind == ObjectKind::Column && eq(&o.name, col))
                    })
                    .collect::<Vec<_>>()
            };
            match parts {
                [one] => {
                    out.extend(named(one));
                    if eq(&c.name, one) {
                        out.push(obj(cnode.clone(), ck, &c.name, None, true));
                    }
                }
                [a, b] => {
                    if eq(&c.name, a) {
                        out.extend(named(b));
                    }
                    out.extend(column(a, b));
                }
                [a, b, col] if eq(&c.name, a) => out.extend(column(b, col)),
                _ => {}
            }
        }
        if !out.is_empty() {
            break;
        }
    }
    out.dedup_by(|a, b| a.node == b.node);
    out
}

fn column_text(c: &Column) -> String {
    let mut s = c.data_type.clone();
    if !c.nullable {
        s.push_str(", not null");
    }
    if let Some(d) = &c.default {
        s.push_str(&format!(", default {d}"));
    }
    if c.serial || c.auto_increment || c.identity.is_some() {
        s.push_str(", auto");
    }
    if let Some(g) = &c.generated {
        s.push_str(&format!(", generated as {g}"));
    }
    if let Some(cm) = &c.comment {
        s.push_str(&format!(" — {cm}"));
    }
    s
}

/// Quick documentation of what a path points to.
pub fn documentation(d: Dialect, container: &str, found: Found<'_>) -> ObjectDoc {
    let loc = |what: &str| format!("{what} in {container}");
    match found {
        Found::Container(c, content) => ObjectDoc {
            title: c.name.clone(),
            subtitle: format!("{:?}", container_kind(d)).to_lowercase(),
            fields: vec![
                (
                    "tables".into(),
                    content
                        .tables
                        .iter()
                        .filter(|t| !t.is_view())
                        .count()
                        .to_string(),
                ),
                (
                    "views".into(),
                    content
                        .tables
                        .iter()
                        .filter(|t| t.is_view())
                        .count()
                        .to_string(),
                ),
                ("routines".into(), content.routines.len().to_string()),
                ("collections".into(), content.collections.len().to_string()),
            ]
            .into_iter()
            .filter(|(_, v)| v != "0")
            .collect(),
            body: c.comment.clone(),
        },
        Found::Object(Object::Table(t)) => {
            let what = match t.kind {
                TableKind::View => "view",
                TableKind::MaterializedView => "materialized view",
                TableKind::Foreign => "foreign table",
                TableKind::Table => "table",
            };
            let mut subtitle = loc(what);
            if let Some(r) = t.rows.filter(|r| *r > 0) {
                subtitle.push_str(&format!(" · ~{r} rows"));
            }
            let mut fields: Vec<(String, String)> = t
                .columns
                .iter()
                .map(|c| {
                    let mut text = column_text(c);
                    if t.primary_key
                        .as_ref()
                        .is_some_and(|k| k.columns.contains(&c.name))
                    {
                        text = format!("PK, {text}");
                    }
                    if let Some(f) = t.foreign_keys.iter().find(|f| f.columns.contains(&c.name)) {
                        text.push_str(&format!(" → {}", f.ref_table));
                    }
                    (c.name.clone(), text)
                })
                .collect();
            for i in &t.indexes {
                fields.push((
                    i.name.clone(),
                    format!(
                        "{}index ({})",
                        if i.unique { "unique " } else { "" },
                        i.columns.join(", ")
                    ),
                ));
            }
            ObjectDoc {
                title: t.name.clone(),
                subtitle,
                fields,
                body: match (&t.comment, &t.view_definition) {
                    (Some(c), Some(v)) => Some(format!("{c}\n\n{v}")),
                    (Some(c), None) => Some(c.clone()),
                    (None, v) => v.clone(),
                },
            }
        }
        Found::Object(Object::Column(t, c)) => ObjectDoc {
            title: format!("{}.{}", t.name, c.name),
            subtitle: loc("column"),
            fields: vec![
                ("type".into(), c.data_type.clone()),
                ("nullable".into(), c.nullable.to_string()),
            ]
            .into_iter()
            .chain(c.default.clone().map(|v| ("default".into(), v)))
            .chain(c.generated.clone().map(|v| ("generated".into(), v)))
            .collect(),
            body: c.comment.clone(),
        },
        Found::Object(Object::Routine(r)) => ObjectDoc {
            title: routine_name(r).to_owned(),
            subtitle: loc(match r.kind {
                RoutineKind::Function => "function",
                RoutineKind::Procedure => "procedure",
            }),
            fields: r
                .arguments
                .iter()
                .enumerate()
                .map(|(i, a)| {
                    (
                        a.name.clone().unwrap_or_else(|| format!("${}", i + 1)),
                        format!("{} {}", a.mode, a.data_type),
                    )
                })
                .chain(r.returns.clone().map(|x| ("returns".into(), x)))
                .chain(r.language.clone().map(|x| ("language".into(), x)))
                .collect(),
            body: match (&r.comment, &r.definition) {
                (Some(c), Some(d)) => Some(format!("{c}\n\n{d}")),
                (c, d) => c.clone().or(d.clone()),
            },
        },
        Found::Object(o) => {
            let t = crate::ddl::Target::new(d, container);
            let (title, what) = match o {
                Object::PrimaryKey(_, k) => (pk_label(k), "primary key"),
                Object::Unique(_, k) => (unique_label(k), "unique constraint"),
                Object::ForeignKey(_, f) => (fk_label(f), "foreign key"),
                Object::Index(_, i) => (i.name.clone(), "index"),
                Object::Trigger(_, x) => (x.name.clone(), "trigger"),
                Object::Sequence(s) => (s.name.clone(), "sequence"),
                _ => (String::new(), "object"),
            };
            ObjectDoc {
                title,
                subtitle: loc(what),
                fields: Vec::new(),
                body: Some(crate::ddl::object_ddl(&t, o).trim().to_owned()),
            }
        }
        Found::Collection(c) => ObjectDoc {
            title: c.name.clone(),
            subtitle: loc(match c.kind {
                CollectionKind::View => "view",
                CollectionKind::TimeSeries => "time series collection",
                CollectionKind::Collection => "collection",
            }),
            fields: c
                .indexes
                .iter()
                .map(|i| (i.name.clone(), i.keys.clone()))
                .collect(),
            body: c.validator.clone().or(c.options.clone()),
        },
        Found::MongoIndex(_, i) => ObjectDoc {
            title: i.name.clone(),
            subtitle: loc("index"),
            fields: vec![("keys".into(), i.keys.clone())]
                .into_iter()
                .chain(i.options.clone().map(|o| ("options".into(), o)))
                .collect(),
            body: None,
        },
    }
}

/// Search Everywhere entries of a catalog.
pub fn search_entries(
    ds: &DataSourceId,
    source_name: &str,
    d: Dialect,
    catalog: &Catalog,
    filter: &Filter,
) -> Vec<SearchEntry> {
    let ck = container_kind(d);
    let root = NodeRef::root(ds.clone());
    let mut out = Vec::new();
    for (c, content) in catalog.loaded() {
        if !filter.matches(&c.name) {
            continue;
        }
        let cnode = root.child(ck, &c.name);
        let location = format!("{source_name} / {}", c.name);
        for o in container_children(&cnode, content) {
            out.push(SearchEntry {
                label: o.name,
                location: location.clone(),
                kind: Some(o.kind),
                target: SearchTarget::Object(o.node),
            });
        }
    }
    out
}

/// Every cached object (containers, their objects and table columns).
pub fn snapshot(
    ds: &DataSourceId,
    d: Dialect,
    catalog: &Catalog,
    filter: &Filter,
) -> Vec<DbObject> {
    let list = catalog.containers.as_deref().unwrap_or_default();
    let mut out = containers(ds, d, list, filter);
    let ck = container_kind(d);
    let root = NodeRef::root(ds.clone());
    for (c, content) in catalog.loaded() {
        if !filter.matches(&c.name) {
            continue;
        }
        let cnode = root.child(ck, &c.name);
        for o in container_children(&cnode, content) {
            out.push(o);
        }
        for t in &content.tables {
            let tnode = cnode.child(table_kind(t), &t.name);
            out.extend(
                table_children(&tnode, t)
                    .into_iter()
                    .filter(|o| o.kind == ObjectKind::Column),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        let f = Filter::parse("public, Sales_*, !sales_tmp");
        assert!(f.matches("public"));
        assert!(f.matches("sales_eu"));
        assert!(!f.matches("sales_tmp"));
        assert!(!f.matches("other"));
        assert!(Filter::parse("").matches("anything"));
        assert!(!Filter::parse("!pg_*").matches("pg_catalog"));
    }
}
