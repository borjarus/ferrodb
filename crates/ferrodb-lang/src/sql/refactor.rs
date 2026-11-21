//! Navigation and refactoring on top of name resolution: "Go to
//! declaration", "Rename", hover, "Parameter info" and the structure view.

use std::ops::Range;

use ferrodb_api::{
    Declaration, Error, Hover, Result, Signature, SignatureHelp, StructureItem, StructureKind,
    TextEdit,
};

use super::{
    dialect::parse_signature,
    document::Document,
    lexer::TokenKind,
    resolve::Symbol,
    schema::TableId,
    syntax::{Role, SourceKind, StmtKind},
};

/// Symbol at `offset`: from a reference or a definition site.
/// Returns (statement, symbol, range of the name under the cursor).
pub fn symbol_at(doc: &Document<'_>, offset: usize) -> Option<(usize, Symbol, Range<usize>)> {
    if let Some((si, ri, pi)) = doc.ref_at(offset) {
        let sym = doc.stmts[si].refs[ri].parts[pi].clone()?;
        return Some((
            si,
            sym,
            doc.stmts[si].parsed.refs[ri].parts[pi].range.clone(),
        ));
    }
    let si = doc.statement_at(offset)?;
    definitions(doc, si)
        .into_iter()
        .find(|(r, _)| r.start <= offset && offset <= r.end)
        .map(|(r, s)| (si, s, r))
}

/// Definition sites of statement `si`: (name range, symbol).
fn definitions(doc: &Document<'_>, si: usize) -> Vec<(Range<usize>, Symbol)> {
    let p = &doc.stmts[si].parsed;
    let d = doc.dialect();
    let mut out = Vec::new();
    for (sc, scope) in p.scopes.iter().enumerate() {
        for (i, src) in scope.sources.iter().enumerate() {
            if let Some(a) = &src.alias
                && !a.range.is_empty()
            {
                out.push((
                    a.range.clone(),
                    Symbol::Alias {
                        scope: sc,
                        source: i,
                    },
                ));
            }
        }
        for (ci, c) in scope.ctes.iter().enumerate() {
            out.push((c.name.range.clone(), Symbol::Cte { scope: sc, idx: ci }));
            for (k, col) in c.columns.iter().enumerate() {
                out.push((
                    col.range.clone(),
                    Symbol::CteColumn {
                        scope: sc,
                        idx: ci,
                        col: k,
                    },
                ));
            }
        }
        for (oi, o) in scope.outputs.iter().enumerate() {
            if let Some(a) = &o.alias {
                out.push((a.range.clone(), Symbol::Output { scope: sc, idx: oi }));
            }
        }
    }
    if let Some(c) = &p.created
        && let Some(name) = c.name.last()
    {
        let schema = (c.name.len() > 1).then(|| c.name[c.name.len() - 2].name(d));
        let table = name.name(d);
        out.push((
            name.range.clone(),
            Symbol::Table {
                schema: schema.clone(),
                name: table.clone(),
            },
        ));
        for col in &c.columns {
            out.push((
                col.range.clone(),
                Symbol::Column {
                    schema: schema.clone(),
                    table: table.clone(),
                    column: col.name(d),
                },
            ));
        }
    }
    out
}

/// Same database object (schemas compare loosely: an unqualified local
/// table matches its qualified uses).
fn same(a: &Symbol, b: &Symbol, doc: &Document<'_>) -> bool {
    let d = doc.dialect();
    let eq_opt = |x: &Option<String>, y: &Option<String>| match (x, y) {
        (Some(x), Some(y)) => d.is_same(x, y),
        _ => true,
    };
    match (a, b) {
        (
            Symbol::Table {
                schema: s1,
                name: n1,
            },
            Symbol::Table {
                schema: s2,
                name: n2,
            },
        ) => d.is_same(n1, n2) && eq_opt(s1, s2),
        (
            Symbol::Column {
                schema: s1,
                table: t1,
                column: c1,
            },
            Symbol::Column {
                schema: s2,
                table: t2,
                column: c2,
            },
        ) => d.is_same(t1, t2) && d.is_same(c1, c2) && eq_opt(s1, s2),
        (Symbol::Schema(x), Symbol::Schema(y)) | (Symbol::Function(x), Symbol::Function(y)) => {
            d.is_same(x, y)
        }
        _ => a == b,
    }
}

