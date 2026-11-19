//! Context-aware SQL completion.
//!
//! The clause under the cursor decides what is offered: tables after
//! `FROM`/`JOIN`/`INTO`/`UPDATE` (whole joins on foreign keys after
//! `JOIN`, join conditions after `ON`), the target's columns in
//! `INSERT (…)` and `SET`, columns of the sources in scope in expressions,
//! columns of one source after `alias.`, keywords and functions
//! elsewhere. Candidates are ranked by [`matcher::score`] (prefix,
//! abbreviation, layout tolerance) plus a priority by kind.

use std::ops::Range;

use ferrodb_api::{CompletionItem, CompletionKind, LiveTemplate};

use super::{
    document::Document,
    lexer::{Token, TokenKind},
    matcher,
    resolve::{SourceColumn, TableTarget},
    schema::{TableId, TableInfo},
    syntax::{Clause, Ident, Parsed, Source, SourceKind, unquote},
    templates,
};

struct Cand {
    label: String,
    /// Text matched against the prefix (defaults to the label).
    filter: Option<String>,
    insert: String,
    kind: CompletionKind,
    detail: Option<String>,
    prio: i32,
}

const MAX_ITEMS: usize = 200;

const STATEMENT_START: &[&str] = &[
    "SELECT",
    "INSERT INTO",
    "UPDATE",
    "DELETE FROM",
    "WITH",
    "CREATE TABLE",
    "CREATE VIEW",
    "CREATE INDEX",
    "ALTER TABLE",
    "DROP TABLE",
    "TRUNCATE",
    "EXPLAIN",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
];

const AFTER_SOURCE: &[&str] = &[
    "WHERE",
    "JOIN",
    "LEFT JOIN",
    "INNER JOIN",
    "RIGHT JOIN",
    "FULL JOIN",
    "CROSS JOIN",
    "ON",
    "USING",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "UNION",
    "UNION ALL",
    "AS",
];

const IN_EXPRESSION: &[&str] = &[
    "AND",
    "OR",
    "NOT",
    "IN",
    "IS NULL",
    "IS NOT NULL",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "AS",
    "DISTINCT",
    "NULL",
    "TRUE",
    "FALSE",
    "FROM",
    "WHERE",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "ASC",
    "DESC",
    "JOIN",
    "LEFT JOIN",
    "UNION",
];

/// Completion items at `offset`.
pub fn complete(
    doc: &Document<'_>,
    offset: usize,
    templates: &[LiveTemplate],
) -> Vec<CompletionItem> {
    let offset = offset.min(doc.text.len());
    // Nothing inside strings and comments.
    if let Some(t) = doc.token_at(offset)
        && t.range.start < offset
        && (matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment)
            || (t.kind == TokenKind::String
                && (offset < t.range.end || !closed_string(doc.text, t))))
    {
        return Vec::new();
    }
    let (replace, prefix) = match doc.word_at(offset) {
        Some(t) if t.range.start < offset => {
            let raw = &doc.text[t.range.start..offset];
            let p = if t.kind == TokenKind::QuotedIdent {
                unquote(raw)
            } else {
                raw.to_owned()
            };
            (t.range.start..offset, p)
        }
        _ => (offset..offset, String::new()),
    };
    let mut c = Ctx {
        doc,
        stmt: doc.statement_at(replace.start),
        replace: replace.clone(),
        cands: Vec::new(),
    };
    c.collect(templates);
    rank(c.cands, &prefix, replace)
}

fn closed_string(src: &str, t: &Token) -> bool {
    let s = t.text(src);
    s.len() >= 2 && s.ends_with('\'')
}

