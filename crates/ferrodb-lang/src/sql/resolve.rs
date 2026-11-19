//! Name resolution: what every name of a parsed statement refers to.
//!
//! Tables resolve to CTEs, tables created earlier in the script, then the
//! catalog along the search path. Columns resolve through the sources of
//! the scope chain (subqueries see the sources of the queries around them,
//! CTE bodies do not). Each part of a qualified name gets a [`Symbol`], so
//! "Go to declaration", rename and hover work on any part.

use std::{collections::HashMap, ops::Range};

use super::{
    dialect::{Dialect, Folding},
    schema::{SchemaModel, TableId},
    syntax::{Ident, NameRef, Parsed, Role, SourceKind, names_eq},
};

/// A table created by an earlier statement of the script.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalTable {
    pub schema: Option<String>,
    pub name: String,
    pub columns: Vec<String>,
    pub view: bool,
    /// Range of the name in the document.
    pub decl: Range<usize>,
}

/// What a name means. Local symbols (CTEs, aliases, output columns) are
/// relative to their statement.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Symbol {
    Schema(String),
    Table {
        schema: Option<String>,
        name: String,
    },
    Column {
        schema: Option<String>,
        table: String,
        column: String,
    },
    Cte {
        scope: usize,
        idx: usize,
    },
    /// Explicit column of a CTE (`WITH x(a, b) AS …`).
    CteColumn {
        scope: usize,
        idx: usize,
        col: usize,
    },
    Alias {
        scope: usize,
        source: usize,
    },
    /// Select-list item of a scope (aliased or computed column).
    Output {
        scope: usize,
        idx: usize,
    },
    Function(String),
}

impl Symbol {
    /// Database objects are shared by all statements of a script.
    pub fn is_global(&self) -> bool {
        matches!(
            self,
            Self::Schema(_) | Self::Table { .. } | Self::Column { .. } | Self::Function(_)
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// Cannot be checked (catalog not loaded, unknown source columns…).
    Unknown,
    /// Part `part` does not exist.
    Unresolved {
        part: usize,
        what: What,
        /// Close names for "did you mean".
        suggestions: Vec<String>,
    },
    /// The unqualified column exists in several sources.
    Ambiguous(Vec<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Schema,
    Table,
    Column,
    /// Qualifier that names no source of the query.
    Qualifier,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Resolution {
    pub parts: Vec<Option<Symbol>>,
    pub status: Status,
    /// Data type of a resolved column.
    pub data_type: Option<String>,
}

/// A column offered by a source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceColumn {
    pub name: String,
    pub symbol: Symbol,
    pub data_type: Option<String>,
}

/// Resolution environment: dialect, catalog, search order, script tables.
pub struct Env<'a> {
    pub d: &'static Dialect,
    pub model: &'a SchemaModel,
    pub order: Vec<usize>,
    pub locals: Vec<LocalTable>,
}

/// What a table name resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableTarget {
    Db(TableId),
    Local(usize),
    Cte { scope: usize, idx: usize },
}

const PSEUDO_COLUMNS: &[&str] = &[
    "ctid", "oid", "xmin", "xmax", "cmin", "cmax", "tableoid", "rowid", "_rowid_",
];

impl<'a> Env<'a> {
    pub fn new(d: &'static Dialect, model: &'a SchemaModel, current: Option<&str>) -> Self {
        Self {
            d,
            model,
            order: model.search_order(current.or(d.default_schema.as_deref())),
            locals: Vec::new(),
        }
    }

    fn eq(&self, stored: &str, wanted: &str) -> bool {
        names_eq(stored, wanted, self.d)
    }