/// Every range naming `sym` (local symbols: within statement `si`).
pub fn occurrences(doc: &Document<'_>, si: usize, sym: &Symbol) -> Vec<Range<usize>> {
    let stmts: Vec<usize> = if sym.is_global() {
        (0..doc.stmts.len()).collect()
    } else {
        vec![si]
    };
    let mut out = Vec::new();
    for s in stmts {
        let st = &doc.stmts[s];
        for (r, res) in st.parsed.refs.iter().zip(&st.refs) {
            for (p, rs) in r.parts.iter().zip(&res.parts) {
                if let Some(x) = rs
                    && same(x, sym, doc)
                    && !p.range.is_empty()
                {
                    out.push(p.range.clone());
                }
            }
        }
        for (r, x) in definitions(doc, s) {
            if same(&x, sym, doc) {
                out.push(r);
            }
        }
    }
    out.sort_by_key(|r| r.start);
    out.dedup();
    out
}

pub fn declaration(doc: &Document<'_>, offset: usize) -> Option<Declaration> {
    let (si, sym, _) = symbol_at(doc, offset)?;
    let p = &doc.stmts[si].parsed;
    let local_table = |schema: &Option<String>, name: &str| {
        doc.env.locals.iter().rfind(|l| {
            doc.dialect().is_same(&l.name, name)
                && match (schema, &l.schema) {
                    (Some(a), Some(b)) => doc.dialect().is_same(a, b),
                    (Some(_), None) => false,
                    _ => true,
                }
        })
    };
    Some(match sym {
        Symbol::Alias { scope, source } => {
            let src = &p.scopes[scope].sources[source];
            match &src.alias {
                Some(a) if !a.range.is_empty() => Declaration::Local(a.range.clone()),
                _ => return None,
            }
        }
        Symbol::Cte { scope, idx } => {
            Declaration::Local(p.scopes[scope].ctes[idx].name.range.clone())
        }
        Symbol::CteColumn { scope, idx, col } => {
            Declaration::Local(p.scopes[scope].ctes[idx].columns[col].range.clone())
        }
        Symbol::Output { scope, idx } => {
            let o = &p.scopes[scope].outputs[idx];
            Declaration::Local(
                o.alias
                    .as_ref()
                    .map_or(o.range.clone(), |a| a.range.clone()),
            )
        }
        Symbol::Table { schema, name } => match local_table(&schema, &name) {
            Some(l) => Declaration::Local(l.decl.clone()),
            None => Declaration::Object(schema.into_iter().chain([name]).collect()),
        },
        Symbol::Column {
            schema,
            table,
            column,
        } => match local_table(&schema, &table) {
            Some(l) => Declaration::Local(l.decl.clone()),
            None => Declaration::Object(schema.into_iter().chain([table, column]).collect()),
        },
        Symbol::Schema(s) => Declaration::Object(vec![s]),
        Symbol::Function(f) => Declaration::Object(vec![f]),
    })
}

pub fn prepare_rename(doc: &Document<'_>, offset: usize) -> Option<(Range<usize>, String)> {
    let (_, sym, range) = symbol_at(doc, offset)?;
    if matches!(sym, Symbol::Function(_) | Symbol::Schema(_)) {
        return None;
    }
    Some((range.clone(), doc.text[range].to_owned()))
}

pub fn rename(doc: &Document<'_>, offset: usize, new_name: &str) -> Result<Vec<TextEdit>> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err(Error::Other("the new name is empty".into()));
    }
    let Some((si, sym, _)) = symbol_at(doc, offset) else {
        return Err(Error::Unsupported("nothing to rename here".into()));
    };
    if matches!(sym, Symbol::Function(_) | Symbol::Schema(_)) {
        return Err(Error::Unsupported("renaming functions and schemas".into()));
    }
    let d = doc.dialect();
    let quoted = new_name.starts_with(d.quote) || new_name.starts_with('"');
    let text = if quoted {
        new_name.to_owned()
    } else {
        d.quote_if_needed(new_name)
    };
    Ok(occurrences(doc, si, &sym)
        .into_iter()
        .map(|range| TextEdit {
            range,
            new_text: text.clone(),
        })
        .collect())
}

fn find_table(doc: &Document<'_>, schema: &Option<String>, name: &str) -> Option<TableId> {
    let m = doc.env.model;
    m.find_tables(schema.as_deref(), name, true, &doc.env.order)
        .first()
        .copied()
        .or_else(|| {
            m.find_tables(schema.as_deref(), name, false, &doc.env.order)
                .first()
                .copied()
        })
}