fn rank(cands: Vec<Cand>, prefix: &str, replace: Range<usize>) -> Vec<CompletionItem> {
    let mut scored: Vec<(i32, Cand)> = cands
        .into_iter()
        .filter_map(|c| {
            let s = matcher::score(c.filter.as_deref().unwrap_or(&c.label), prefix)?;
            Some((s + c.prio, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.label.cmp(&b.1.label)));
    let mut seen = std::collections::HashSet::new();
    scored
        .into_iter()
        .filter(|(_, c)| seen.insert((c.label.clone(), c.insert.clone())))
        .take(MAX_ITEMS)
        .map(|(_, c)| CompletionItem {
            label: c.label,
            kind: c.kind,
            insert_text: c.insert,
            detail: c.detail,
            replace: replace.clone(),
        })
        .collect()
}

struct Ctx<'d, 'a> {
    doc: &'d Document<'a>,
    stmt: Option<usize>,
    replace: Range<usize>,
    cands: Vec<Cand>,
}

impl Ctx<'_, '_> {
    fn parsed(&self) -> Option<&Parsed> {
        self.stmt.map(|s| &self.doc.stmts[s].parsed)
    }

    fn toks(&self) -> &[Token] {
        self.stmt.map_or(&[], |s| &self.doc.stmts[s].toks)
    }

    /// Significant tokens before the word being completed.
    fn before(&self) -> &[Token] {
        let toks = self.toks();
        let n = toks.partition_point(|t| t.range.end <= self.replace.start);
        &toks[..n]
    }

    fn word(&self, t: &Token) -> Option<String> {
        (t.kind == TokenKind::Word).then(|| t.text(self.doc.text).to_ascii_lowercase())
    }

    fn push(&mut self, c: Cand) {
        self.cands.push(c);
    }

    fn keyword(&mut self, k: &str, prio: i32) {
        self.push(Cand {
            label: k.to_owned(),
            filter: None,
            insert: k.to_owned(),
            kind: CompletionKind::Keyword,
            detail: None,
            prio,
        });
    }

    fn collect(&mut self, templates: &[LiveTemplate]) {
        let before = self.before().to_vec();
        let Some(last) = before.last().cloned() else {
            for k in STATEMENT_START {
                self.keyword(k, 300);
            }
            for t in templates {
                let (text, _) = templates::expand_body(&t.body, None);
                self.push(Cand {
                    label: t.abbreviation.clone(),
                    filter: None,
                    insert: text,
                    kind: CompletionKind::Snippet,
                    detail: Some(t.description.clone()),
                    prio: 250,
                });
            }
            return;
        };
        let lt = last.text(self.doc.text);
        // `qualifier.` → columns of a source, tables of a schema.
        if last.kind == TokenKind::Punct && lt == "." {
            let qual = qualifier(self.doc.text, &before);
            self.after_dot(&qual);
            return;
        }
        let (scope, clause) = self
            .parsed()
            .map_or((0, Clause::None), |p| p.at(self.replace.start));
        let prev = self.word(&last).unwrap_or_default();
        let in_from = matches!(clause, Clause::From | Clause::Join);
        if matches!(
            prev.as_str(),
            "from" | "join" | "update" | "into" | "table" | "lateral" | "only"
        ) || (lt == "," && in_from)
            || (prev == "using"
                && self
                    .parsed()
                    .is_some_and(|p| p.kind == super::syntax::StmtKind::Delete))
        {
            if prev == "join" {
                self.joins(scope);
            }
            self.tables(prev == "into");
            return;
        }
        if clause == Clause::InsertColumns {
            self.insert_columns(scope);
            return;
        }
        if clause == Clause::Set && (prev == "set" || lt == ",") {
            self.target_columns(scope, " = ");
            return;
        }
        if clause == Clause::On && matches!(prev.as_str(), "on" | "and") {
            self.join_conditions(scope);
        }
        let after_source_name = in_from
            && (matches!(last.kind, TokenKind::Word | TokenKind::QuotedIdent) || lt == ")")
            && !matches!(prev.as_str(), "as");
        if after_source_name {
            for k in AFTER_SOURCE {
                self.keyword(k, 300);
            }
            return;
        }
        if matches!(
            clause,
            Clause::Select
                | Clause::Where
                | Clause::On
                | Clause::GroupBy
                | Clause::OrderBy
                | Clause::Having
                | Clause::Set
                | Clause::Values
                | Clause::Returning
                | Clause::Other
                | Clause::Limit
                | Clause::Target
        ) {
            let expect_name = !matches!(
                last.kind,
                TokenKind::Word | TokenKind::QuotedIdent | TokenKind::Number | TokenKind::String
            ) || self.doc.dialect().is_keyword(lt)
                || lt == ")";
            if expect_name && !(lt == ")") {
                self.scope_columns(scope, clause);
                self.functions();
                for k in IN_EXPRESSION {
                    self.keyword(k, 0);
                }
            } else {
                for k in IN_EXPRESSION.iter().chain(AFTER_SOURCE) {
                    self.keyword(k, 200);
                }
            }
            return;
        }
        if clause == Clause::None || clause == Clause::With {
            for k in [
                "SELECT",
                "INSERT INTO",
                "UPDATE",
                "DELETE FROM",
                "AS",
                "RECURSIVE",
            ] {
                self.keyword(k, 200);
            }
        }
        self.dialect_keywords();
    }

    fn dialect_keywords(&mut self) {
        let d = self.doc.dialect();
        let mut ks: Vec<&String> = d.keywords.iter().chain(d.soft_keywords.iter()).collect();
        ks.sort();
        for k in ks {
            self.keyword(&k.to_ascii_uppercase(), -50);
        }
    }

    fn functions(&mut self) {
        let d = self.doc.dialect();
        for f in &d.functions {
            let no_args = f.signatures.iter().all(|s| s.contains("()"));
            self.push(Cand {
                label: f.name.clone(),
                filter: None,
                insert: if no_args {
                    format!("{}()", f.name)
                } else {
                    format!("{}(", f.name)
                },
                kind: CompletionKind::Function,
                detail: f.signatures.first().cloned(),
                prio: 0,
            });
        }
        let env = &self.doc.env;
        let order = env.order.clone();
        let mut routines = Vec::new();
        for &s in &order {
            for r in &env.model.schemas[s].routines {
                routines.push((r.name.clone(), r.signature(), r.arguments.is_empty()));
            }
        }
        for (name, sig, no_args) in routines {
            let q = self.doc.dialect().quote_if_needed(&name);
            self.push(Cand {
                label: name,
                filter: None,
                insert: if no_args {
                    format!("{q}()")
                } else {
                    format!("{q}(")
                },
                kind: CompletionKind::Function,
                detail: Some(sig),
                prio: 20,
            });
        }
    }

    /// Tables of the catalog, CTEs and script tables.
    fn tables(&mut self, with_columns: bool) {
        let d = self.doc.dialect();
        // CTEs visible here.
        if let Some(p) = self.parsed() {
            let (scope, _) = p.at(self.replace.start);
            let mut ctes = Vec::new();
            for (s, _) in p.chain(scope) {
                let owner = p.scopes[s].cte_owner.unwrap_or(s);
                for o in [s, owner] {
                    for c in &p.scopes[o].ctes {
                        ctes.push(c.name.text.clone());
                    }
                }
            }
            for c in ctes {
                self.push(Cand {
                    insert: d.quote_if_needed(&c),
                    label: c,
                    filter: None,
                    kind: CompletionKind::Table,
                    detail: Some("CTE".into()),
                    prio: 150,
                });
            }
        }
        let locals: Vec<(String, Vec<String>)> = self
            .doc
            .env
            .locals
            .iter()
            .map(|l| (l.name.clone(), l.columns.clone()))
            .collect();
        for (name, cols) in locals {
            if with_columns && !cols.is_empty() {
                self.push(Cand {
                    label: format!("{name} ({})", cols.join(", ")),
                    filter: Some(name.clone()),
                    insert: format!(
                        "{} ({})",
                        d.quote_if_needed(&name),
                        cols.iter()
                            .map(|c| d.quote_if_needed(c))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    kind: CompletionKind::Table,
                    detail: Some("script".into()),
                    prio: 90,
                });
            }
            self.push(Cand {
                insert: d.quote_if_needed(&name),
                label: name,
                filter: None,
                kind: CompletionKind::Table,
                detail: Some("script".into()),
                prio: 120,
            });
        }
        let env = &self.doc.env;
        let first = env.order.first().copied();
        let ids: Vec<TableId> = env.model.all_tables(&env.order).collect();
        for id in ids {
            let env = &self.doc.env;
            let t = env.model.table(id);
            let schema = env.model.schema_name(id).to_owned();
            let in_path = env.order.contains(&id.schema);
            let name = if in_path {
                d.quote_if_needed(&t.name)
            } else {
                format!(
                    "{}.{}",
                    d.quote_if_needed(&schema),
                    d.quote_if_needed(&t.name)
                )
            };
            let prio = if Some(id.schema) == first {
                100
            } else if in_path {
                80
            } else {
                30
            };
            let label = if in_path {
                t.name.clone()
            } else {
                format!("{schema}.{}", t.name)
            };
            if with_columns {
                let cols: Vec<String> = t
                    .columns
                    .iter()
                    .filter(|c| !c.generated)
                    .map(|c| d.quote_if_needed(&c.name))
                    .collect();
                if !cols.is_empty() {
                    let c = Cand {
                        label: format!("{label} ({})", cols.join(", ")),
                        filter: Some(t.name.clone()),
                        insert: format!("{name} ({})", cols.join(", ")),
                        kind: CompletionKind::Table,
                        detail: Some(schema.clone()),
                        prio: prio - 10,
                    };
                    self.push(c);
                }
            }
            let c = Cand {
                label,
                filter: Some(t.name.clone()),
                insert: name,
                kind: CompletionKind::Table,
                detail: Some(if t.view {
                    format!("{schema} (view)")
                } else {
                    schema
                }),
                prio,
            };
            self.push(c);
        }
        // Schemas, to qualify names.
        let schemas: Vec<String> = self
            .doc
            .env
            .model
            .schemas
            .iter()
            .map(|s| s.name.clone())
            .collect();
        for s in schemas {
            self.push(Cand {
                insert: d.quote_if_needed(&s),
                label: s,
                filter: None,
                kind: CompletionKind::Keyword,
                detail: Some("schema".into()),
                prio: -20,
            });
        }
    }

    /// After `x.`: columns of source `x`, or tables of schema `x`.
    fn after_dot(&mut self, qual: &[Ident]) {
        let d = self.doc.dialect();
        let Some(last) = qual.last() else { return };
        let qn = last.name(d);
        let mut found: Option<(Vec<SourceColumn>, Option<String>)> = None;
        if let (Some(si), Some(p)) = (self.stmt, self.parsed()) {
            let (scope, _) = p.at(self.replace.start);
            'outer: for (s, visible) in p.chain(scope) {
                if !visible {
                    continue;
                }
                for (i, src) in p.scopes[s].sources.iter().enumerate() {
                    if src.range.start >= self.replace.start.saturating_sub(qn.len() + 1)
                        && src.range.end >= self.replace.start
                        && !src.target
                        && src.alias.is_none()
                    {
                        // The qualifier itself being typed as a source.
                        continue;
                    }
                    let hit = src.exposed().is_some_and(|e| d.is_same(&e.name(d), &qn));
                    if hit {
                        found = Some((
                            self.doc.columns(si, s, i).unwrap_or_default(),
                            src_label(src),
                        ));
                        break 'outer;
                    }
                }
            }
        }
        if let Some((cols, owner)) = found {
            for c in cols {
                self.column_cand(&c, None, owner.clone(), 100);
            }
            self.push(Cand {
                label: "*".into(),
                filter: None,
                insert: "*".into(),
                kind: CompletionKind::Column,
                detail: None,
                prio: -100,
            });
            return;
        }
        // Schema: its tables and routines.
        let env = &self.doc.env;
        let Some(si) = env.model.schema(&qn) else {
            // `schema.table.` → columns of the table.
            if qual.len() >= 2 {
                let tables =
                    env.model
                        .find_tables(Some(&qual[qual.len() - 2].name(d)), &qn, false, &[]);
                if let Some(id) = tables.first() {
                    let t = env.model.table(*id).clone();
                    for c in table_columns(&t, env.model.schema_name(*id)) {
                        self.column_cand(&c, None, Some(t.name.clone()), 100);
                    }
                }
            }
            return;
        };
        let tables: Vec<TableInfo> = env.model.schemas[si].tables.clone();
        let routines = env.model.schemas[si].routines.clone();
        for t in tables {
            self.push(Cand {
                insert: d.quote_if_needed(&t.name),
                label: t.name,
                filter: None,
                kind: CompletionKind::Table,
                detail: None,
                prio: 100,
            });
        }
        for r in routines {
            self.push(Cand {
                insert: format!("{}(", d.quote_if_needed(&r.name)),
                detail: Some(r.signature()),
                label: r.name,
                filter: None,
                kind: CompletionKind::Function,
                prio: 50,
            });
        }
    }

    fn column_cand(
        &mut self,
        c: &SourceColumn,
        qualify: Option<&str>,
        owner: Option<String>,
        prio: i32,
    ) {
        let d = self.doc.dialect();
        let name = d.quote_if_needed(&c.name);
        let insert = match qualify {
            Some(q) => format!("{q}.{name}"),
            None => name,
        };
        let detail = match (&c.data_type, owner) {
            (Some(t), Some(o)) => Some(format!("{t} · {o}")),
            (Some(t), None) => Some(t.clone()),
            (None, o) => o,
        };
        self.push(Cand {
            label: c.name.clone(),
            filter: None,
            insert,
            kind: CompletionKind::Column,
            detail,
            prio,
        });
    }

    /// Columns of every source visible from `scope`.
    fn scope_columns(&mut self, scope: usize, clause: Clause) {
        let Some(si) = self.stmt else { return };
        let doc = self.doc;
        let p = &doc.stmts[si].parsed;
        let d = self.doc.dialect();
        let mut all: Vec<(String, Vec<SourceColumn>, i32)> = Vec::new();
        let mut aliases: Vec<String> = Vec::new();
        for (depth, (s, visible)) in p.chain(scope).into_iter().enumerate() {
            if !visible {
                continue;
            }
            for (i, src) in p.scopes[s].sources.iter().enumerate() {
                let Some(exposed) = src.exposed() else {
                    continue;
                };
                aliases.push(exposed.text.clone());
                if let Some(cols) = self.doc.columns(si, s, i) {
                    all.push((exposed.text.clone(), cols, 100 - 20 * depth as i32));
                }
            }
            // Output aliases in ORDER BY / GROUP BY.
            if matches!(clause, Clause::OrderBy | Clause::GroupBy | Clause::Having) && depth == 0 {
                let outs: Vec<String> = p.scopes[s]
                    .outputs
                    .iter()
                    .filter_map(|o| o.alias.as_ref().map(|a| a.text.clone()))
                    .collect();
                for a in outs {
                    self.push(Cand {
                        insert: d.quote_if_needed(&a),
                        label: a,
                        filter: None,
                        kind: CompletionKind::Column,
                        detail: Some("alias".into()),
                        prio: 110,
                    });
                }
            }
        }
        let several = all.len() > 1;
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (_, cols, _) in &all {
            for c in cols {
                *counts.entry(c.name.to_lowercase()).or_default() += 1;
            }
        }
        for (owner, cols, prio) in all {
            for c in cols {
                let ambiguous = counts.get(&c.name.to_lowercase()).copied().unwrap_or(0) > 1;
                let q = (several && ambiguous).then(|| owner.clone());
                self.column_cand(&c, q.as_deref(), Some(owner.clone()), prio);
            }
        }
        for a in aliases {
            self.push(Cand {
                insert: a.clone(),
                label: a,
                filter: None,
                kind: CompletionKind::Table,
                detail: Some("alias".into()),
                prio: 10,
            });
        }
    }

    fn target(&self, scope: usize) -> Option<(usize, usize)> {
        let p = self.parsed()?;
        let i = p.scopes[scope].sources.iter().position(|s| s.target)?;
        Some((scope, i))
    }

    fn target_columns(&mut self, scope: usize, suffix: &str) {
        let (Some(si), Some((s, i))) = (self.stmt, self.target(scope)) else {
            return;
        };
        let d = self.doc.dialect();
        for c in self.doc.columns(si, s, i).unwrap_or_default() {
            self.push(Cand {
                insert: format!("{}{suffix}", d.quote_if_needed(&c.name)),
                label: c.name.clone(),
                filter: None,
                kind: CompletionKind::Column,
                detail: c.data_type.clone(),
                prio: 100,
            });
        }
    }

    /// `INSERT INTO t (|`: columns not listed yet, and all of them at once.
    fn insert_columns(&mut self, scope: usize) {
        let (Some(si), Some((s, i))) = (self.stmt, self.target(scope)) else {
            return;
        };
        let d = self.doc.dialect();
        let listed: Vec<String> = self
            .parsed()
            .and_then(|p| p.insert.as_ref())
            .map(|ins| {
                ins.columns
                    .iter()
                    .filter(|c| {
                        c.range.end < self.replace.start || c.range.start > self.replace.end
                    })
                    .map(|c| c.name(d).to_lowercase())
                    .collect()
            })
            .unwrap_or_default();
        let cols = self.doc.columns(si, s, i).unwrap_or_default();
        let generated: Vec<String> =
            match self.doc.stmts[si].sources.get(&(s, i)).copied().flatten() {
                Some(TableTarget::Db(id)) => self
                    .doc
                    .env
                    .model
                    .table(id)
                    .columns
                    .iter()
                    .filter(|c| c.generated)
                    .map(|c| c.name.clone())
                    .collect(),
                _ => Vec::new(),
            };
        let rest: Vec<&SourceColumn> = cols
            .iter()
            .filter(|c| !listed.contains(&c.name.to_lowercase()))
            .collect();
        let all: Vec<String> = rest
            .iter()
            .filter(|c| !generated.contains(&c.name))
            .map(|c| d.quote_if_needed(&c.name))
            .collect();
        if all.len() > 1 {
            self.push(Cand {
                label: all.join(", "),
                filter: Some(String::new()),
                insert: all.join(", "),
                kind: CompletionKind::Column,
                detail: Some("all columns".into()),
                prio: 200,
            });
        }
        for c in rest {
            let prio = if generated.contains(&c.name) { 60 } else { 100 };
            self.push(Cand {
                insert: d.quote_if_needed(&c.name),
                label: c.name.clone(),
                filter: None,
                kind: CompletionKind::Column,
                detail: c.data_type.clone(),
                prio,
            });
        }
    }

    /// Sources of `scope` already in the query (not the one being typed).
    fn existing_sources(&self, scope: usize) -> Vec<(usize, Source)> {
        let Some(p) = self.parsed() else {
            return Vec::new();
        };
        p.scopes[scope]
            .sources
            .iter()
            .enumerate()
            .filter(|(_, s)| s.range.end <= self.replace.start && !s.range.is_empty())
            .map(|(i, s)| (i, s.clone()))
            .collect()
    }

    fn db_table(&self, scope: usize, src: usize) -> Option<TableId> {
        match self.doc.stmts[self.stmt?]
            .sources
            .get(&(scope, src))
            .copied()
            .flatten()?
        {
            TableTarget::Db(id) => Some(id),
            _ => None,
        }
    }

    /// After `JOIN`: `table alias ON alias.fk = other.pk` for every foreign
    /// key between an existing source and another table.
    fn joins(&mut self, scope: usize) {
        let d = self.doc.dialect();
        let existing = self.existing_sources(scope);
        let mut taken: Vec<String> = existing
            .iter()
            .filter_map(|(_, s)| s.exposed().map(|e| e.text.to_lowercase()))
            .collect();
        let mut out = Vec::new();
        let model = self.doc.env.model;
        for (i, src) in &existing {
            let Some(id) = self.db_table(scope, *i) else {
                continue;
            };
            let Some(exposed) = src.exposed().map(|e| e.text.clone()) else {
                continue;
            };
            let t = model.table(id);
            // Outgoing: this source references another table.
            for fk in &t.foreign_keys {
                if let Some(target) = model.fk_target(id, fk) {
                    let other = model.table(target);
                    let alias = gen_alias(&other.name, &taken);
                    let cond = pairs(&alias, &fk.ref_columns, &exposed, &fk.columns, d);
                    out.push((target, alias, cond));
                }
            }
            // Incoming: another table references this source.
            for &(from, fi) in model.referencing(id) {
                let other = model.table(from);
                let fk = &other.foreign_keys[fi];
                let alias = gen_alias(&other.name, &taken);
                let cond = pairs(&alias, &fk.columns, &exposed, &fk.ref_columns, d);
                out.push((from, alias, cond));
            }
        }
        for (target, alias, cond) in out {
            let t = model.table(target);
            let schema = model.schema_name(target);
            let name = if self.doc.env.order.contains(&target.schema) {
                d.quote_if_needed(&t.name)
            } else {
                format!(
                    "{}.{}",
                    d.quote_if_needed(schema),
                    d.quote_if_needed(&t.name)
                )
            };
            let text = format!("{name} {alias} ON {cond}");
            taken.push(alias.to_lowercase());
            self.push(Cand {
                label: text.clone(),
                filter: Some(t.name.clone()),
                insert: text,
                kind: CompletionKind::Snippet,
                detail: Some("join on foreign key".into()),
                prio: 160,
            });
        }
    }

    /// After `ON`: conditions between the source being joined and the
    /// sources before it.
    fn join_conditions(&mut self, scope: usize) {
        let d = self.doc.dialect();
        let existing = self.existing_sources(scope);
        let Some((li, last)) = existing.last().cloned() else {
            return;
        };
        let Some(lid) = self.db_table(scope, li) else {
            return;
        };
        let Some(lname) = last.exposed().map(|e| e.text.clone()) else {
            return;
        };
        let model = self.doc.env.model;
        let mut conds = Vec::new();
        for (oi, other) in &existing[..existing.len() - 1] {
            let Some(oid) = self.db_table(scope, *oi) else {
                continue;
            };
            let Some(oname) = other.exposed().map(|e| e.text.clone()) else {
                continue;
            };
            for fk in &model.table(lid).foreign_keys {
                if model.fk_target(lid, fk) == Some(oid) {
                    conds.push(pairs(&lname, &fk.columns, &oname, &fk.ref_columns, d));
                }
            }
            for fk in &model.table(oid).foreign_keys {
                if model.fk_target(oid, fk) == Some(lid) {
                    conds.push(pairs(&lname, &fk.ref_columns, &oname, &fk.columns, d));
                }
            }
        }
        for c in conds {
            self.push(Cand {
                label: c.clone(),
                filter: Some(lname.clone()),
                insert: c,
                kind: CompletionKind::Snippet,
                detail: Some("foreign key".into()),
                prio: 300,
            });
        }
    }
}

/// `a.x = b.y AND a.z = b.w`
fn pairs(
    a: &str,
    acols: &[String],
    b: &str,
    bcols: &[String],
    d: &super::dialect::Dialect,
) -> String {
    acols
        .iter()
        .zip(bcols)
        .map(|(x, y)| {
            format!(
                "{a}.{} = {b}.{}",
                d.quote_if_needed(x),
                d.quote_if_needed(y)
            )
        })
        .collect::<Vec<_>>()
        .join(" AND ")
}

/// Alias from the initials of the name's segments, unique among `taken`.
pub fn gen_alias(name: &str, taken: &[String]) -> String {
    let mut base: String = name
        .split(['_', ' ', '-'])
        .filter_map(|s| s.chars().next())
        .flat_map(char::to_lowercase)
        .collect();
    if base.is_empty() || !base.chars().next().is_some_and(char::is_alphabetic) {
        base = "t".into();
    }
    let mut alias = base.clone();
    let mut n = 1;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&alias)) {
        alias = format!("{base}{n}");
        n += 1;
    }
    alias
}