    /// Resolves a table name (`parts` = [schema.]table) outside of CTEs.
    /// `Err(true)` = checkable and missing, `Err(false)` = unknown.
    pub fn table(&self, parts: &[Ident]) -> Result<(TableTarget, bool), bool> {
        let (schema, name) = match parts {
            [t] => (None, t),
            [.., s, t] => (Some(s.name(self.d)), t),
            [] => return Err(false),
        };
        let n = name.name(self.d);
        if schema.is_none()
            && let Some(i) = self.locals.iter().rposition(|l| self.eq(&l.name, &n))
        {
            return Ok((TableTarget::Local(i), true));
        }
        if let Some(s) = &schema
            && let Some(i) = self.locals.iter().rposition(|l| {
                self.eq(&l.name, &n) && l.schema.as_deref().is_some_and(|x| self.eq(x, s))
            })
        {
            return Ok((TableTarget::Local(i), true));
        }
        let exact = self
            .model
            .find_tables(schema.as_deref(), &n, true, &self.order);
        if let Some(id) = exact.first() {
            return Ok((TableTarget::Db(*id), true));
        }
        let loose = self
            .model
            .find_tables(schema.as_deref(), &n, false, &self.order);
        if let Some(id) = loose.first() {
            // Same name in another case: fine where lookups ignore case.
            let fine = self.d.folding == Folding::None;
            return Ok((TableTarget::Db(*id), fine));
        }
        let checkable = match &schema {
            None => self.model.search_path_loaded(&self.order),
            Some(s) => self
                .model
                .schema(s)
                .is_some_and(|i| self.model.schemas[i].loaded),
        };
        Err(checkable)
    }

    pub fn table_symbol(&self, t: TableTarget) -> Option<Symbol> {
        match t {
            TableTarget::Db(id) => Some(Symbol::Table {
                schema: Some(self.model.schema_name(id).to_owned()),
                name: self.model.table(id).name.clone(),
            }),
            TableTarget::Local(i) => Some(Symbol::Table {
                schema: self.locals[i].schema.clone(),
                name: self.locals[i].name.clone(),
            }),
            TableTarget::Cte { scope, idx } => Some(Symbol::Cte { scope, idx }),
        }
    }

    /// Table names close to `name` (typos, other case).
    pub fn similar_tables(&self, name: &str) -> Vec<String> {
        let mut v: Vec<(usize, String)> = self
            .model
            .all_tables(&self.order)
            .map(|id| self.model.table(id).name.clone())
            .chain(self.locals.iter().map(|l| l.name.clone()))
            .filter_map(|t| close(name, &t).map(|d| (d, t)))
            .collect();
        v.sort();
        v.dedup_by(|a, b| a.1 == b.1);
        v.into_iter().take(3).map(|(_, t)| t).collect()
    }
}

/// Edit distance when small enough to be a typo (case differences count 0).
pub fn close(a: &str, b: &str) -> Option<usize> {
    let a = a.to_lowercase();
    let b = b.to_lowercase();
    if a == b {
        return Some(0);
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let max = match a.len().max(b.len()) {
        0..=3 => 1,
        4..=8 => 2,
        _ => 3,
    };
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for i in 1..=a.len() {
        let mut cur = vec![i; b.len() + 1];
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        prev = cur;
    }
    (prev[b.len()] <= max).then_some(prev[b.len()])
}

/// Resolution of every reference of a statement.
pub struct Resolved<'p> {
    pub parsed: &'p Parsed,
    pub refs: Vec<Resolution>,
    /// Resolved table of every (scope, source).
    pub sources: HashMap<(usize, usize), Option<TableTarget>>,
}

pub fn resolve<'p>(parsed: &'p Parsed, env: &Env<'_>) -> Resolved<'p> {
    let mut r = Resolver {
        p: parsed,
        env,
        sources: HashMap::new(),
    };
    // Table sources first: columns depend on them.
    for (si, s) in parsed.scopes.iter().enumerate() {
        for (i, src) in s.sources.iter().enumerate() {
            let t = match &src.kind {
                SourceKind::Table(parts) => r.table_target(si, parts),
                _ => None,
            };
            r.sources.insert((si, i), t);
        }
    }
    let refs = parsed.refs.iter().map(|x| r.resolve_ref(x)).collect();
    Resolved {
        parsed,
        refs,
        sources: r.sources,
    }
}

struct Resolver<'p, 'e> {
    p: &'p Parsed,
    env: &'e Env<'e>,
    sources: HashMap<(usize, usize), Option<TableTarget>>,
}

impl Resolver<'_, '_> {
    fn d(&self) -> &'static Dialect {
        self.env.d
    }

