//! A permissive structural parser of one SQL statement.
//!
//! It does not build a full AST: it finds what name resolution, completion
//! and refactorings need — query scopes (each `SELECT`, subquery, CTE
//! body), their sources (`FROM`/`JOIN` tables, subqueries, table
//! functions) with aliases, CTEs, output columns, the clause at every
//! position and every name reference with its role. It never fails:
//! unknown constructs are skipped, so half-typed statements still give
//! useful scopes for completion.

use std::ops::Range;

use super::{
    dialect::Dialect,
    lexer::{Token, TokenKind},
};

/// An identifier as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ident {
    /// Text without quotes (doubled quotes unescaped).
    pub text: String,
    pub quoted: bool,
    pub range: Range<usize>,
}

impl Ident {
    /// The stored name: quoted as is, unquoted folded by the dialect.
    pub fn name(&self, d: &Dialect) -> String {
        if self.quoted {
            self.text.clone()
        } else {
            d.fold(&self.text)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clause {
    /// Statement start / unknown.
    None,
    With,
    Select,
    From,
    Join,
    On,
    Using,
    Where,
    GroupBy,
    Having,
    OrderBy,
    Limit,
    Window,
    /// `INSERT INTO t`, `UPDATE t`, `DELETE FROM t`: the target table.
    Target,
    InsertColumns,
    Values,
    Set,
    Returning,
    /// DDL and other statements.
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StmtKind {
    Select,
    Insert,
    Update,
    Delete,
    CreateTable,
    CreateView,
    CreateIndex,
    Ddl,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// Qualified table name.
    Table(Vec<Ident>),
    /// Subquery or `VALUES` list: its scope.
    Subquery(usize),
    /// Table function (`generate_series(…)`, `unnest(…)`): unknown columns.
    Function(Ident),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    pub kind: SourceKind,
    pub alias: Option<Ident>,
    /// `AS x(a, b)`.
    pub column_aliases: Vec<Ident>,
    pub range: Range<usize>,
    /// Introduced by `JOIN`.
    pub joined: bool,
    /// The statement target (`INSERT INTO`, `UPDATE`, `DELETE FROM`).
    pub target: bool,
}

impl Source {
    /// Name used to qualify columns: the alias, else the table name.
    pub fn exposed(&self) -> Option<&Ident> {
        self.alias.as_ref().or(match &self.kind {
            SourceKind::Table(parts) => parts.last(),
            SourceKind::Function(f) => Some(f),
            SourceKind::Subquery(_) => None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cte {
    pub name: Ident,
    pub columns: Vec<Ident>,
    /// Scope of the body.
    pub scope: usize,
    /// `name … (body)`.
    pub range: Range<usize>,
}

/// One item of a select list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Output {
    pub alias: Option<Ident>,
    /// The item is a bare column reference (index into `Parsed::refs`).
    pub column_ref: Option<usize>,
    /// `*` or `t.*` (the qualifier).
    pub star: Option<Option<Ident>>,
    pub range: Range<usize>,
}

impl Output {
    /// Output column name, when known.
    pub fn name<'a>(&'a self, refs: &'a [NameRef]) -> Option<&'a Ident> {
        self.alias
            .as_ref()
            .or_else(|| self.column_ref.and_then(|r| refs[r].parts.last()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    pub parent: Option<usize>,
    /// CTE bodies see the CTEs of their parents but not their sources.
    pub cte_body: bool,
    pub range: Range<usize>,
    pub sources: Vec<Source>,
    pub ctes: Vec<Cte>,
    pub outputs: Vec<Output>,
    /// `UNION` branches after the first: they share its CTEs.
    pub cte_owner: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Column,
    /// Column of the statement target (`INSERT (…)`, `UPDATE … SET x =`).
    TargetColumn,
    /// `ORDER BY` / `GROUP BY` / `HAVING`: select aliases are visible.
    SortColumn,
    /// Name of a table source (`sources[source]` of the scope).
    Table { source: usize },
    /// Table named in DDL (`ALTER TABLE x`, `DROP TABLE x`): no check.
    DdlTable,
    Function,
    /// `t.*`
    Star,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NameRef {
    pub parts: Vec<Ident>,
    pub scope: usize,
    pub role: Role,
    /// A `.` follows the last part (typing in progress).
    pub trailing_dot: bool,
}

impl NameRef {
    pub fn range(&self) -> Range<usize> {
        let s = self.parts.first().map_or(0, |p| p.range.start);
        let e = self.parts.last().map_or(0, |p| p.range.end);
        s..e
    }
}

/// `INSERT INTO t (cols) VALUES (…), (…)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InsertInfo {
    pub scope: usize,
    pub columns: Vec<Ident>,
    /// Number of values and range of every `VALUES` row.
    pub rows: Vec<(usize, Range<usize>)>,
}

/// A table created by the statement, visible to later statements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    pub name: Vec<Ident>,
    pub columns: Vec<Ident>,
    pub view: bool,
    /// `CREATE VIEW v AS SELECT`: columns from this scope's outputs.
    pub query: Option<usize>,
}

/// Position where the clause changes (for "clause at offset").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub pos: usize,
    pub scope: usize,
    pub clause: Clause,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parsed {
    pub kind: StmtKind,
    pub range: Range<usize>,
    pub scopes: Vec<Scope>,
    pub refs: Vec<NameRef>,
    pub marks: Vec<Mark>,
    pub insert: Option<InsertInfo>,
    pub created: Option<Created>,
    /// `UPDATE`/`DELETE` has a top-level `WHERE`.
    pub has_where: bool,
    /// Comparisons with `NULL` by `=`/`<>`/`!=`: (operator range, negated).
    pub null_comparisons: Vec<(Range<usize>, Range<usize>, bool)>,
}

impl Parsed {
    /// Innermost scope and clause at `pos`.
    pub fn at(&self, pos: usize) -> (usize, Clause) {
        let mut best = (0, Clause::None);
        for m in &self.marks {
            if m.pos <= pos {
                best = (m.scope, m.clause);
            } else {
                break;
            }
        }
        best
    }

    /// Scope chain from `scope` outwards: (scope, sources visible).
    pub fn chain(&self, scope: usize) -> Vec<(usize, bool)> {
        let mut out = Vec::new();
        let mut cur = Some(scope);
        let mut sources = true;
        while let Some(s) = cur {
            out.push((s, sources));
            if self.scopes[s].cte_body {
                sources = false;
            }
            cur = self.scopes[s].parent;
        }
        out
    }

    /// CTE named `name` visible from `scope`.
    pub fn find_cte(&self, scope: usize, name: &str, d: &Dialect) -> Option<(usize, usize)> {
        for (s, _) in self.chain(scope) {
            let owner = self.scopes[s].cte_owner.unwrap_or(s);
            for o in [s, owner] {
                if let Some(i) = self.scopes[o]
                    .ctes
                    .iter()
                    .position(|c| names_eq(&c.name.name(d), name, d))
                {
                    return Some((o, i));
                }
            }
        }
        None
    }
}

/// Name equality under the dialect rules (case-insensitive unless folded).
pub fn names_eq(a: &str, b: &str, d: &Dialect) -> bool {
    match d.folding {
        super::dialect::Folding::None => a.eq_ignore_ascii_case(b),
        _ => a == b,
    }
}

/// Words that end a source and are never its alias.
const NOT_ALIAS: &[&str] = &[
    "where", "group", "order", "having", "limit", "offset", "union", "intersect", "except",
    "minus", "join", "inner", "left", "right", "full", "cross", "natural", "outer", "lateral",
    "straight_join", "on", "using", "set", "values", "select", "from", "into", "for", "fetch",
    "window", "returning", "tablesample", "force", "ignore", "use", "partition", "with", "do",
    "then", "when", "else", "end", "and", "or", "not", "as", "default", "qualify",
];

/// Soft keywords that start a keyword phrase when next to another keyword.
const PHRASE_WORDS: &[&str] = &[
    "nulls", "first", "last", "unbounded", "preceding", "following", "current", "row",
    "rows", "range", "groups", "nothing", "conflict", "only", "ties", "exclude", "others",
    "no", "action", "cascade", "restrict", "duplicate", "key", "zone", "time", "local",
];

/// Words after which an expression starts (an identifier there is a name).
const EXPR_START: &[&str] = &[
    "select", "where", "and", "or", "on", "by", "set", "when", "then", "else", "not", "having",
    "distinct", "returning", "in", "is", "like", "between", "case", "all", "any", "some",
    "exists", "using", "values", "as",
];

/// Parses the statement whose significant tokens are `toks` (absolute
/// ranges into `src`).
pub fn parse(src: &str, toks: &[Token], d: &Dialect) -> Parsed {
    let range = match (toks.first(), toks.last()) {
        (Some(a), Some(b)) => a.range.start..b.range.end,
        _ => 0..0,
    };
    let mut p = Parser {
        src,
        toks,
        d,
        i: 0,
        out: Parsed {
            kind: StmtKind::Other,
            range: range.clone(),
            scopes: Vec::new(),
            refs: Vec::new(),
            marks: Vec::new(),
            insert: None,
            created: None,
            has_where: false,
            null_comparisons: Vec::new(),
        },
    };
    p.statement();
    p.out.marks.sort_by_key(|m| m.pos);
    p.out
}

struct Parser<'a> {
    src: &'a str,
    toks: &'a [Token],
    d: &'a Dialect,
    i: usize,
    out: Parsed,
}

/// Per-scope state while scanning a query.
struct QState {
    scope: usize,
    clause: Clause,
    expect_source: bool,
    /// Start of the current select item (token index).
    item_start: Option<usize>,
    /// `refs.len()` when the item started.
    item_refs: usize,
    /// Insert target scope for `INSERT … SELECT`.
    in_extract: bool,
}

impl<'a> Parser<'a> {
    fn tok(&self, i: usize) -> Option<&'a Token> {
        self.toks.get(i)
    }

    fn word(&self, i: usize) -> Option<String> {
        let t = self.toks.get(i)?;
        (t.kind == TokenKind::Word).then(|| t.text(self.src).to_ascii_lowercase())
    }

    fn is_word(&self, i: usize, w: &str) -> bool {
        self.toks.get(i).is_some_and(|t| t.is_word(self.src, w))
    }

    fn is_punct(&self, i: usize, p: &str) -> bool {
        self.toks
            .get(i)
            .is_some_and(|t| t.kind == TokenKind::Punct && t.text(self.src) == p)
    }

    fn end_pos(&self) -> usize {
        self.out.range.end
    }

    fn pos(&self, i: usize) -> usize {
        self.toks.get(i).map_or(self.end_pos(), |t| t.range.start)
    }

    /// Identifier at `i`: a non-reserved word or a quoted identifier.
    fn ident(&self, i: usize) -> Option<Ident> {
        let t = self.toks.get(i)?;
        match t.kind {
            TokenKind::Word => {
                let w = t.text(self.src);
                if self.d.is_keyword(w) {
                    return None;
                }
                Some(Ident {
                    text: w.to_owned(),
                    quoted: false,
                    range: t.range.clone(),
                })
            }
            TokenKind::QuotedIdent => Some(Ident {
                text: unquote(t.text(self.src)),
                quoted: true,
                range: t.range.clone(),
            }),
            _ => None,
        }
    }

    /// `a.b.c` starting at `i`; returns parts, trailing dot, next index.
    fn qualified(&self, mut i: usize) -> Option<(Vec<Ident>, bool, usize)> {
        let first = self.ident(i)?;
        let mut parts = vec![first];
        i += 1;
        let mut trailing = false;
        while self.is_punct(i, ".") {
            // Reserved words are fine after a dot (`t.order` is a name).
            let dot_end = self.toks[i].range.end;
            match self.tok(i + 1) {
                // `o. FROM`: typing in progress, the keyword is not a name.
                Some(t)
                    if matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent)
                        && !(t.range.start > dot_end
                            && t.kind == TokenKind::Word
                            && self.d.is_keyword(t.text(self.src))) =>
                {
                    let quoted = t.kind == TokenKind::QuotedIdent;
                    let raw = t.text(self.src);
                    parts.push(Ident {
                        text: if quoted { unquote(raw) } else { raw.to_owned() },
                        quoted,
                        range: t.range.clone(),
                    });
                    i += 2;
                }
                _ => {
                    trailing = !self.is_punct(i + 1, "*");
                    if trailing {
                        i += 1;
                    }
                    break;
                }
            }
        }
        Some((parts, trailing, i))
    }

    fn new_scope(&mut self, parent: Option<usize>, start: usize, cte_body: bool) -> usize {
        self.out.scopes.push(Scope {
            parent,
            cte_body,
            range: start..self.end_pos(),
            sources: Vec::new(),
            ctes: Vec::new(),
            outputs: Vec::new(),
            cte_owner: None,
        });
        self.out.scopes.len() - 1
    }

    fn mark(&mut self, pos: usize, scope: usize, clause: Clause) {
        self.out.marks.push(Mark { pos, scope, clause });
    }

    fn push_ref(&mut self, parts: Vec<Ident>, scope: usize, role: Role, trailing_dot: bool) -> usize {
        self.out.refs.push(NameRef {
            parts,
            scope,
            role,
            trailing_dot,
        });
        self.out.refs.len() - 1
    }

    // ------------------------------------------------------------ statement

    fn statement(&mut self) {
        let root = self.new_scope(None, self.out.range.start, false);
        self.mark(self.out.range.start, root, Clause::None);
        let first = self.word(0).unwrap_or_default();
        match first.as_str() {
            "select" | "with" | "values" | "table" | "(" => {
                self.out.kind = StmtKind::Select;
                self.query(root, 0, false);
            }
            "insert" | "replace" => self.insert(root),
            "update" => self.update(root),
            "delete" => self.delete(root),
            "create" => self.create(root),
            "alter" | "drop" | "truncate" | "comment" | "lock" | "analyze" | "vacuum" => {
                self.ddl(root)
            }
            "explain" => {
                // `EXPLAIN [ANALYZE] [(…)] SELECT …`
                let mut i = 1;
                while i < self.toks.len() && !self.is_word(i, "select") && !self.is_word(i, "with")
                {
                    i += 1;
                }
                if i < self.toks.len() {
                    self.out.kind = StmtKind::Select;
                    self.i = i;
                    self.query(root, i, false);
                }
            }
            _ => {
                if self.is_punct(0, "(") {
                    self.out.kind = StmtKind::Select;
                    self.query(root, 0, false);
                }
            }
        }
    }

    /// `INSERT [INTO] t [(cols)] {VALUES … | SELECT … | SET …}`
    fn insert(&mut self, root: usize) {
        self.out.kind = StmtKind::Insert;
        let mut i = 1;
        while i < self.toks.len() && !self.is_word(i, "into") && self.word(i).is_some() {
            // `INSERT IGNORE`, `INSERT OR REPLACE`, `REPLACE`
            i += 1;
        }
        if self.is_word(i, "into") {
            i += 1;
        }
        self.mark(self.pos(i), root, Clause::Target);
        i = self.target_source(root, i);
        let mut info = InsertInfo {
            scope: root,
            columns: Vec::new(),
            rows: Vec::new(),
        };
        if self.is_punct(i, "(") && !self.starts_query(i + 1) {
            self.mark(self.pos(i + 1), root, Clause::InsertColumns);
            i += 1;
            while i < self.toks.len() && !self.is_punct(i, ")") {
                if let Some((parts, trailing, next)) = self.qualified(i) {
                    if let Some(last) = parts.last() {
                        info.columns.push(last.clone());
                    }
                    self.push_ref(parts, root, Role::TargetColumn, trailing);
                    i = next;
                } else {
                    i += 1;
                }
            }
            i += 1;
            self.mark(self.pos(i), root, Clause::Other);
        }
        self.i = i;
        while self.i < self.toks.len() {
            let w = self.word(self.i).unwrap_or_default();
            match w.as_str() {
                "values" | "value" => {
                    self.mark(self.pos(self.i), root, Clause::Values);
                    self.i += 1;
                    self.values_rows(root, &mut info);
                }
                "select" | "with" => {
                    // Its own scope: the insert target is not visible.
                    let s = self.new_scope(None, self.pos(self.i), false);
                    let start = self.i;
                    self.query(s, start, false);
                }
                "set" => {
                    self.mark(self.pos(self.i), root, Clause::Set);
                    self.i += 1;
                    self.set_list(root);
                }
                "on" => {
                    // `ON CONFLICT (…) DO UPDATE SET …` / `ON DUPLICATE KEY UPDATE …`
                    if let Some(SourceKind::Table(parts)) =
                        self.out.scopes[root].sources.first().map(|s| s.kind.clone())
                    {
                        let excluded = Ident {
                            text: "excluded".into(),
                            quoted: false,
                            range: 0..0,
                        };
                        self.out.scopes[root].sources.push(Source {
                            kind: SourceKind::Table(parts),
                            alias: Some(excluded),
                            column_aliases: Vec::new(),
                            range: 0..0,
                            joined: false,
                            target: false,
                        });
                    }
                    self.mark(self.pos(self.i), root, Clause::Set);
                    self.i += 1;
                    self.expressions(root, Clause::Set);
                }
                "returning" => {
                    self.mark(self.pos(self.i), root, Clause::Returning);
                    self.i += 1;
                    self.expressions(root, Clause::Returning);
                }
                _ => self.i += 1,
            }
        }
        self.out.insert = Some(info);
    }

    fn values_rows(&mut self, scope: usize, info: &mut InsertInfo) {
        while self.is_punct(self.i, "(") {
            let open = self.i;
            let mut count = 0;
            let mut depth = 0;
            let mut any = false;
            self.i += 1;
            while self.i < self.toks.len() {
                if self.is_punct(self.i, "(") {
                    depth += 1;
                } else if self.is_punct(self.i, ")") {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                } else if self.is_punct(self.i, ",") && depth == 0 {
                    count += 1;
                    self.i += 1;
                    continue;
                } else if self.starts_query(self.i) && self.is_punct(self.i - 1, "(") {
                    let s = self.new_scope(Some(scope), self.pos(self.i), false);
                    let start = self.i;
                    self.query(s, start, true);
                    any = true;
                    continue;
                }
                any = true;
                self.expression_token(scope, Clause::Values);
            }
            let close = self.i.min(self.toks.len().saturating_sub(1));
            let end = self.tok(close).map_or(self.end_pos(), |t| t.range.end);
            if self.is_punct(self.i, ")") {
                info.rows
                    .push((if any { count + 1 } else { 0 }, self.pos(open)..end));
            }
            self.i += 1;
            if self.is_punct(self.i, ",") {
                self.i += 1;
            } else {
                break;
            }
        }
    }

    /// `UPDATE t [AS a] SET … [FROM …] [WHERE …]`
    fn update(&mut self, root: usize) {
        self.out.kind = StmtKind::Update;
        let mut i = 1;
        while self.is_word(i, "only") || self.is_word(i, "low_priority") || self.is_word(i, "ignore")
        {
            i += 1;
        }
        self.mark(self.pos(i), root, Clause::Target);
        i = self.target_source(root, i);
        self.i = i;
        // MySQL multi-table `UPDATE a JOIN b ON …`: reuse the query loop.
        let mut st = QState {
            scope: root,
            clause: Clause::Target,
            expect_source: false,
            item_start: None,
            item_refs: 0,
            in_extract: false,
        };
        self.query_loop(&mut st, false);
    }

    /// `DELETE FROM t [USING …] [WHERE …]`
    fn delete(&mut self, root: usize) {
        self.out.kind = StmtKind::Delete;
        let mut i = 1;
        while i < self.toks.len() && !self.is_word(i, "from") {
            i += 1;
        }
        i += 1;
        self.mark(self.pos(i), root, Clause::Target);
        i = self.target_source(root, i);
        self.i = i;
        let mut st = QState {
            scope: root,
            clause: Clause::Target,
            expect_source: false,
            item_start: None,
            item_refs: 0,
            in_extract: false,
        };
        self.query_loop(&mut st, false);
    }

    /// The single target table of a DML statement at `i`.
    fn target_source(&mut self, scope: usize, i: usize) -> usize {
        let Some((parts, trailing, mut next)) = self.qualified(i) else {
            return i;
        };
        let start = parts[0].range.start;
        let sidx = self.out.scopes[scope].sources.len();
        self.push_ref(parts.clone(), scope, Role::Table { source: sidx }, trailing);
        let mut alias = None;
        if self.is_word(next, "as") {
            next += 1;
        }
        if let Some(a) = self.alias_at(next) {
            alias = Some(a);
            next += 1;
        }
        let end = self.tok(next.saturating_sub(1)).map_or(start, |t| t.range.end);
        self.out.scopes[scope].sources.push(Source {
            kind: SourceKind::Table(parts),
            alias,
            column_aliases: Vec::new(),
            range: start..end,
            joined: false,
            target: true,
        });
        next
    }

    fn alias_at(&self, i: usize) -> Option<Ident> {
        let id = self.ident(i)?;
        if !id.quoted && NOT_ALIAS.iter().any(|w| id.text.eq_ignore_ascii_case(w)) {
            return None;
        }
        if self.is_punct(i + 1, ".") || self.is_punct(i + 1, "(") {
            return None;
        }
        Some(id)
    }

    /// `CREATE [OR REPLACE] [TEMP] {TABLE|VIEW|INDEX} …`
    fn create(&mut self, root: usize) {
        let mut i = 1;
        let mut what = String::new();
        while let Some(w) = self.word(i) {
            i += 1;
            if matches!(w.as_str(), "table" | "view" | "index") {
                what = w;
                break;
            }
            if matches!(
                w.as_str(),
                "function" | "procedure" | "trigger" | "schema" | "database" | "sequence"
                    | "type" | "extension" | "event" | "role" | "user" | "domain" | "policy"
            ) {
                self.out.kind = StmtKind::Ddl;
                return;
            }
        }
        // IF NOT EXISTS
        while self.is_word(i, "if") || self.is_word(i, "not") || self.is_word(i, "exists") {
            i += 1;
        }
        match what.as_str() {
            "table" => {
                self.out.kind = StmtKind::CreateTable;
                let Some((name, _, mut next)) = self.qualified(i) else {
                    return;
                };
                let mut columns = Vec::new();
                if self.is_punct(next, "(") {
                    next += 1;
                    let mut depth = 0;
                    let mut item_start = true;
                    while next < self.toks.len() {
                        if self.is_punct(next, "(") {
                            depth += 1;
                        } else if self.is_punct(next, ")") {
                            if depth == 0 {
                                break;
                            }
                            depth -= 1;
                        } else if depth == 0 && self.is_punct(next, ",") {
                            item_start = true;
                            next += 1;
                            continue;
                        } else if item_start {
                            item_start = false;
                            let w = self.word(next).unwrap_or_default();
                            let constraint = matches!(
                                w.as_str(),
                                "constraint" | "primary" | "unique" | "foreign" | "check"
                                    | "key" | "index" | "exclude" | "like" | "fulltext"
                                    | "spatial"
                            );
                            if !constraint && let Some(c) = self.ident(next) {
                                columns.push(c);
                            }
                        } else if self.is_word(next, "references") {
                            if let Some((parts, trailing, n)) = self.qualified(next + 1) {
                                self.push_ref(parts, root, Role::DdlTable, trailing);
                                next = n;
                                continue;
                            }
                        }
                        next += 1;
                    }
                    next += 1;
                }
                let mut query = None;
                if self.is_word(next, "as") {
                    let s = self.new_scope(None, self.pos(next + 1), false);
                    self.query(s, next + 1, false);
                    query = Some(s);
                }
                self.out.created = Some(Created {
                    name,
                    columns,
                    view: false,
                    query,
                });
            }
            "view" => {
                self.out.kind = StmtKind::CreateView;
                let Some((name, _, mut next)) = self.qualified(i) else {
                    return;
                };
                let mut columns = Vec::new();
                if self.is_punct(next, "(") {
                    next += 1;
                    while next < self.toks.len() && !self.is_punct(next, ")") {
                        if let Some(c) = self.ident(next) {
                            columns.push(c);
                        }
                        next += 1;
                    }
                    next += 1;
                }
                while next < self.toks.len() && !self.is_word(next, "as") {
                    next += 1;
                }
                let mut query = None;
                if next < self.toks.len() {
                    let s = self.new_scope(None, self.pos(next + 1), false);
                    self.query(s, next + 1, false);
                    query = Some(s);
                }
                self.out.created = Some(Created {
                    name,
                    columns,
                    view: true,
                    query,
                });
            }
            "index" => {
                self.out.kind = StmtKind::CreateIndex;
                while i < self.toks.len() && !self.is_word(i, "on") {
                    i += 1;
                }
                i += 1;
                if self.is_word(i, "only") {
                    i += 1;
                }
                let next = self.target_source(root, i);
                self.i = next;
                self.mark(self.pos(next), root, Clause::Other);
                // Columns and expressions of the index.
                while self.i < self.toks.len() {
                    if self.is_word(self.i, "where") {
                        self.mark(self.pos(self.i), root, Clause::Where);
                    }
                    if self.is_word(self.i, "using") || self.is_word(self.i, "with") {
                        self.i += 1;
                        continue;
                    }
                    self.expression_token(root, Clause::Other);
                }
            }
            _ => self.out.kind = StmtKind::Ddl,
        }
    }

    /// `ALTER TABLE x …`, `DROP TABLE x, y`, `TRUNCATE x`: table names only.
    fn ddl(&mut self, root: usize) {
        self.out.kind = StmtKind::Ddl;
        let mut i = 1;
        let mut tables = false;
        while i < self.toks.len() {
            let w = self.word(i).unwrap_or_default();
            if matches!(w.as_str(), "table" | "view") || (i == 1 && self.is_word(0, "truncate")) {
                tables = true;
                if matches!(w.as_str(), "table" | "view") {
                    i += 1;
                }
                while self.is_word(i, "if") || self.is_word(i, "exists") || self.is_word(i, "only")
                {
                    i += 1;
                }
                while let Some((parts, trailing, next)) = self.qualified(i) {
                    self.push_ref(parts, root, Role::DdlTable, trailing);
                    i = next;
                    if self.is_punct(i, ",") {
                        i += 1;
                    } else {
                        break;
                    }
                }
                break;
            }
            i += 1;
        }
        let _ = tables;
    }

    fn starts_query(&self, i: usize) -> bool {
        self.is_word(i, "select") || self.is_word(i, "with") || self.is_word(i, "values")
    }

    // ---------------------------------------------------------------- query

    /// Parses a query starting at token `start` into `scope`. With
    /// `nested`, stops at the `)` closing the subquery (not consumed).
    fn query(&mut self, scope: usize, start: usize, nested: bool) {
        self.i = start;
        let mut st = QState {
            scope,
            clause: Clause::None,
            expect_source: false,
            item_start: None,
            item_refs: 0,
            in_extract: false,
        };
        self.query_loop(&mut st, nested);
        self.finish_item(&mut st);
        let end = self.tok(self.i.saturating_sub(1)).map_or(self.end_pos(), |t| t.range.end);
        if nested {
            self.out.scopes[scope].range.end = end;
        }
    }

    fn set_clause(&mut self, st: &mut QState, clause: Clause, at: usize) {
        if st.clause == Clause::Select {
            self.finish_item(st);
        }
        st.clause = clause;
        self.mark(self.pos(at), st.scope, clause);
    }

    fn query_loop(&mut self, st: &mut QState, nested: bool) {
        // Parenthesis depth inside this scope (non-subquery parens) with
        // the clause to restore when they close.
        let mut stack: Vec<Clause> = Vec::new();
        while self.i < self.toks.len() {
            let i = self.i;
            let t = &self.toks[i];
            let txt = t.text(self.src);
            if t.kind == TokenKind::Punct {
                match txt {
                    "(" => {
                        if self.starts_query(i + 1) {
                            // Subquery: a source in FROM/JOIN, else an expression.
                            let parent = Some(st.scope);
                            let s = self.new_scope(parent, self.pos(i + 1), false);
                            let as_source = st.expect_source;
                            self.query(s, i + 1, true);
                            let close = self.i;
                            self.i = close + 1;
                            let end_pos = self.pos(close + 1);
                            self.mark(end_pos, st.scope, st.clause);
                            if as_source {
                                self.after_source(st, SourceKind::Subquery(s), self.pos(i));
                            }
                            continue;
                        }
                        if st.expect_source {
                            // `FROM (a JOIN b)`: keep expecting a source.
                            stack.push(st.clause);
                            self.i += 1;
                            continue;
                        }
                        stack.push(st.clause);
                        self.i += 1;
                        continue;
                    }
                    ")" => {
                        if let Some(c) = stack.pop() {
                            if c != st.clause {
                                st.clause = c;
                                self.mark(t.range.end, st.scope, c);
                            }
                            self.i += 1;
                            continue;
                        }
                        if nested {
                            return;
                        }
                        self.i += 1;
                        continue;
                    }
                    "," => {
                        if stack.is_empty() {
                            match st.clause {
                                Clause::From | Clause::Join => st.expect_source = true,
                                Clause::Select => {
                                    self.finish_item(st);
                                    st.item_start = Some(i + 1);
                                    st.item_refs = self.out.refs.len();
                                }
                                _ => {}
                            }
                        }
                        self.i += 1;
                        continue;
                    }
                    ";" => {
                        self.i += 1;
                        continue;
                    }
                    "*" if st.clause == Clause::Select && stack.is_empty() => {
                        let prev_item_start = st.item_start.is_none() || st.item_start == Some(i);
                        if prev_item_start {
                            self.out.scopes[st.scope].outputs.push(Output {
                                alias: None,
                                column_ref: None,
                                star: Some(None),
                                range: t.range.clone(),
                            });
                            st.item_start = None;
                        }
                        self.i += 1;
                        continue;
                    }
                    _ => {
                        self.check_null_comparison(i);
                        self.i += 1;
                        continue;
                    }
                }
            }
            if t.kind == TokenKind::Word {
                let w = txt.to_ascii_lowercase();
                if self.clause_keyword(st, &w, i, stack.len(), nested) {
                    if self.i == i {
                        self.i += 1;
                    }
                    continue;
                }
            }
            if st.expect_source {
                if self.is_word(i, "lateral") || self.is_word(i, "only") {
                    self.i += 1;
                    continue;
                }
                if let Some((parts, trailing, next)) = self.qualified(i) {
                    let start = parts[0].range.start;
                    if self.is_punct(next, "(") {
                        // Table function: arguments are expressions.
                        let f = parts.last().cloned().expect("non-empty");
                        self.push_ref(parts, st.scope, Role::Function, false);
                        self.i = next;
                        st.expect_source = false;
                        let depth_before = stack.len();
                        // Consume the argument list.
                        let mut depth = 0;
                        while self.i < self.toks.len() {
                            if self.is_punct(self.i, "(") {
                                depth += 1;
                            } else if self.is_punct(self.i, ")") {
                                depth -= 1;
                                if depth == 0 {
                                    self.i += 1;
                                    break;
                                }
                            } else {
                                self.expression_token(st.scope, Clause::From);
                                continue;
                            }
                            self.i += 1;
                        }
                        let _ = depth_before;
                        self.after_source(st, SourceKind::Function(f), start);
                    } else {
                        let sidx = self.out.scopes[st.scope].sources.len();
                        self.push_ref(parts.clone(), st.scope, Role::Table { source: sidx }, trailing);
                        self.i = next;
                        self.after_source(st, SourceKind::Table(parts), start);
                    }
                    continue;
                }
                st.expect_source = false;
            }
            if st.clause == Clause::Select && st.item_start.is_none() {
                st.item_start = Some(i);
                st.item_refs = self.out.refs.len();
            }
            let clause = st.clause;
            self.expression_token(st.scope, clause);
        }
    }

    /// Handles clause keywords; `true` when `w` was one.
    fn clause_keyword(
        &mut self,
        st: &mut QState,
        w: &str,
        i: usize,
        depth: usize,
        nested: bool,
    ) -> bool {
        let _ = nested;
        let top = depth == 0;
        match w {
            "with" if top && st.clause == Clause::None => {
                self.set_clause(st, Clause::With, i);
                self.i = i + 1;
                self.ctes(st.scope);
                true
            }
            "select" if top => {
                if st.clause != Clause::None && st.clause != Clause::With {
                    // `INSERT … SELECT` inside UPDATE etc.: treat as new item.
                }
                self.set_clause(st, Clause::Select, i);
                self.i = i + 1;
                // DISTINCT [ON (…)], ALL, TOP n
                while self.is_word(self.i, "distinct") || self.is_word(self.i, "all") {
                    self.i += 1;
                }
                st.item_start = None;
                st.item_refs = self.out.refs.len();
                true
            }
            "values" if top && matches!(st.clause, Clause::None | Clause::With) => {
                self.set_clause(st, Clause::Values, i);
                true
            }
            "from" if top && !st.in_extract => {
                if self.word(i.wrapping_sub(1)).is_some_and(|p| p == "distinct" || p == "is") {
                    return false;
                }
                self.set_clause(st, Clause::From, i);
                st.expect_source = true;
                true
            }
            "join" | "straight_join" if top => {
                self.set_clause(st, Clause::Join, i);
                st.expect_source = true;
                true
            }
            "on" if top && matches!(st.clause, Clause::From | Clause::Join) => {
                self.set_clause(st, Clause::On, i);
                true
            }
            "using" if top && matches!(st.clause, Clause::From | Clause::Join | Clause::Target) => {
                if self.out.kind == StmtKind::Delete && st.clause == Clause::Target {
                    // `DELETE FROM t USING a, b`
                    self.set_clause(st, Clause::From, i);
                    st.expect_source = true;
                    return true;
                }
                self.set_clause(st, Clause::Using, i);
                true
            }
            "where" if top => {
                self.set_clause(st, Clause::Where, i);
                if st.scope == 0 {
                    self.out.has_where = true;
                }
                true
            }
            "group" | "order" if top && self.is_word(i + 1, "by") => {
                let c = if w == "group" {
                    Clause::GroupBy
                } else {
                    Clause::OrderBy
                };
                self.set_clause(st, c, i);
                self.i = i + 2;
                true
            }
            "having" if top => {
                self.set_clause(st, Clause::Having, i);
                true
            }
            "limit" | "offset" | "fetch" if top => {
                self.set_clause(st, Clause::Limit, i);
                true
            }
            "window" if top && !self.is_punct(i + 1, "(") => {
                self.set_clause(st, Clause::Window, i);
                true
            }
            "set" if top && self.out.kind == StmtKind::Update && st.scope == 0 => {
                self.set_clause(st, Clause::Set, i);
                self.i = i + 1;
                self.set_list(st.scope);
                true
            }
            "returning" if top => {
                self.set_clause(st, Clause::Returning, i);
                true
            }
            "union" | "intersect" | "except" | "minus" if top => {
                self.finish_item(st);
                // Next branch: a sibling scope sharing the CTEs.
                let owner = self.out.scopes[st.scope].cte_owner.unwrap_or(st.scope);
                let parent = self.out.scopes[st.scope].parent;
                let cte_body = self.out.scopes[st.scope].cte_body;
                let s = self.new_scope(parent, self.pos(i + 1), cte_body);
                self.out.scopes[s].cte_owner = Some(owner);
                let end = self.pos(i);
                self.out.scopes[st.scope].range.end = end;
                st.scope = s;
                st.clause = Clause::None;
                self.mark(self.pos(i), s, Clause::None);
                self.i = i + 1;
                while self.is_word(self.i, "all") || self.is_word(self.i, "distinct") {
                    self.i += 1;
                }
                true
            }
            "for" if top && matches!(st.clause, Clause::Where | Clause::Limit | Clause::OrderBy | Clause::From | Clause::Join) => {
                // FOR UPDATE / SHARE
                self.set_clause(st, Clause::Other, i);
                true
            }
            _ => false,
        }
    }

    /// After a source's name/subquery: alias and column aliases.
    fn after_source(&mut self, st: &mut QState, kind: SourceKind, start: usize) {
        st.expect_source = false;
        let mut i = self.i;
        if self.is_word(i, "as") {
            i += 1;
        }
        let mut alias = None;
        let mut column_aliases = Vec::new();
        if let Some(a) = self.alias_at(i) {
            alias = Some(a);
            i += 1;
            if self.is_punct(i, "(") && !self.starts_query(i + 1) {
                let mut j = i + 1;
                while j < self.toks.len() && !self.is_punct(j, ")") {
                    if let Some(c) = self.ident(j) {
                        column_aliases.push(c);
                    }
                    j += 1;
                }
                i = j + 1;
            }
        }
        let end = self
            .tok(i.saturating_sub(1))
            .map_or(start, |t| t.range.end)
            .max(start);
        let joined = st.clause == Clause::Join;
        self.out.scopes[st.scope].sources.push(Source {
            kind,
            alias,
            column_aliases,
            range: start..end,
            joined,
            target: false,
        });
        self.i = i;
    }

    /// `name [(cols)] AS [NOT MATERIALIZED] (query), …` after `WITH`.
    fn ctes(&mut self, scope: usize) {
        if self.is_word(self.i, "recursive") {
            self.i += 1;
        }
        loop {
            let Some(name) = self.ident(self.i) else { return };
            let start = name.range.start;
            self.i += 1;
            let mut columns = Vec::new();
            if self.is_punct(self.i, "(") {
                self.i += 1;
                while self.i < self.toks.len() && !self.is_punct(self.i, ")") {
                    if let Some(c) = self.ident(self.i) {
                        columns.push(c);
                    }
                    self.i += 1;
                }
                self.i += 1;
            }
            if !self.is_word(self.i, "as") {
                return;
            }
            self.i += 1;
            while self.is_word(self.i, "not") || self.is_word(self.i, "materialized") {
                self.i += 1;
            }
            if !self.is_punct(self.i, "(") {
                return;
            }
            let body = self.new_scope(Some(scope), self.pos(self.i + 1), true);
            // Recursive CTEs see themselves: register before the body.
            let idx = self.out.scopes[scope].ctes.len();
            let end = self.end_pos();
            self.out.scopes[scope].ctes.push(Cte {
                name,
                columns,
                scope: body,
                range: start..end,
            });
            let open = self.i;
            self.query(body, open + 1, true);
            let end = self.tok(self.i).map_or(self.end_pos(), |t| t.range.end);
            self.out.scopes[scope].ctes[idx].range = start..end;
            self.i += 1;
            self.mark(self.pos(self.i), scope, Clause::With);
            if self.is_punct(self.i, ",") {
                self.i += 1;
            } else {
                return;
            }
        }
    }

    /// `a = expr, b = expr` (UPDATE SET, INSERT SET, ON DUPLICATE KEY UPDATE).
    fn set_list(&mut self, scope: usize) {
        let mut item_start = true;
        let mut depth = 0;
        while self.i < self.toks.len() {
            let w = self.word(self.i).unwrap_or_default();
            if depth == 0 && matches!(w.as_str(), "from" | "where" | "returning" | "on" | "order" | "limit")
            {
                return;
            }
            if self.is_punct(self.i, "(") {
                if self.starts_query(self.i + 1) {
                    let s = self.new_scope(Some(scope), self.pos(self.i + 1), false);
                    let start = self.i + 1;
                    self.query(s, start, true);
                    self.i += 1;
                    continue;
                }
                depth += 1;
            } else if self.is_punct(self.i, ")") {
                depth -= 1;
            } else if depth == 0 && self.is_punct(self.i, ",") {
                item_start = true;
                self.i += 1;
                continue;
            }
            if item_start && let Some((parts, trailing, next)) = self.qualified(self.i)
                && self.is_punct(next, "=")
            {
                self.push_ref(parts, scope, Role::TargetColumn, trailing);
                self.i = next;
                item_start = false;
                continue;
            }
            item_start = false;
            if self.is_punct(self.i, "(") || self.is_punct(self.i, ")") {
                self.i += 1;
                continue;
            }
            self.expression_token(scope, Clause::Set);
        }
    }

    /// Expressions until the end (INSERT tail clauses).
    fn expressions(&mut self, scope: usize, clause: Clause) {
        while self.i < self.toks.len() {
            if self.is_word(self.i, "returning") {
                return;
            }
            if self.is_word(self.i, "update") || self.is_word(self.i, "set") {
                self.i += 1;
                self.set_list(scope);
                continue;
            }
            self.expression_token(scope, clause);
        }
    }

    /// One token of an expression: records name references.
    fn expression_token(&mut self, scope: usize, clause: Clause) {
        let i = self.i;
        let Some(t) = self.tok(i) else { return };
        if t.kind == TokenKind::Punct {
            self.check_null_comparison(i);
            self.i += 1;
            return;
        }
        if !matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent) {
            self.i += 1;
            return;
        }
        let Some((parts, trailing, next)) = self.qualified(i) else {
            self.i += 1;
            return;
        };
        self.i = next;
        if self.skip_as_name(i, &parts, next) {
            return;
        }
        // `t.*`
        if self.is_punct(next, ".") && self.is_punct(next + 1, "*") {
            self.push_ref(parts, scope, Role::Star, false);
            self.i = next + 2;
            return;
        }
        let role = if self.is_punct(next, "(") {
            Role::Function
        } else {
            match clause {
                Clause::OrderBy | Clause::GroupBy | Clause::Having => Role::SortColumn,
                _ => Role::Column,
            }
        };
        self.push_ref(parts, scope, role, trailing);
    }

    /// Words in expression position that are not names.
    fn skip_as_name(&self, i: usize, parts: &[Ident], next: usize) -> bool {
        if parts.len() > 1 || parts[0].quoted {
            return false;
        }
        let w = parts[0].text.to_ascii_lowercase();
        let prev = i.checked_sub(1);
        // Casts `x::int`, typed literals `date '…'`, MySQL `@var`, named args.
        if prev.is_some_and(|p| self.is_punct(p, "::") || self.is_punct(p, "@")) {
            return true;
        }
        if self.tok(next).is_some_and(|t| t.kind == TokenKind::String) {
            return true;
        }
        if self.is_punct(next, "=") && self.is_punct(next + 1, ">") {
            return true;
        }
        // `CAST(x AS int)`: the type after AS inside parentheses.
        if prev.is_some_and(|p| self.is_word(p, "as")) {
            return true;
        }
        // `OVER w`
        if prev.is_some_and(|p| self.is_word(p, "over")) {
            return true;
        }
        // `EXTRACT(year FROM …)`, `INTERVAL 1 DAY`, `TRIM(LEADING …)`
        if self.is_word(next, "from") && prev.is_some_and(|p| self.is_punct(p, "(")) {
            return true;
        }
        if prev.is_some_and(|p| self.is_word(p, "interval")) {
            return true;
        }
        // Soft keyword phrases: `NULLS FIRST`, `ROWS UNBOUNDED PRECEDING`…
        if PHRASE_WORDS.contains(&w.as_str()) || self.d.is_soft_keyword(&w) {
            let prev_kw = prev.and_then(|p| self.word(p)).is_some_and(|pw| {
                (self.d.is_keyword(&pw) || self.d.is_soft_keyword(&pw) || PHRASE_WORDS.contains(&pw.as_str()))
                    && !EXPR_START.contains(&pw.as_str())
            });
            let next_kw = self.word(next).is_some_and(|nw| {
                self.d.is_keyword(&nw) || PHRASE_WORDS.contains(&nw.as_str())
            });
            let prev_ident_and_phrase =
                PHRASE_WORDS.contains(&w.as_str()) && prev.is_some_and(|p| self.ident(p).is_some() || self.is_punct(p, ")"));
            if prev_kw || (next_kw && PHRASE_WORDS.contains(&w.as_str())) || prev_ident_and_phrase {
                return true;
            }
        }
        false
    }

    fn finish_item(&mut self, st: &mut QState) {
        let Some(start) = st.item_start.take() else {
            return;
        };
        let end = self.i.min(self.toks.len());
        if start >= end {
            return;
        }
        // Trim a trailing `,` / clause keyword already consumed.
        let mut last = end - 1;
        while last > start && (self.is_punct(last, ",") || self.word(last).is_some_and(|w| w == "from")) {
            last -= 1;
        }
        let range = self.toks[start].range.start..self.toks[last].range.end;
        let refs_in = st.item_refs..self.out.refs.len();
        let mut alias = None;
        // `expr AS alias` / `expr alias`
        if last > start
            && let Some(a) = self.ident(last)
        {
            let before = last - 1;
            let explicit = self.is_word(before, "as");
            let implicit = !explicit
                && !self.is_punct(before, ".")
                && !self.is_punct(before, "::")
                && (self.tok(before).is_some_and(|t| {
                    matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent | TokenKind::String | TokenKind::Number)
                        && !self.word(before).is_some_and(|w| self.d.is_keyword(&w) && !matches!(w.as_str(), "end" | "null" | "true" | "false"))
                }) || self.is_punct(before, ")")
                    || self.is_punct(before, "*"));
            if explicit || implicit {
                // The alias was recorded as a column reference: drop it.
                if let Some(pos) = self.out.refs[refs_in.clone()]
                    .iter()
                    .rposition(|r| r.parts.len() == 1 && r.parts[0].range == a.range)
                {
                    self.out.refs.remove(refs_in.start + pos);
                }
                alias = Some(a);
            }
        }
        let refs_now = st.item_refs..self.out.refs.len();
        let single_ref = (refs_now.len() == 1)
            .then_some(refs_now.start)
            .filter(|&r| {
                let rr = self.out.refs[r].range();
                matches!(self.out.refs[r].role, Role::Column)
                    && rr.start == range.start
                    && (alias.is_some() || rr.end == range.end)
                    && (alias.is_none() || self.toks[start + 1..=last].iter().take_while(|t| t.range.end <= rr.end).count() > 0)
            });
        let star = self.out.refs[refs_now.clone()]
            .iter()
            .find(|r| r.role == Role::Star && r.range().start == range.start)
            .map(|r| r.parts.last().cloned());
        // `x AS y` where x is not a plain column keeps the alias only.
        let column_ref = single_ref.filter(|&r| {
            let rr = self.out.refs[r].range();
            match &alias {
                Some(a) => {
                    let between = &self.src[rr.end..a.range.start];
                    between.trim().is_empty() || between.trim().eq_ignore_ascii_case("as")
                }
                None => true,
            }
        });
        self.out.scopes[st.scope].outputs.push(Output {
            alias,
            column_ref,
            star,
            range,
        });
    }

    /// `x = NULL`, `x <> NULL`, `x != NULL`.
    fn check_null_comparison(&mut self, i: usize) {
        let op_start = i;
        let (op_end, negated) = if self.is_punct(i, "=") {
            // Not part of `<=`, `>=`, `!=`.
            if i > 0
                && (self.is_punct(i - 1, "<") || self.is_punct(i - 1, ">") || self.is_punct(i - 1, "!") || self.is_punct(i - 1, ":"))
                && self.toks[i - 1].range.end == self.toks[i].range.start
            {
                return;
            }
            (i, false)
        } else if (self.is_punct(i, "<") && self.is_punct(i + 1, ">"))
            || (self.is_punct(i, "!") && self.is_punct(i + 1, "="))
        {
            if self.toks[i].range.end != self.toks[i + 1].range.start {
                return;
            }
            (i + 1, true)
        } else {
            return;
        };
        if !self.is_word(op_end + 1, "null") {
            return;
        }
        // `SET x = NULL` is an assignment.
        let mut j = op_start;
        while j > 0 {
            j -= 1;
            if self.is_word(j, "set") || self.is_punct(j, ",") && self.out.kind == StmtKind::Update {
                if self.is_word(j, "set") || self.is_punct(j, ",") {
                    // Assignment if only a name sits between.
                    let between = (j + 1..op_start).all(|k| {
                        self.ident(k).is_some() || self.is_punct(k, ".")
                    });
                    if between {
                        return;
                    }
                }
                break;
            }
            if self.word(j).is_some_and(|w| matches!(w.as_str(), "where" | "on" | "having" | "when" | "and" | "or")) {
                break;
            }
        }
        if self.is_word(op_start.wrapping_sub(1), "is") {
            return;
        }
        let op = self.toks[op_start].range.start..self.toks[op_end].range.end;
        let null = self.toks[op_end + 1].range.clone();
        self.out.null_comparisons.push((op, null, negated));
    }
}

/// Text of a quoted identifier without quotes.
pub fn unquote(raw: &str) -> String {
    let mut chars = raw.chars();
    let Some(open) = chars.next() else {
        return String::new();
    };
    let close = match open {
        '[' => ']',
        c => c,
    };
    let inner = &raw[open.len_utf8()..];
    let inner = inner.strip_suffix(close).unwrap_or(inner);
    let doubled = format!("{close}{close}");
    inner.replace(&doubled, &close.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sql::lexer::tokenize;

    fn parse_sql(sql: &str, dialect: &str) -> Parsed {
        let d = Dialect::get(dialect);
        let toks: Vec<Token> = tokenize(sql, &d.flavor)
            .into_iter()
            .filter(|t| !t.is_trivia())
            .collect();
        parse(sql, &toks, d)
    }

    fn ref_texts(p: &Parsed, sql: &str) -> Vec<(String, Role)> {
        p.refs
            .iter()
            .map(|r| (sql[r.range()].to_owned(), r.role))
            .collect()
    }

    #[test]
    fn sources_aliases_and_refs() {
        let sql = "select o.id, c.name as customer, count(*) n from orders o join public.customers c on c.id = o.customer_id where o.total > 10 order by n";
        let p = parse_sql(sql, "postgresql");
        assert_eq!(p.kind, StmtKind::Select);
        let s = &p.scopes[0];
        assert_eq!(s.sources.len(), 2);
        assert_eq!(s.sources[0].alias.as_ref().unwrap().text, "o");
        assert_eq!(s.sources[1].alias.as_ref().unwrap().text, "c");
        assert!(s.sources[1].joined);
        let names: Vec<_> = s
            .outputs
            .iter()
            .map(|o| o.name(&p.refs).map(|i| i.text.clone()))
            .collect();
        assert_eq!(
            names,
            vec![Some("id".into()), Some("customer".into()), Some("n".into())]
        );
        let refs = ref_texts(&p, sql);
        assert!(refs.contains(&("o.id".into(), Role::Column)));
        assert!(refs.contains(&("count".into(), Role::Function)));
        assert!(refs.contains(&("public.customers".into(), Role::Table { source: 1 })));
        assert!(refs.contains(&("n".into(), Role::SortColumn)));
        assert!(!refs.iter().any(|(t, _)| t == "customer"));
    }

    #[test]
    fn ctes_subqueries_and_unions() {
        let sql = "with big as (select id from orders where total > 100) select x.id from (select id from big) x union all select id from big";
        let p = parse_sql(sql, "postgresql");
        assert_eq!(p.scopes[0].ctes.len(), 1);
        assert_eq!(p.scopes[0].ctes[0].name.text, "big");
        let cte_scope = p.scopes[0].ctes[0].scope;
        assert!(p.scopes[cte_scope].cte_body);
        // The subquery in FROM is a source with alias x.
        assert!(matches!(p.scopes[0].sources[0].kind, SourceKind::Subquery(_)));
        // The union branch shares the CTEs.
        let branch = p.scopes.iter().position(|s| s.cte_owner == Some(0)).unwrap();
        assert_eq!(p.find_cte(branch, "big", Dialect::get("postgresql")), Some((0, 0)));
    }

    #[test]
    fn insert_update_delete() {
        let sql = "insert into orders (id, total) values (1, 2), (3)";
        let p = parse_sql(sql, "postgresql");
        let ins = p.insert.unwrap();
        assert_eq!(ins.columns.len(), 2);
        assert_eq!(ins.rows.iter().map(|r| r.0).collect::<Vec<_>>(), vec![2, 1]);
        let sql = "update orders o set total = o.total + 1, note = null where o.id = 1";
        let p = parse_sql(sql, "postgresql");
        assert!(p.has_where);
        assert!(p.null_comparisons.is_empty());
        let refs = ref_texts(&p, sql);
        assert!(refs.contains(&("total".into(), Role::TargetColumn)));
        assert!(refs.contains(&("o.total".into(), Role::Column)));
        let p = parse_sql("delete from orders", "mysql");
        assert!(!p.has_where);
        assert_eq!(p.scopes[0].sources.len(), 1);
    }

    #[test]
    fn keywords_and_literals_are_not_names() {
        let sql = "select cast(a as int), b::text, date '2020-01-01', extract(year from c) from t order by d nulls first, e desc";
        let p = parse_sql(sql, "postgresql");
        let refs: Vec<String> = ref_texts(&p, sql).into_iter().map(|r| r.0).collect();
        for bad in ["int", "text", "date", "year", "nulls", "first"] {
            assert!(!refs.contains(&bad.to_owned()), "{bad} in {refs:?}");
        }
        for good in ["a", "b", "c", "d", "e"] {
            assert!(refs.contains(&good.to_owned()), "{good} missing");
        }
    }

    #[test]
    fn null_comparisons() {
        let p = parse_sql("select * from t where a = null or b <> null and c is null", "postgresql");
        assert_eq!(p.null_comparisons.len(), 2);
        assert!(!p.null_comparisons[0].2);
        assert!(p.null_comparisons[1].2);
    }

    #[test]
    fn clause_at_offset() {
        let sql = "select a from t where b = 1";
        let p = parse_sql(sql, "postgresql");
        assert_eq!(p.at(7).1, Clause::Select);
        assert_eq!(p.at(14).1, Clause::From);
        assert_eq!(p.at(sql.len()).1, Clause::Where);
    }

    #[test]
    fn create_table_columns() {
        let p = parse_sql(
            "create table tmp_x (id int primary key, name text, constraint fk foreign key (id) references orders(id))",
            "postgresql",
        );
        let c = p.created.unwrap();
        assert_eq!(c.name[0].text, "tmp_x");
        assert_eq!(
            c.columns.iter().map(|c| c.text.as_str()).collect::<Vec<_>>(),
            vec!["id", "name"]
        );
    }

    #[test]
    fn unquote_variants() {
        assert_eq!(unquote("\"a\"\"b\""), "a\"b");
        assert_eq!(unquote("`x`"), "x");
        assert_eq!(unquote("[a]]b]"), "a]b");
        assert_eq!(unquote("\"open"), "open");
    }
}