fn table_hover(doc: &Document<'_>, schema: &Option<String>, name: &str) -> Option<String> {
    let id = find_table(doc, schema, name)?;
    let m = doc.env.model;
    let t = m.table(id);
    let mut s = format!(
        "**{}.{}** — {}\n",
        m.schema_name(id),
        t.name,
        if t.view { "view" } else { "table" }
    );
    if let Some(c) = &t.comment {
        s.push_str(&format!("\n{c}\n"));
    }
    s.push('\n');
    for c in t.columns.iter().take(25) {
        s.push_str(&format!(
            "- `{}` {}{}\n",
            c.name,
            c.data_type,
            if c.primary_key { " (PK)" } else { "" }
        ));
    }
    if t.columns.len() > 25 {
        s.push_str(&format!("- … {} more\n", t.columns.len() - 25));
    }
    Some(s)
}

pub fn hover(doc: &Document<'_>, offset: usize) -> Option<Hover> {
    let (si, sym, range) = symbol_at(doc, offset)?;
    let p = &doc.stmts[si].parsed;
    let contents = match &sym {
        Symbol::Column {
            schema,
            table,
            column,
        } => match find_table(doc, schema, table) {
            Some(id) => {
                let m = doc.env.model;
                let t = m.table(id);
                let c = t.column(column).or_else(|| {
                    t.columns
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(column))
                })?;
                let mut s = format!(
                    "**{}.{}** `{}`{}{}",
                    t.name,
                    c.name,
                    c.data_type,
                    if c.nullable { "" } else { " NOT NULL" },
                    if c.primary_key { " · primary key" } else { "" }
                );
                if let Some(fk) = t.foreign_keys.iter().find(|f| f.columns.contains(&c.name)) {
                    s.push_str(&format!(" · → {}", fk.ref_table));
                }
                if let Some(com) = &c.comment {
                    s.push_str(&format!("\n\n{com}"));
                }
                s
            }
            None => format!("**{table}.{column}**"),
        },
        Symbol::Table { schema, name } => table_hover(doc, schema, name)
            .unwrap_or_else(|| format!("**{name}** — table of this script")),
        Symbol::Alias { scope, source } => {
            let src = &p.scopes[*scope].sources[*source];
            match (
                &src.kind,
                doc.stmts[si]
                    .sources
                    .get(&(*scope, *source))
                    .copied()
                    .flatten(),
            ) {
                (_, Some(super::resolve::TableTarget::Db(id))) => {
                    let m = doc.env.model;
                    table_hover(doc, &Some(m.schema_name(id).to_owned()), &m.table(id).name)?
                }
                (SourceKind::Subquery(_), _) => "Alias of a subquery".into(),
                _ => format!("Alias of {}", doc.text[src.range.clone()].trim()),
            }
        }
        Symbol::Cte { scope, idx } => {
            let c = &p.scopes[*scope].ctes[*idx];
            format!("**{}** — common table expression", c.name.text)
        }
        Symbol::CteColumn { scope, idx, col } => {
            let c = &p.scopes[*scope].ctes[*idx];
            format!("**{}.{}** — CTE column", c.name.text, c.columns[*col].text)
        }
        Symbol::Output { scope, idx } => {
            let o = &p.scopes[*scope].outputs[*idx];
            format!("Result column: `{}`", doc.text[o.range.clone()].trim())
        }
        Symbol::Function(name) => {
            let sigs = signatures(doc, name);
            if sigs.is_empty() {
                return None;
            }
            let mut s = sigs
                .iter()
                .map(|x| format!("`{}`", x.label))
                .collect::<Vec<_>>()
                .join("\n\n");
            if let Some(doc_text) = sigs.iter().find_map(|x| x.documentation.clone()) {
                s.push_str(&format!("\n\n{doc_text}"));
            }
            s
        }
        Symbol::Schema(s) => format!("**{s}** — schema"),
    };
    Some(Hover { range, contents })
}

/// Signatures of a function: dialect built-ins, then catalog routines.
fn signatures(doc: &Document<'_>, name: &str) -> Vec<Signature> {
    let d = doc.dialect();
    let mut raw: Vec<(String, Option<String>)> = Vec::new();
    if let Some(f) = d.function(name) {
        raw.extend(f.signatures.iter().map(|s| (s.clone(), f.doc.clone())));
    }
    for r in doc.env.model.routines(name, &doc.env.order) {
        raw.push((r.signature(), r.comment.clone()));
    }
    raw.into_iter()
        .map(|(label, documentation)| {
            let parsed = parse_signature(&label);
            let mut ranges = Vec::new();
            let mut from = label.find('(').map_or(0, |i| i + 1);
            for p in &parsed.params {
                if let Some(i) = label[from..].find(p.as_str()) {
                    ranges.push(from + i..from + i + p.len());
                    from += i + p.len();
                }
            }
            Signature {
                label,
                parameters: ranges,
                documentation,
            }
        })
        .collect()
}