    fn table_target(&self, scope: usize, parts: &[Ident]) -> Option<TableTarget> {
        if parts.len() == 1
            && let Some((s, idx)) = self.p.find_cte(scope, &parts[0].name(self.d()), self.d())
        {
            return Some(TableTarget::Cte { scope: s, idx });
        }
        self.env.table(parts).ok().map(|(t, _)| t)
    }

    fn resolve_ref(&self, r: &NameRef) -> Resolution {
        match r.role {
            Role::Table { .. } | Role::DdlTable => self.resolve_table(r),
            Role::Function => Resolution {
                parts: r
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        (i + 1 == r.parts.len()).then(|| Symbol::Function(p.name(self.d())))
                    })
                    .collect(),
                status: Status::Unknown,
                data_type: None,
            },
            Role::Column | Role::SortColumn | Role::TargetColumn | Role::Star => {
                self.resolve_column(r)
            }
        }
    }

    fn resolve_table(&self, r: &NameRef) -> Resolution {
        let n = r.parts.len();
        let mut parts = vec![None; n];
        if n == 1
            && let Some((s, idx)) = self
                .p
                .find_cte(r.scope, &r.parts[0].name(self.d()), self.d())
        {
            parts[0] = Some(Symbol::Cte { scope: s, idx });
            return ok(parts, None);
        }
        if n >= 2 {
            let s = r.parts[n - 2].name(self.d());
            if let Some(i) = self.env.model.schema(&s) {
                parts[n - 2] = Some(Symbol::Schema(self.env.model.schemas[i].name.clone()));
            }
        }
        let ddl = r.role == Role::DdlTable;
        match self.env.table(&r.parts) {
            Ok((t, exact)) => {
                parts[n - 1] = self.env.table_symbol(t);
                if exact || ddl {
                    ok(parts, None)
                } else {
                    let real = match parts[n - 1] {
                        Some(Symbol::Table { ref name, .. }) => name.clone(),
                        _ => String::new(),
                    };
                    Resolution {
                        parts,
                        status: Status::Unresolved {
                            part: n - 1,
                            what: What::Table,
                            suggestions: vec![self.d().quote(&real)],
                        },
                        data_type: None,
                    }
                }
            }
            Err(true) if !ddl => {
                // A missing schema is reported on the schema.
                if n >= 2 && parts[n - 2].is_none() && !self.env.model.is_empty() {
                    return Resolution {
                        parts,
                        status: Status::Unresolved {
                            part: n - 2,
                            what: What::Schema,
                            suggestions: Vec::new(),
                        },
                        data_type: None,
                    };
                }
                let name = r.parts[n - 1].name(self.d());
                Resolution {
                    parts,
                    status: Status::Unresolved {
                        part: n - 1,
                        what: What::Table,
                        suggestions: self
                            .env
                            .similar_tables(&name)
                            .into_iter()
                            .map(|t| self.d().quote_if_needed(&t))
                            .collect(),
                    },
                    data_type: None,
                }
            }
            Err(_) => Resolution {
                parts,
                status: Status::Unknown,
                data_type: None,
            },
        }
    }

    /// Columns of source `src` of `scope`; `None` = unknown.
    pub fn source_columns(
        &self,
        scope: usize,
        src: usize,
        depth: usize,
    ) -> Option<Vec<SourceColumn>> {
        source_columns(self.p, self.env, &self.sources, scope, src, depth)
    }

    fn resolve_column(&self, r: &NameRef) -> Resolution {
        let n = r.parts.len();
        let mut parts: Vec<Option<Symbol>> = vec![None; n];
        let col = &r.parts[n - 1];
        let col_name = col.name(self.d());
        let chain = self.p.chain(r.scope);

        // Target columns: only the statement target.
        if r.role == Role::TargetColumn {
            let Some(ti) = self.p.scopes[r.scope].sources.iter().position(|s| s.target) else {
                return unknown(parts);
            };
            return self.column_in(r.scope, ti, col, &mut parts, n - 1);
        }
        if r.role == Role::Star && n == 1 && self.p.scopes[r.scope].sources.is_empty() {
            return unknown(parts);
        }

        let qual = &r.parts[..n - if r.role == Role::Star { 0 } else { 1 }];
        if !qual.is_empty() {
            // Find the source named by the qualifier.
            let qn = qual.last().expect("non-empty").name(self.d());
            for &(s, visible) in &chain {
                if !visible {
                    continue;
                }
                for (i, src) in self.p.scopes[s].sources.iter().enumerate() {
                    let hit = match (&src.alias, &src.kind) {
                        (Some(a), _) => qual.len() == 1 && self.d().is_same(&a.name(self.d()), &qn),
                        (None, SourceKind::Table(tp)) => {
                            let tn = tp.last().map(|x| x.name(self.d())).unwrap_or_default();
                            self.d().is_same(&tn, &qn)
                                && (qual.len() == 1
                                    || tp.len() >= 2
                                        && self.d().is_same(
                                            &tp[tp.len() - 2].name(self.d()),
                                            &qual[qual.len() - 2].name(self.d()),
                                        ))
                        }
                        (None, SourceKind::Function(f)) => {
                            qual.len() == 1 && self.d().is_same(&f.name(self.d()), &qn)
                        }
                        _ => false,
                    };
                    if !hit {
                        continue;
                    }
                    let qi = qual.len() - 1;
                    parts[qi] = if src.alias.is_some() {
                        Some(Symbol::Alias {
                            scope: s,
                            source: i,
                        })
                    } else {
                        self.sources
                            .get(&(s, i))
                            .copied()
                            .flatten()
                            .and_then(|t| self.env.table_symbol(t))
                    };
                    if r.role == Role::Star {
                        return ok(parts, None);
                    }
                    return self.column_in(s, i, col, &mut parts, n - 1);
                }
            }
            // `excluded.x` (ON CONFLICT) and the like are sources; anything
            // else names nothing in the query.
            let has_sources = chain
                .iter()
                .any(|&(s, v)| v && !self.p.scopes[s].sources.is_empty());
            if !has_sources {
                return unknown(parts);
            }
            let suggestions = chain
                .iter()
                .filter(|(_, v)| *v)
                .flat_map(|&(s, _)| self.p.scopes[s].sources.iter())
                .filter_map(|src| src.exposed().map(|e| e.text.clone()))
                .filter(|e| close(&qn, e).is_some())
                .collect();
            return Resolution {
                parts,
                status: Status::Unresolved {
                    part: qual.len() - 1,
                    what: What::Qualifier,
                    suggestions,
                },
                data_type: None,
            };
        }

        // Unqualified: innermost scope with a match wins.
        if PSEUDO_COLUMNS
            .iter()
            .any(|p| col.text.eq_ignore_ascii_case(p))
        {
            return unknown(parts);
        }
        let mut all_known = true;
        let mut any_source = false;
        for &(s, visible) in &chain {
            if !visible {
                continue;
            }
            let mut hits: Vec<(usize, SourceColumn)> = Vec::new();
            for i in 0..self.p.scopes[s].sources.len() {
                // Pseudo sources (`excluded`) are only reachable qualified.
                if self.p.scopes[s].sources[i].range.is_empty() {
                    continue;
                }
                any_source = true;
                match self.source_columns(s, i, 0) {
                    Some(cols) => {
                        if let Some(c) = pick(&cols, &col_name, self.d()) {
                            hits.push((i, c.clone()));
                        }
                    }
                    None => all_known = false,
                }
            }
            if hits.len() == 1 {
                let (_, c) = hits.remove(0);
                parts[n - 1] = Some(c.symbol);
                return ok(parts, c.data_type);
            }
            if hits.len() > 1 {
                // `USING (id)` / natural joins expose one column: same
                // symbol everywhere is not ambiguous.
                parts[n - 1] = Some(hits[0].1.symbol.clone());
                let names: Vec<String> = hits
                    .iter()
                    .filter_map(|(i, _)| {
                        self.p.scopes[s].sources[*i]
                            .exposed()
                            .map(|e| e.text.clone())
                    })
                    .collect();
                return Resolution {
                    parts,
                    status: Status::Ambiguous(names),
                    data_type: None,
                };
            }
            // Select-list aliases in ORDER BY / GROUP BY / HAVING.
            if r.role == Role::SortColumn
                && let Some(idx) = self.p.scopes[s].outputs.iter().position(|o| {
                    o.alias
                        .as_ref()
                        .is_some_and(|a| self.d().is_same(&a.name(self.d()), &col_name))
                })
            {
                parts[n - 1] = Some(Symbol::Output { scope: s, idx });
                return ok(parts, None);
            }
            // A whole-row reference to a source (`SELECT o FROM orders o`).
            if let Some(i) = self.p.scopes[s].sources.iter().position(|src| {
                src.exposed()
                    .is_some_and(|e| self.d().is_same(&e.name(self.d()), &col_name))
            }) {
                parts[n - 1] = Some(Symbol::Alias {
                    scope: s,
                    source: i,
                });
                return ok(parts, None);
            }
        }
        if !any_source
            || !all_known
            || self.d().is_soft_keyword(&col.text)
            || self.d().is_type(&col.text)
        {
            return unknown(parts);
        }
        let suggestions = chain
            .iter()
            .filter(|(_, v)| *v)
            .flat_map(|&(s, _)| {
                (0..self.p.scopes[s].sources.len())
                    .filter_map(move |i| self.source_columns(s, i, 0))
                    .flatten()
            })
            .filter(|c| close(&col_name, &c.name).is_some())
            .map(|c| self.d().quote_if_needed(&c.name))
            .collect();
        Resolution {
            parts,
            status: Status::Unresolved {
                part: n - 1,
                what: What::Column,
                suggestions,
            },
            data_type: None,
        }
    }

    fn column_in(
        &self,
        scope: usize,
        src: usize,
        col: &Ident,
        parts: &mut Vec<Option<Symbol>>,
        at: usize,
    ) -> Resolution {
        let parts_out = std::mem::take(parts);
        let mut parts = parts_out;
        let Some(cols) = self.source_columns(scope, src, 0) else {
            return unknown(parts);
        };
        let name = col.name(self.d());
        match pick(&cols, &name, self.d()) {
            Some(c) => {
                parts[at] = Some(c.symbol.clone());
                ok(parts, c.data_type.clone())
            }
            None if PSEUDO_COLUMNS
                .iter()
                .any(|p| col.text.eq_ignore_ascii_case(p)) =>
            {
                unknown(parts)
            }
            None => {
                let suggestions = cols
                    .iter()
                    .filter(|c| close(&name, &c.name).is_some())
                    .map(|c| self.d().quote_if_needed(&c.name))
                    .collect();
                Resolution {
                    parts,
                    status: Status::Unresolved {
                        part: at,
                        what: What::Column,
                        suggestions,
                    },
                    data_type: None,
                }
            }
        }
    }
}