fn src_label(src: &Source) -> Option<String> {
    match &src.kind {
        SourceKind::Table(parts) => parts.last().map(|p| p.text.clone()),
        _ => src.alias.as_ref().map(|a| a.text.clone()),
    }
}

fn table_columns(t: &TableInfo, schema: &str) -> Vec<SourceColumn> {
    t.columns
        .iter()
        .map(|c| SourceColumn {
            name: c.name.clone(),
            symbol: super::resolve::Symbol::Column {
                schema: Some(schema.to_owned()),
                table: t.name.clone(),
                column: c.name.clone(),
            },
            data_type: Some(c.data_type.clone()),
        })
        .collect()
}

/// Identifier chain ending before the trailing `.` of `before`.
fn qualifier(src: &str, before: &[Token]) -> Vec<Ident> {
    let mut parts = Vec::new();
    let mut i = before.len();
    // before[i-1] is the dot.
    while i >= 2 {
        let t = &before[i - 2];
        let ident = match t.kind {
            TokenKind::Word => Ident {
                text: t.text(src).to_owned(),
                quoted: false,
                range: t.range.clone(),
            },
            TokenKind::QuotedIdent => Ident {
                text: unquote(t.text(src)),
                quoted: true,
                range: t.range.clone(),
            },
            _ => break,
        };
        parts.push(ident);
        i -= 2;
        if i == 0 || before[i - 1].text(src) != "." {
            break;
        }
    }
    parts.reverse();
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::{resolve::tests::shop, schema::SchemaModel};

    fn labels(sql: &str, dialect: &str, model: &SchemaModel) -> Vec<String> {
        let at = sql.find('|').expect("cursor");
        let text = sql.replacen('|', "", 1);
        let doc = Document::analyze(&text, dialect, model, None);
        complete(
            &doc,
            at,
            &templates::builtin(&ferrodb_api::QueryLanguage::Sql(dialect.into())),
        )
        .into_iter()
        .map(|i| i.insert_text)
        .collect()
    }

    #[test]
    fn tables_after_from_and_columns_after_dot() {
        let m = shop("public");
        let l = labels("select * from ord|", "postgresql", &m);
        assert_eq!(l[0], "orders", "{l:?}");
        let l = labels("select o.| from orders o", "postgresql", &m);
        assert!(l.contains(&"customer_id".to_owned()), "{l:?}");
        let l = labels("select * from audit.|", "postgresql", &m);
        assert_eq!(l[0], "log");
        let l = labels("select * from oi|", "postgresql", &m);
        assert_eq!(l[0], "order_items", "{l:?}");
    }

    #[test]
    fn joins_on_foreign_keys() {
        let m = shop("public");
        let l = labels("select * from orders o join |", "postgresql", &m);
        assert!(
            l.contains(&"customers c ON c.id = o.customer_id".to_owned()),
            "{l:?}"
        );
        assert!(
            l.contains(&"order_items oi ON oi.order_id = o.id".to_owned()),
            "{l:?}"
        );
        let l = labels(
            "select * from orders o join customers c on |",
            "postgresql",
            &m,
        );
        assert_eq!(l[0], "c.id = o.customer_id", "{l:?}");
    }

    #[test]
    fn insert_columns() {
        let m = shop("public");
        let l = labels("insert into orders (|)", "postgresql", &m);
        assert_eq!(l[0], "customer_id, total, status, created_at", "{l:?}");
        let l = labels("insert into ord|", "postgresql", &m);
        assert!(l.contains(&"orders (customer_id, total, status, created_at)".to_owned()));
        let l = labels("update orders set st|", "postgresql", &m);
        assert_eq!(l[0], "status = ");
    }

    #[test]
    fn expression_columns_are_qualified_when_ambiguous() {
        let m = shop("public");
        let l = labels(
            "select | from orders o join customers c on c.id = o.customer_id",
            "postgresql",
            &m,
        );
        assert!(l.contains(&"customer_id".to_owned()));
        assert!(
            l.contains(&"o.id".to_owned()) && l.contains(&"c.id".to_owned()),
            "{l:?}"
        );
        let l = labels("select * from orders where tot|", "postgresql", &m);
        assert_eq!(l[0], "total");
    }

    #[test]
    fn keywords_layout_and_templates() {
        let m = SchemaModel::empty();
        let l = labels("sel|", "postgresql", &m);
        assert_eq!(l[0], "SELECT");
        let l = labels("ыуд|", "postgresql", &m);
        assert_eq!(l[0], "SELECT", "{l:?}");
        let m = shop("public");
        let l = labels("select * from щкв|", "postgresql", &m);
        assert_eq!(l[0], "orders", "{l:?}");
        assert!(labels("select 'ord|'", "postgresql", &m).is_empty());
        assert!(labels("-- ord|", "postgresql", &m).is_empty());
    }

    #[test]
    fn alias_generation() {
        assert_eq!(gen_alias("order_items", &[]), "oi");
        assert_eq!(gen_alias("orders", &["o".into()]), "o1");
    }
}
