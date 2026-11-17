//! What the SQL engine knows about a database: schemas, tables with
//! columns and foreign keys, routines. Built by the backend from its
//! metadata catalog (or by tests); indexed once for fast lookups, so a
//! catalog of 10k tables stays cheap on every keystroke.

use std::collections::HashMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
    pub primary_key: bool,
    /// Generated, identity, serial or auto-increment: filled by the server.
    pub generated: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ForeignKeyInfo {
    pub columns: Vec<String>,
    /// `None` = same schema.
    pub ref_schema: Option<String>,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub view: bool,
    pub columns: Vec<ColumnInfo>,
    pub foreign_keys: Vec<ForeignKeyInfo>,
    pub comment: Option<String>,
}

impl TableInfo {
    pub fn column(&self, name: &str) -> Option<&ColumnInfo> {
        self.columns.iter().find(|c| c.name == name)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutineInfo {
    pub name: String,
    pub procedure: bool,
    /// (name, type) of the input arguments.
    pub arguments: Vec<(Option<String>, String)>,
    pub returns: Option<String>,
    pub comment: Option<String>,
}

impl RoutineInfo {
    /// `name(a int, b text) -> type`, the format of dialect signatures.
    pub fn signature(&self) -> String {
        let args: Vec<String> = self
            .arguments
            .iter()
            .map(|(n, t)| match n {
                Some(n) => format!("{n} {t}"),
                None => t.clone(),
            })
            .collect();
        match &self.returns {
            Some(r) => format!("{}({}) -> {r}", self.name, args.join(", ")),
            None => format!("{}({})", self.name, args.join(", ")),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SchemaInfo {
    pub name: String,
    /// Content introspected: missing names in it are errors. A schema that
    /// is only listed gives no "unresolved" diagnostics.
    pub loaded: bool,
    pub tables: Vec<TableInfo>,
    pub routines: Vec<RoutineInfo>,
}

/// A table of the model: (schema index, table index).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TableId {
    pub schema: usize,
    pub table: usize,
}

#[derive(Debug, Default)]
pub struct SchemaModel {
    pub schemas: Vec<SchemaInfo>,
    /// Schemas searched for unqualified names, in order.
    pub search_path: Vec<String>,
    /// Lower-cased table name → tables.
    by_name: HashMap<String, Vec<TableId>>,
    /// Lower-cased (schema, table) → (referencing table, foreign key index).
    referenced_by: HashMap<(String, String), Vec<(TableId, usize)>>,
}

impl SchemaModel {
    pub fn new(schemas: Vec<SchemaInfo>, search_path: Vec<String>) -> Self {
        let mut m = Self {
            schemas,
            search_path,
            ..Default::default()
        };
        for (si, s) in m.schemas.iter().enumerate() {
            for (ti, t) in s.tables.iter().enumerate() {
                let id = TableId {
                    schema: si,
                    table: ti,
                };
                m.by_name
                    .entry(t.name.to_lowercase())
                    .or_default()
                    .push(id);
                for (fi, fk) in t.foreign_keys.iter().enumerate() {
                    let rs = fk.ref_schema.as_deref().unwrap_or(&s.name);
                    m.referenced_by
                        .entry((rs.to_lowercase(), fk.ref_table.to_lowercase()))
                        .or_default()
                        .push((id, fi));
                }
            }
        }
        m
    }

    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty()
    }

    pub fn table(&self, id: TableId) -> &TableInfo {
        &self.schemas[id.schema].tables[id.table]
    }

    pub fn schema_name(&self, id: TableId) -> &str {
        &self.schemas[id.schema].name
    }

    pub fn schema(&self, name: &str) -> Option<usize> {
        self.schemas
            .iter()
            .position(|s| s.name == name)
            .or_else(|| {
                self.schemas
                    .iter()
                    .position(|s| s.name.eq_ignore_ascii_case(name))
            })
    }

    /// Tables named `name` (`exact`: same spelling; else case-insensitive),
    /// in `schema` or along `order` (see [`Self::search_order`]).
    pub fn find_tables(
        &self,
        schema: Option<&str>,
        name: &str,
        exact: bool,
        order: &[usize],
    ) -> Vec<TableId> {
        let Some(cands) = self.by_name.get(&name.to_lowercase()) else {
            return Vec::new();
        };
        let ok = |id: &TableId| !exact || self.table(*id).name == name;
        match schema {
            Some(s) => {
                let Some(si) = self.schema(s) else {
                    return Vec::new();
                };
                cands
                    .iter()
                    .filter(|id| id.schema == si && ok(id))
                    .copied()
                    .collect()
            }
            None => {
                for &s in order {
                    let hits: Vec<TableId> = cands
                        .iter()
                        .filter(|id| id.schema == s && ok(id))
                        .copied()
                        .collect();
                    if !hits.is_empty() {
                        return hits;
                    }
                }
                Vec::new()
            }
        }
    }

    /// Schema indexes for unqualified names: `current` (the console's
    /// schema), then the search path; every schema when both are unknown
    /// (SQLite attachments).
    pub fn search_order(&self, current: Option<&str>) -> Vec<usize> {
        let mut path: Vec<usize> = Vec::new();
        for s in current
            .into_iter()
            .chain(self.search_path.iter().map(String::as_str))
        {
            if let Some(i) = self.schema(s)
                && !path.contains(&i)
            {
                path.push(i);
            }
        }
        if path.is_empty() {
            (0..self.schemas.len()).collect()
        } else {
            path
        }
    }

    /// Unqualified names along `order` can be checked (all loaded).
    pub fn search_path_loaded(&self, order: &[usize]) -> bool {
        !order.is_empty() && order.iter().all(|&s| self.schemas[s].loaded)
    }

    /// Tables with a foreign key to `id`.
    pub fn referencing(&self, id: TableId) -> &[(TableId, usize)] {
        let key = (
            self.schema_name(id).to_lowercase(),
            self.table(id).name.to_lowercase(),
        );
        self.referenced_by.get(&key).map_or(&[], Vec::as_slice)
    }

    /// The table a foreign key of `from` points to.
    pub fn fk_target(&self, from: TableId, fk: &ForeignKeyInfo) -> Option<TableId> {
        let schema = fk
            .ref_schema
            .as_deref()
            .unwrap_or(&self.schemas[from.schema].name);
        let hits = self.find_tables(Some(schema), &fk.ref_table, true, &[]);
        hits.first().copied().or_else(|| {
            self.find_tables(Some(schema), &fk.ref_table, false, &[])
                .first()
                .copied()
        })
    }

    pub fn routines<'a>(
        &'a self,
        name: &'a str,
        order: &'a [usize],
    ) -> impl Iterator<Item = &'a RoutineInfo> + 'a {
        self.schemas
            .iter()
            .enumerate()
            .filter(move |(i, _)| order.contains(i))
            .flat_map(|(_, s)| s.routines.iter())
            .filter(move |r| r.name.eq_ignore_ascii_case(name))
    }

    /// Every table id, `order` first.
    pub fn all_tables(&self, order: &[usize]) -> impl Iterator<Item = TableId> + '_ {
        let order = order.to_vec();
        let rest: Vec<usize> = (0..self.schemas.len())
            .filter(|s| !order.contains(s))
            .collect();
        order.into_iter().chain(rest).flat_map(move |s| {
            (0..self.schemas[s].tables.len()).map(move |t| TableId {
                schema: s,
                table: t,
            })
        })
    }
}