impl Dialect {
    /// Same identifier under the dialect's lookup rules.
    pub fn is_same(&self, a: &str, b: &str) -> bool {
        names_eq(a, b, self)
    }
}

/// Exact match first, then case-insensitive where folding allows it.
fn pick<'c>(cols: &'c [SourceColumn], name: &str, d: &Dialect) -> Option<&'c SourceColumn> {
    cols.iter().find(|c| c.name == name).or_else(|| {
        (d.folding == Folding::None)
            .then(|| cols.iter().find(|c| c.name.eq_ignore_ascii_case(name)))
            .flatten()
    })
}

fn ok(parts: Vec<Option<Symbol>>, data_type: Option<String>) -> Resolution {
    Resolution {
        parts,
        status: Status::Ok,
        data_type,
    }
}

fn unknown(parts: Vec<Option<Symbol>>) -> Resolution {
    Resolution {
        parts,
        status: Status::Unknown,
        data_type: None,
    }
}

/// Columns of source `src` of `scope`; `None` = unknown.
pub fn source_columns(
    p: &Parsed,
    env: &Env<'_>,
    sources: &HashMap<(usize, usize), Option<TableTarget>>,
    scope: usize,
    src: usize,
    depth: usize,
) -> Option<Vec<SourceColumn>> {
    if depth > 8 {
        return None;
    }
    let source = &p.scopes[scope].sources[src];
    let mut cols = match &source.kind {
        SourceKind::Table(_) => match sources.get(&(scope, src)).copied().flatten()? {
            TableTarget::Db(id) => {
                let t = env.model.table(id);
                let schema = Some(env.model.schema_name(id).to_owned());
                t.columns
                    .iter()
                    .map(|c| SourceColumn {
                        name: c.name.clone(),
                        symbol: Symbol::Column {
                            schema: schema.clone(),
                            table: t.name.clone(),
                            column: c.name.clone(),
                        },
                        data_type: Some(c.data_type.clone()),
                    })
                    .collect()
            }
            TableTarget::Local(i) => {
                let l = &env.locals[i];
                l.columns
                    .iter()
                    .map(|c| SourceColumn {
                        name: c.clone(),
                        symbol: Symbol::Column {
                            schema: l.schema.clone(),
                            table: l.name.clone(),
                            column: c.clone(),
                        },
                        data_type: None,
                    })
                    .collect()
            }
            TableTarget::Cte { scope: cs, idx } => {
                let cte = &p.scopes[cs].ctes[idx];
                if cte.columns.is_empty() {
                    scope_outputs(p, env, sources, cte.scope, depth + 1)?
                } else {
                    cte.columns
                        .iter()
                        .enumerate()
                        .map(|(col, c)| SourceColumn {
                            name: c.name(env.d),
                            symbol: Symbol::CteColumn {
                                scope: cs,
                                idx,
                                col,
                            },
                            data_type: None,
                        })
                        .collect()
                }
            }
        },
        SourceKind::Subquery(s) => scope_outputs(p, env, sources, *s, depth + 1)?,
        SourceKind::Function(_) => {
            if source.column_aliases.is_empty() {
                return None;
            }
            Vec::new()
        }
    };
    // `AS x(a, b)` renames the first columns.
    for (i, a) in source.column_aliases.iter().enumerate() {
        let c = SourceColumn {
            name: a.name(env.d),
            symbol: Symbol::Alias { scope, source: src },
            data_type: None,
        };
        match cols.get_mut(i) {
            Some(x) => x.name = c.name,
            None => cols.push(c),
        }
    }
    Some(cols)
}