pub fn signature_help(doc: &Document<'_>, offset: usize) -> Option<SignatureHelp> {
    let si = doc.statement_at(offset)?;
    let toks = &doc.stmts[si].toks;
    let before = toks.partition_point(|t| t.range.end <= offset);
    let mut depth = 0i32;
    let mut commas = 0usize;
    let mut open = None;
    for i in (0..before).rev() {
        let t = &toks[i];
        if t.kind != TokenKind::Punct {
            continue;
        }
        match t.text(doc.text) {
            ")" => depth += 1,
            "(" if depth == 0 => {
                open = Some(i);
                break;
            }
            "(" => depth -= 1,
            "," if depth == 0 => commas += 1,
            _ => {}
        }
    }
    let open = open?;
    let name_tok = toks.get(open.checked_sub(1)?)?;
    if !matches!(name_tok.kind, TokenKind::Word | TokenKind::QuotedIdent) {
        return None;
    }
    let name = if name_tok.kind == TokenKind::QuotedIdent {
        super::syntax::unquote(name_tok.text(doc.text))
    } else {
        name_tok.text(doc.text).to_owned()
    };
    let sigs = signatures(doc, &name);
    if sigs.is_empty() {
        return None;
    }
    let active_signature = sigs
        .iter()
        .position(|s| {
            let p = parse_signature(&s.label);
            p.variadic || p.params.len() > commas
        })
        .unwrap_or(0);
    let p = parse_signature(&sigs[active_signature].label);
    let active_parameter = if p.variadic && commas + 1 >= p.params.len() {
        p.params.len().saturating_sub(2)
    } else {
        commas.min(p.params.len().saturating_sub(1))
    };
    Some(SignatureHelp {
        signatures: sigs,
        active_signature,
        active_parameter,
    })
}

/// Statements of the console with their CTEs, sources and subqueries.
pub fn structure(doc: &Document<'_>) -> Vec<StructureItem> {
    let d = doc.dialect();
    doc.stmts
        .iter()
        .map(|s| {
            let p = &s.parsed;
            let words: Vec<String> = s
                .toks
                .iter()
                .take(4)
                .map(|t| {
                    let x = t.text(doc.text);
                    if t.kind == TokenKind::Word && d.is_keyword(x) {
                        x.to_ascii_uppercase()
                    } else {
                        x.to_owned()
                    }
                })
                .collect();
            let mut label = match (&p.created, p.kind) {
                (Some(c), _) => format!(
                    "CREATE {} {}",
                    if c.view { "VIEW" } else { "TABLE" },
                    c.name
                        .iter()
                        .map(|x| x.text.as_str())
                        .collect::<Vec<_>>()
                        .join(".")
                ),
                (
                    None,
                    StmtKind::Select
                    | StmtKind::Insert
                    | StmtKind::Update
                    | StmtKind::Delete
                    | StmtKind::Merge,
                ) => {
                    let tables: Vec<String> = p
                        .refs
                        .iter()
                        .filter(|r| matches!(r.role, Role::Table { .. }))
                        .map(|r| {
                            r.parts
                                .iter()
                                .map(|x| x.text.as_str())
                                .collect::<Vec<_>>()
                                .join(".")
                        })
                        .collect();
                    let verb = match p.kind {
                        StmtKind::Insert => "INSERT",
                        StmtKind::Update => "UPDATE",
                        StmtKind::Delete => "DELETE",
                        StmtKind::Merge => "MERGE",
                        _ => "SELECT",
                    };
                    if tables.is_empty() {
                        verb.to_owned()
                    } else {
                        format!("{verb} {}", tables.join(", "))
                    }
                }
                _ => words.join(" "),
            };
            if label.chars().count() > 80 {
                label = label.chars().take(79).collect::<String>() + "…";
            }
            let first = s
                .toks
                .first()
                .map_or(s.span.body.clone(), |t| t.range.clone());
            StructureItem {
                label,
                detail: s.span.title.clone(),
                kind: StructureKind::Statement,
                range: s.span.range.clone(),
                selection: first,
                children: scope_children(s, 0),
            }
        })
        .collect()
}