/// Output columns of a query scope; `None` when a `*` cannot be expanded.
fn scope_outputs(
    p: &Parsed,
    env: &Env<'_>,
    sources: &HashMap<(usize, usize), Option<TableTarget>>,
    scope: usize,
    depth: usize,
) -> Option<Vec<SourceColumn>> {
    let s = &p.scopes[scope];
    let mut out = Vec::new();
    for (idx, o) in s.outputs.iter().enumerate() {
        if let Some(star) = &o.star {
            for (i, src) in s.sources.iter().enumerate() {
                let take = match star {
                    None => true,
                    Some(q) => src
                        .exposed()
                        .is_some_and(|e| env.d.is_same(&e.name(env.d), &q.name(env.d))),
                };
                if take {
                    out.extend(source_columns(p, env, sources, scope, i, depth + 1)?);
                }
            }
            continue;
        }
        if let Some(a) = &o.alias {
            out.push(SourceColumn {
                name: a.name(env.d),
                symbol: Symbol::Output { scope, idx },
                data_type: None,
            });
        } else if let Some(r) = o.column_ref {
            let id = p.refs[r].parts.last().expect("non-empty");
            out.push(SourceColumn {
                name: id.name(env.d),
                symbol: Symbol::Output { scope, idx },
                data_type: None,
            });
        }
        // Unnamed expressions (`count(*)`) get server names: not listed.
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sql::{
        lexer::{Token, tokenize},
        schema::{ColumnInfo, ForeignKeyInfo, SchemaInfo, TableInfo},
        syntax::parse,
    };

    fn col(name: &str, ty: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.into(),
            data_type: ty.into(),
            nullable: true,
            ..Default::default()
        }
    }

    fn fk(cols: &[&str], table: &str, refs: &[&str]) -> ForeignKeyInfo {
        ForeignKeyInfo {
            columns: cols.iter().map(|s| s.to_string()).collect(),
            ref_schema: None,
            ref_table: table.into(),
            ref_columns: refs.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The shop schema used by the SQL engine tests.
    pub(crate) fn shop(schema: &str) -> SchemaModel {
        let t = |name: &str, cols: Vec<ColumnInfo>, fks: Vec<ForeignKeyInfo>| TableInfo {
            name: name.into(),
            view: false,
            columns: cols,
            foreign_keys: fks,
            comment: None,
        };
        let mut id = col("id", "integer");
        id.primary_key = true;
        id.generated = true;
        let tables = vec![
            t(
                "customers",
                vec![
                    id.clone(),
                    col("name", "text"),
                    col("email", "text"),
                    col("created_at", "timestamp"),
                ],
                vec![],
            ),
            t(
                "orders",
                vec![
                    id.clone(),
                    col("customer_id", "integer"),
                    col("total", "numeric"),
                    col("status", "text"),
                    col("created_at", "timestamp"),
                ],
                vec![fk(&["customer_id"], "customers", &["id"])],
            ),
            t(
                "order_items",
                vec![
                    id.clone(),
                    col("order_id", "integer"),
                    col("product_id", "integer"),
                    col("quantity", "integer"),
                    col("price", "numeric"),
                ],
                vec![
                    fk(&["order_id"], "orders", &["id"]),
                    fk(&["product_id"], "products", &["id"]),
                ],
            ),
            t(
                "products",
                vec![
                    id.clone(),
                    col("name", "text"),
                    col("price", "numeric"),
                    col("category_id", "integer"),
                ],
                vec![fk(&["category_id"], "categories", &["id"])],
            ),
            t("categories", vec![id, col("title", "text")], vec![]),
        ];
        SchemaModel::new(
            vec![
                SchemaInfo {
                    name: schema.into(),
                    loaded: true,
                    tables,
                    routines: vec![],
                },
                SchemaInfo {
                    name: "audit".into(),
                    loaded: true,
                    tables: vec![t(
                        "log",
                        vec![col("id", "integer"), col("message", "text")],
                        vec![],
                    )],
                    routines: vec![],
                },
            ],
            vec![schema.into()],
        )
    }

    pub(crate) fn run(sql: &str, dialect: &str, model: &SchemaModel) -> (Parsed, Vec<Resolution>) {
        let d = Dialect::get(dialect);
        let toks: Vec<Token> = tokenize(sql, &d.flavor)
            .into_iter()
            .filter(|t| !t.is_trivia())
            .collect();
        let p = parse(sql, &toks, d);
        let env = Env::new(d, model, None);
        let r = resolve(&p, &env).refs;
        (p, r)
    }

    fn status_of<'a>(sql: &str, p: &Parsed, r: &'a [Resolution], text: &str) -> &'a Status {
        let i = p
            .refs
            .iter()
            .position(|x| &sql[x.range()] == text)
            .unwrap_or_else(|| panic!("no ref {text}"));
        &r[i].status
    }

    #[test]
    fn aliases_and_columns_resolve() {
        let m = shop("public");
        let sql =
            "select o.id, c.name, total from orders o join customers c on c.id = o.customer_id";
        let (p, r) = run(sql, "postgresql", &m);
        for x in &r {
            assert_eq!(x.status, Status::Ok, "{:?}", x);
        }
        let i = p
            .refs
            .iter()
            .position(|x| &sql[x.range()] == "o.id")
            .unwrap();
        assert_eq!(
            r[i].parts[0],
            Some(Symbol::Alias {
                scope: 0,
                source: 0
            })
        );
        assert_eq!(
            r[i].parts[1],
            Some(Symbol::Column {
                schema: Some("public".into()),
                table: "orders".into(),
                column: "id".into()
            })
        );
        assert_eq!(r[i].data_type.as_deref(), Some("integer"));
    }

    #[test]
    fn errors_are_found() {
        let m = shop("public");
        let sql = "select id, x.name, totl from orders o join customers c on c.id = o.customer_id join nope n on true";
        let (p, r) = run(sql, "postgresql", &m);
        assert!(matches!(status_of(sql, &p, &r, "id"), Status::Ambiguous(_)));
        assert!(matches!(
            status_of(sql, &p, &r, "x.name"),
            Status::Unresolved {
                what: What::Qualifier,
                ..
            }
        ));
        // `nope` is unknown, so unqualified columns cannot be checked.
        assert_eq!(status_of(sql, &p, &r, "totl"), &Status::Unknown);
        assert!(matches!(
            status_of(sql, &p, &r, "nope"),
            Status::Unresolved {
                what: What::Table,
                ..
            }
        ));
        let sql = "select totl from orders";
        let (p, r) = run(sql, "postgresql", &m);
        match status_of(sql, &p, &r, "totl") {
            Status::Unresolved { suggestions, .. } => {
                assert_eq!(suggestions, &vec!["total".to_owned()])
            }
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn ctes_subqueries_and_order_by_aliases() {
        let m = shop("public");
        let sql = "with big(cid, s) as (select customer_id, sum(total) from orders group by customer_id) \
                   select b.cid, x.n, s from big b join (select id, name n from customers) x on x.id = b.cid order by s desc";
        let (_, r) = run(sql, "postgresql", &m);
        for x in &r {
            assert!(matches!(x.status, Status::Ok | Status::Unknown), "{x:?}");
            assert!(!matches!(x.status, Status::Unresolved { .. }));
        }
        let sql = "select total * 2 as dbl from orders order by dbl";
        let (p, r) = run(sql, "postgresql", &m);
        assert_eq!(status_of(sql, &p, &r, "dbl"), &Status::Ok);
    }

    #[test]
    fn case_rules_follow_the_dialect() {
        let m = shop("public");
        // PostgreSQL folds `ORDERS` to `orders`.
        let (_, r) = run("select ID from ORDERS", "postgresql", &m);
        assert!(r.iter().all(|x| x.status == Status::Ok));
        // A quoted name keeps its case: "Orders" does not exist.
        let (_, r) = run("select 1 from \"Orders\"", "postgresql", &m);
        assert!(matches!(r[0].status, Status::Unresolved { .. }));
        let m = shop("shop");
        let (_, r) = run("select ID from Orders", "mysql", &m);
        assert!(r.iter().all(|x| x.status == Status::Ok), "{r:?}");
    }

    #[test]
    fn insert_update_targets() {
        let m = shop("public");
        let sql = "insert into orders (customer_id, totl) values (1, 2)";
        let (p, r) = run(sql, "postgresql", &m);
        assert_eq!(status_of(sql, &p, &r, "customer_id"), &Status::Ok);
        assert!(matches!(
            status_of(sql, &p, &r, "totl"),
            Status::Unresolved { .. }
        ));
        let sql = "update orders set status = 'x' where id = 1";
        let (_, r) = run(sql, "postgresql", &m);
        assert!(r.iter().all(|x| x.status == Status::Ok), "{r:?}");
    }

    #[test]
    fn typo_distance() {
        assert_eq!(close("totl", "total"), Some(1));
        assert_eq!(close("Orders", "orders"), Some(0));
        assert_eq!(close("abc", "xyz"), None);
    }
}