fn scope_children(s: &super::document::StmtAnalysis, scope: usize) -> Vec<StructureItem> {
    let p = &s.parsed;
    let sc = &p.scopes[scope];
    let mut out = Vec::new();
    for c in &sc.ctes {
        out.push(StructureItem {
            label: c.name.text.clone(),
            detail: Some("CTE".into()),
            kind: StructureKind::Cte,
            range: c.range.clone(),
            selection: c.name.range.clone(),
            children: scope_children(s, c.scope),
        });
    }
    for src in &sc.sources {
        if src.range.is_empty() {
            continue;
        }
        match &src.kind {
            SourceKind::Table(parts) => out.push(StructureItem {
                label: parts
                    .iter()
                    .map(|x| x.text.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                detail: src.alias.as_ref().map(|a| a.text.clone()),
                kind: StructureKind::Table,
                range: src.range.clone(),
                selection: parts.last().map_or(src.range.clone(), |x| x.range.clone()),
                children: Vec::new(),
            }),
            SourceKind::Subquery(q) => out.push(StructureItem {
                label: "(subquery)".into(),
                detail: src.alias.as_ref().map(|a| a.text.clone()),
                kind: StructureKind::Query,
                range: src.range.clone(),
                selection: src
                    .alias
                    .as_ref()
                    .map_or(src.range.clone(), |a| a.range.clone()),
                children: scope_children(s, *q),
            }),
            SourceKind::Function(f) => out.push(StructureItem {
                label: format!("{}()", f.text),
                detail: src.alias.as_ref().map(|a| a.text.clone()),
                kind: StructureKind::Function,
                range: src.range.clone(),
                selection: f.range.clone(),
                children: Vec::new(),
            }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::resolve::tests::shop;

    fn apply(sql: &str, edits: &[TextEdit]) -> String {
        let mut s = sql.to_owned();
        let mut e = edits.to_vec();
        e.sort_by_key(|x| std::cmp::Reverse(x.range.start));
        for x in e {
            s.replace_range(x.range, &x.new_text);
        }
        s
    }

    #[test]
    fn rename_alias_cte_and_column() {
        let m = shop("public");
        let sql = "select o.id from orders o where o.total > 0 order by o.id";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let e = rename(&doc, 24, "ord").unwrap();
        assert_eq!(
            apply(sql, &e),
            "select ord.id from orders ord where ord.total > 0 order by ord.id"
        );
        let sql = "with t as (select 1 x) select x from t; select * from t";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let e = rename(&doc, 5, "Big").unwrap();
        // Local to its statement; quoted because PostgreSQL folds case.
        assert_eq!(
            apply(sql, &e),
            "with \"Big\" as (select 1 x) select x from \"Big\"; select * from t"
        );
        let sql = "select total from orders; update orders set total = 1 where total > 2";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let e = rename(&doc, 8, "amount").unwrap();
        assert_eq!(e.len(), 3);
        assert!(prepare_rename(&doc, 8).is_some());
        let sql = "select sum(total) s from orders order by s";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let e = rename(&doc, 41, "summa").unwrap();
        assert_eq!(
            apply(sql, &e),
            "select sum(total) summa from orders order by summa"
        );
    }

    #[test]
    fn declarations() {
        let m = shop("public");
        let sql = "select o.total from orders o";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        assert_eq!(declaration(&doc, 7), Some(Declaration::Local(27..28)));
        assert_eq!(
            declaration(&doc, 10),
            Some(Declaration::Object(vec![
                "public".into(),
                "orders".into(),
                "total".into()
            ]))
        );
        let sql = "create table tmp (a int); select a from tmp";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        assert_eq!(declaration(&doc, 41), Some(Declaration::Local(13..16)));
    }

    #[test]
    fn hover_signature_and_structure() {
        let m = shop("public");
        let sql = "select o.total, round(o.total, ) from orders o";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let h = hover(&doc, 10).unwrap();
        assert!(h.contents.contains("`numeric`"), "{}", h.contents);
        let s = signature_help(&doc, 31).unwrap();
        assert_eq!(s.active_parameter, 1);
        assert_eq!(
            s.signatures[s.active_signature].label,
            "round(x numeric, digits int) -> numeric"
        );
        let sig = &s.signatures[1];
        assert_eq!(&sig.label[sig.parameters[1].clone()], "digits int");
        let sql = "-- Big orders\nwith t as (select * from orders) select * from t join customers c on true;\ninsert into orders (id) values (1)";
        let doc = Document::analyze(sql, "postgresql", &m, None);
        let st = structure(&doc);
        assert_eq!(st.len(), 2);
        assert_eq!(st[0].detail.as_deref(), Some("Big orders"));
        assert_eq!(st[0].label, "SELECT orders, t, customers");
        assert_eq!(st[0].children[0].label, "t");
        assert_eq!(st[1].label, "INSERT orders");
    }
}
