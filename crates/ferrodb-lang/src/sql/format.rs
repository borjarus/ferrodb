//! SQL formatter.
//!
//! Only whitespace and keyword case change: the sequence of significant
//! tokens and every comment are kept. Layout:
//! - each clause (`SELECT`, `FROM`, joins, `WHERE`, `GROUP BY`…) on its own
//!   line at the query's indentation;
//! - select lists, `SET` and `RETURNING` lists with several items: one item
//!   per line, indented;
//! - `AND`/`OR` of `WHERE`/`ON`/`HAVING` on their own lines (not the `AND`
//!   of `BETWEEN`);
//! - subqueries and `CREATE TABLE` column lists indented in their
//!   parentheses; other parentheses stay inline;
//! - statements end with `;` and are separated by a line break.
//!
//! The output depends only on the tokens and on where comments sit
//! (own line or not), so formatting twice gives the same text. Routine
//! definitions (`CREATE FUNCTION/PROCEDURE/TRIGGER/EVENT`, `DO`), MySQL
//! `DELIMITER` scripts and statements with lexical errors are left as
//! they are.

use ferrodb_api::FormatStyle;

use super::{
    dialect::Dialect,
    lexer::{Token, TokenKind, tokenize},
    split::split,
};

const OPERATORS: &[&str] = &[
    "->>", "#>>", "!~*", "<=>", "<>", "<=", ">=", "!=", "||", "->", "#>", "@>", "<@", "&&", ":=",
    "=>", "**", "<<", ">>", "!~", "~*", "?|", "?&", "#-", "~~",
];

/// Clause keywords that start a line at the query's indentation.
const CLAUSES: &[&str] = &[
    "select",
    "from",
    "where",
    "having",
    "limit",
    "offset",
    "fetch",
    "union",
    "intersect",
    "except",
    "minus",
    "values",
    "set",
    "returning",
    "window",
    "qualify",
];

/// First words of a join phrase.
const JOIN_WORDS: &[&str] = &[
    "join",
    "left",
    "right",
    "full",
    "inner",
    "cross",
    "natural",
    "straight_join",
];

/// Reserved words used as functions: no space before `(`.
const KEYWORD_FUNCTIONS: &[&str] = &[
    "cast",
    "left",
    "right",
    "replace",
    "char",
    "convert",
    "coalesce",
    "nullif",
    "if",
    "row",
    "array",
    "position",
    "substring",
    "trim",
    "extract",
    "overlay",
    "collate",
    "current_date",
    "current_time",
    "current_timestamp",
    "values",
    "exists",
];

/// Formats a whole script.
pub fn format(text: &str, dialect: &str, style: &FormatStyle) -> String {
    let d = Dialect::get(dialect);
    let toks = tokenize(text, &d.flavor);
    let spans = split(text, &d.flavor);
    if spans.is_empty() {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut pos = 0; // byte position in `text` of the next unhandled char
    let mut ti = 0;
    for (k, span) in spans.iter().enumerate() {
        // Gap before the statement (terminator of the previous one, comments).
        let gap_toks: Vec<&Token> = toks[ti..]
            .iter()
            .take_while(|t| t.range.start < span.body.start)
            .collect();
        ti += gap_toks.len();
        let code_in_gap = gap_toks
            .iter()
            .any(|t| !t.is_trivia() && !(t.kind == TokenKind::Punct && t.text(text) == ";"));
        if code_in_gap {
            // `DELIMITER` commands: keep the gap verbatim.
            out.push_str(&text[pos..span.body.start]);
        } else {
            gap(&mut out, text, &gap_toks, k == 0, k > 0);
        }
        let body_toks: Vec<Token> = toks[ti..]
            .iter()
            .take_while(|t| t.range.start < span.body.end)
            .cloned()
            .collect();
        ti += body_toks.len();
        let body = &text[span.body.clone()];
        if verbatim(text, &body_toks) {
            out.push_str(body);
        } else {
            out.push_str(&Body::new(text, &body_toks, d, style).run());
        }
        pos = span.body.end;
    }
    // Tail: terminator and trailing comments.
    let tail: Vec<&Token> = toks[ti..].iter().collect();
    let code_in_tail = tail
        .iter()
        .any(|t| !t.is_trivia() && !(t.kind == TokenKind::Punct && t.text(text) == ";"));
    if code_in_tail {
        out.push_str(&text[pos..]);
    } else {
        gap(&mut out, text, &tail, false, false);
        if text.ends_with('\n') && !out.ends_with('\n') {
            out.push('\n');
        }
    }
    out
}

/// Whitespace class: 0 = none/spaces, 1 = line break, 2 = blank line.
fn breaks(ws: &str) -> usize {
    ws.matches('\n').count().min(2)
}

/// Emits a gap between statements: `;`, comments, line breaks.
fn gap(out: &mut String, text: &str, toks: &[&Token], first: bool, before_stmt: bool) {
    let mut pending_ws = 0usize;
    let mut after_semicolon = false;
    let mut after_line_comment = false;
    // The terminator goes right after the statement, before any comment;
    // further `;` (empty statements) are dropped.
    let terminated = toks
        .iter()
        .any(|t| t.kind == TokenKind::Punct && t.text(text) == ";");
    if !first && terminated && !out.is_empty() && !out.ends_with(';') {
        out.push(';');
        after_semicolon = true;
    }
    for t in toks {
        match t.kind {
            TokenKind::Whitespace => pending_ws = pending_ws.max(breaks(t.text(text))),
            TokenKind::LineComment | TokenKind::BlockComment => {
                if !out.is_empty() {
                    let ws = if after_line_comment {
                        pending_ws.max(1)
                    } else {
                        pending_ws
                    };
                    sep(out, ws);
                }
                let c = t.text(text);
                out.push_str(if t.kind == TokenKind::LineComment {
                    c.trim_end()
                } else {
                    c
                });
                after_line_comment = t.kind == TokenKind::LineComment;
                pending_ws = 0;
            }
            _ => {}
        }
    }
    if before_stmt || !out.is_empty() {
        if out.is_empty() {
            return;
        }
        if before_stmt {
            let mut ws = pending_ws;
            if after_semicolon || after_line_comment {
                ws = ws.max(1);
            }
            sep(out, ws);
        } else if after_line_comment {
            // Comment before the first statement.
            sep(out, pending_ws.max(1));
        }
    }
}

fn sep(out: &mut String, ws: usize) {
    match ws {
        0 => out.push(' '),
        1 => out.push('\n'),
        _ => out.push_str("\n\n"),
    }
}

/// Statements left as written.
fn verbatim(src: &str, toks: &[Token]) -> bool {
    let words: Vec<String> = toks
        .iter()
        .filter(|t| t.kind == TokenKind::Word)
        .take(8)
        .map(|t| t.text(src).to_ascii_lowercase())
        .collect();
    let routine = words.first().is_some_and(|w| w == "create")
        && words.iter().any(|w| {
            matches!(
                w.as_str(),
                "function" | "procedure" | "trigger" | "event" | "package" | "rule"
            )
        });
    let block = words.first().is_some_and(|w| {
        matches!(
            w.as_str(),
            "do" | "begin" | "declare" | "if" | "while" | "loop"
        )
    }) && toks.len() > 2;
    let broken = toks.iter().any(|t| {
        let s = t.text(src);
        match t.kind {
            TokenKind::String => !(s.ends_with('\'') && s.len() >= 2) && !s.starts_with('$'),
            TokenKind::BlockComment => !s.ends_with("*/"),
            TokenKind::QuotedIdent => s.chars().count() < 2,
            _ => false,
        }
    });
    routine || block || broken
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameKind {
    /// The statement or a subquery: clauses break lines.
    Query,
    /// `CREATE TABLE (…)` column list.
    Columns,
    /// Any other parenthesis.
    Inline,
}

struct Frame {
    kind: FrameKind,
    /// Indentation of the query's clauses.
    base: usize,
    /// Indentation of the line where the frame opened.
    outer: usize,
    /// Clause of the query (lower case).
    clause: String,
    /// The current list clause has several items (one per line).
    list_break: bool,
    /// `BETWEEN` seen: the next `AND` belongs to it.
    between: bool,
}

/// One code element (merged operators) or comment.
struct Item<'a> {
    text: String,
    kind: TokenKind,
    /// Comment preceded by a line break in the source.
    own_line: bool,
    /// Line break after a block comment in the source.
    break_after: bool,
    _src: &'a str,
}

struct Body<'a> {
    src: &'a str,
    items: Vec<Item<'a>>,
    d: &'static Dialect,
    style: &'a FormatStyle,
    out: String,
    indent: usize,
    /// Start of the current line in `out` has no content yet.
    at_line_start: bool,
    frames: Vec<Frame>,
}

impl<'a> Body<'a> {
    fn new(src: &'a str, toks: &'a [Token], d: &'static Dialect, style: &'a FormatStyle) -> Self {
        let mut items: Vec<Item<'a>> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let t = &toks[i];
            if t.kind == TokenKind::Whitespace {
                i += 1;
                continue;
            }
            let prev_ws = i > 0 && toks[i - 1].kind == TokenKind::Whitespace;
            let own_line = i > 0 && prev_ws && toks[i - 1].text(src).contains('\n')
                || i > 0 && toks[i - 1].kind == TokenKind::LineComment;
            let break_after = toks
                .get(i + 1)
                .is_some_and(|n| n.kind == TokenKind::Whitespace && n.text(src).contains('\n'));
            // Merge adjacent operator characters.
            if t.kind == TokenKind::Punct {
                let mut j = i;
                let mut s = String::new();
                while j < toks.len()
                    && toks[j].kind == TokenKind::Punct
                    && (j == i || toks[j].range.start == toks[j - 1].range.end)
                {
                    s.push_str(toks[j].text(src));
                    j += 1;
                }
                let best = OPERATORS
                    .iter()
                    .filter(|op| s.starts_with(**op))
                    .max_by_key(|op| op.len());
                if let Some(op) = best {
                    // Number of tokens forming the operator.
                    let mut n = 0;
                    let mut len = 0;
                    while len < op.len() {
                        len += toks[i + n].text(src).len();
                        n += 1;
                    }
                    items.push(Item {
                        text: (*op).to_owned(),
                        kind: TokenKind::Punct,
                        own_line: false,
                        break_after: false,
                        _src: src,
                    });
                    i += n;
                    continue;
                }
            }
            let text = match t.kind {
                TokenKind::LineComment => t.text(src).trim_end().to_owned(),
                _ => t.text(src).to_owned(),
            };
            items.push(Item {
                text,
                kind: t.kind,
                own_line: own_line
                    && matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment),
                break_after,
                _src: src,
            });
            i += 1;
        }
        Self {
            src,
            items,
            d,
            style,
            out: String::new(),
            indent: 0,
            at_line_start: true,
            frames: vec![Frame {
                kind: FrameKind::Query,
                base: 0,
                outer: 0,
                clause: String::new(),
                list_break: false,
                between: false,
            }],
        }
    }

    fn word(&self, i: usize) -> Option<String> {
        let it = self.items.get(i)?;
        (it.kind == TokenKind::Word).then(|| it.text.to_ascii_lowercase())
    }

    fn is(&self, i: usize, s: &str) -> bool {
        self.items
            .get(i)
            .is_some_and(|it| it.text == s && it.kind == TokenKind::Punct)
    }

    /// Next / previous code item (not a comment).
    fn next_code(&self, i: usize) -> Option<usize> {
        (i + 1..self.items.len()).find(|&k| !is_comment(self.items[k].kind))
    }

    fn prev_code(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&k| !is_comment(self.items[k].kind))
    }

    fn newline(&mut self, indent: usize) {
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.indent = indent;
        self.at_line_start = true;
    }

    fn ensure_line(&mut self, indent: usize) {
        if self.at_line_start {
            self.indent = indent;
        } else {
            self.newline(indent);
        }
    }

    fn put(&mut self, s: &str, space: bool) {
        if self.at_line_start {
            for _ in 0..self.indent * self.style.indent {
                self.out.push(' ');
            }
            self.at_line_start = false;
        } else if space {
            self.out.push(' ');
        }
        self.out.push_str(s);
    }

    fn top(&self) -> &Frame {
        self.frames.last().expect("root frame")
    }

    fn top_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("root frame")
    }

    /// The list clause starting at item `i` (keyword) has a top-level comma.
    fn list_has_comma(&self, i: usize) -> bool {
        let mut depth = 0i32;
        for k in i + 1..self.items.len() {
            let it = &self.items[k];
            if it.kind == TokenKind::Punct {
                match it.text.as_str() {
                    "(" | "[" => depth += 1,
                    ")" | "]" => {
                        depth -= 1;
                        if depth < 0 {
                            return false;
                        }
                    }
                    "," if depth == 0 => return true,
                    _ => {}
                }
            } else if depth == 0
                && let Some(w) = self.word(k)
                && (CLAUSES.contains(&w.as_str()) && w != "set" && w != "values"
                    || matches!(w.as_str(), "group" | "order" | "where" | "from" | "into"))
            {
                return false;
            }
        }
        false
    }

    fn keyword_case(&self, i: usize, w: &str) -> String {
        let after_dot = self.prev_code(i).is_some_and(|p| self.is(p, "."));
        let before_dot = self.next_code(i).is_some_and(|n| self.is(n, "."));
        if !after_dot && !before_dot && self.d.is_keyword(w) {
            if self.style.uppercase_keywords {
                w.to_ascii_uppercase()
            } else {
                w.to_ascii_lowercase()
            }
        } else {
            w.to_owned()
        }
    }

    fn run(mut self) -> String {
        let n = self.items.len();
        let mut i = 0;
        while i < n {
            let kind = self.items[i].kind;
            if is_comment(kind) {
                let (own, brk) = (self.items[i].own_line, self.items[i].break_after);
                let text = self.items[i].text.clone();
                let ind = self.indent;
                if own {
                    self.ensure_line(ind);
                }
                self.put(&text, true);
                if kind == TokenKind::LineComment || brk {
                    self.newline(ind);
                }
                i += 1;
                continue;
            }
            let prev = self.prev_code(i);
            let text = self.items[i].text.clone();
            let word = self.word(i);
            let in_query = self.top().kind == FrameKind::Query;
            let base = self.top().base;

            if let Some(w) = word.as_deref()
                && in_query
                && !prev.is_some_and(|p| self.is(p, "."))
            {
                // Clause starts.
                let join_start = JOIN_WORDS.contains(&w)
                    && !prev
                        .and_then(|p| self.word(p))
                        .is_some_and(|pw| JOIN_WORDS.contains(&pw.as_str()) || pw == "outer")
                    && self.join_follows(i);
                let group_order = matches!(w, "group" | "order")
                    && self
                        .next_code(i)
                        .and_then(|k| self.word(k))
                        .is_some_and(|x| x == "by");
                let set_clause = w == "set"
                    && !prev
                        .and_then(|p| self.word(p))
                        .is_some_and(|p| matches!(p.as_str(), "character" | "char" | "charset"));
                let is_clause = (CLAUSES.contains(&w) && (w != "set" || set_clause))
                    || join_start
                    || group_order
                    || (w == "on" && self.top().clause == "join" && false);
                if is_clause && prev.is_some() {
                    self.ensure_line(base);
                }
                if is_clause {
                    let clause = if join_start {
                        "join".to_owned()
                    } else {
                        w.to_owned()
                    };
                    let list = matches!(clause.as_str(), "select" | "set" | "returning")
                        && self.list_has_comma(i);
                    let f = self.top_mut();
                    f.clause = clause;
                    f.list_break = list;
                    f.between = false;
                    let cased = self.keyword_case(i, &text);
                    self.put(&cased, true);
                    // `SELECT DISTINCT`, `UNION ALL`, `GROUP BY` stay on the line.
                    i += 1;
                    while let Some(nw) = self.word(i) {
                        if matches!(nw.as_str(), "distinct" | "all" | "by")
                            || (group_order && nw == "by")
                        {
                            let c = self.keyword_case(i, &self.items[i].text.clone());
                            self.put(&c, true);
                            i += 1;
                        } else {
                            break;
                        }
                    }
                    if list {
                        self.newline(base + 1);
                    }
                    continue;
                }
                if matches!(w, "and" | "or") {
                    let clause = self.top().clause.clone();
                    if w == "and" && self.top().between {
                        self.top_mut().between = false;
                    } else if matches!(clause.as_str(), "where" | "having" | "join") {
                        self.ensure_line(base + 1);
                    }
                }
                if w == "between" {
                    self.top_mut().between = true;
                }
            }

            if self.items[i].kind == TokenKind::Punct {
                match text.as_str() {
                    "(" => {
                        let space = self.space_before_paren(prev);
                        self.put("(", space);
                        let next_word = self.next_code(i).and_then(|k| self.word(k));
                        let sub = next_word
                            .as_deref()
                            .is_some_and(|w| matches!(w, "select" | "with"));
                        let cols = !sub && self.create_table_paren(i);
                        let outer = self.indent;
                        if sub || cols {
                            let b = self.indent + 1;
                            self.frames.push(Frame {
                                kind: if sub {
                                    FrameKind::Query
                                } else {
                                    FrameKind::Columns
                                },
                                base: b,
                                outer,
                                clause: String::new(),
                                list_break: false,
                                between: false,
                            });
                            self.newline(b);
                        } else {
                            self.frames.push(Frame {
                                kind: FrameKind::Inline,
                                base: self.top().base,
                                outer,
                                clause: self.top().clause.clone(),
                                list_break: false,
                                between: false,
                            });
                        }
                        i += 1;
                        continue;
                    }
                    ")" => {
                        if self.frames.len() > 1 {
                            let f = self.frames.pop().expect("frame");
                            if f.kind != FrameKind::Inline {
                                self.newline(f.outer);
                            }
                        }
                        self.put(")", false);
                        i += 1;
                        continue;
                    }
                    "," => {
                        self.put(",", false);
                        let f = self.top();
                        let at_list = (f.kind == FrameKind::Query && f.list_break)
                            || f.kind == FrameKind::Columns;
                        if at_list {
                            let b = if f.kind == FrameKind::Columns {
                                f.base
                            } else {
                                f.base + 1
                            };
                            self.newline(b);
                        }
                        i += 1;
                        continue;
                    }
                    _ => {}
                }
            }

            let space = self.space_before(i, prev);
            let shown = if word.is_some() {
                self.keyword_case(i, &text)
            } else {
                text.clone()
            };
            self.put(&shown, space);
            i += 1;
        }
        while self.out.ends_with(' ') || self.out.ends_with('\n') {
            self.out.pop();
        }
        self.out
    }

    /// A join phrase starting at `i` ends with `JOIN` (not `LEFT(` …).
    fn join_follows(&self, i: usize) -> bool {
        let mut k = i;
        while let Some(w) = self.word(k) {
            if w == "join" || w == "straight_join" {
                return true;
            }
            if !JOIN_WORDS.contains(&w.as_str()) && w != "outer" {
                return false;
            }
            k = match self.next_code(k) {
                Some(n) => n,
                None => return false,
            };
        }
        false
    }

    /// `CREATE [TEMP] TABLE name (` → column list.
    fn create_table_paren(&self, i: usize) -> bool {
        let Some(name) = self.prev_code(i) else {
            return false;
        };
        if !matches!(
            self.items[name].kind,
            TokenKind::Word | TokenKind::QuotedIdent
        ) {
            return false;
        }
        // Walk back over the (qualified) name and IF NOT EXISTS.
        let mut k = name;
        loop {
            let Some(p) = self.prev_code(k) else {
                return false;
            };
            if self.is(p, ".") {
                k = match self.prev_code(p) {
                    Some(x) => x,
                    None => return false,
                };
                continue;
            }
            match self.word(p).as_deref() {
                Some("table") => {
                    return self
                        .items
                        .iter()
                        .take(p)
                        .filter(|it| it.kind == TokenKind::Word)
                        .any(|it| it.text.eq_ignore_ascii_case("create"));
                }
                Some("exists" | "not" | "if") => k = p,
                _ => return false,
            }
        }
    }

    fn space_before_paren(&self, prev: Option<usize>) -> bool {
        let Some(p) = prev else { return false };
        let it = &self.items[p];
        match it.kind {
            TokenKind::Word => {
                let w = it.text.to_ascii_lowercase();
                if KEYWORD_FUNCTIONS.contains(&w.as_str()) && w != "values" && w != "exists" {
                    return false;
                }
                // Keywords (`IN (`, `AS (`, `VALUES (`), the INSERT target.
                if self.d.is_keyword(&w) {
                    return true;
                }
                self.prev_code(p)
                    .and_then(|pp| self.word(pp))
                    .is_some_and(|pw| matches!(pw.as_str(), "into" | "table" | "exists"))
                    || self.prev_code(p).is_some_and(|pp| self.is(pp, ".")) && self.insert_target(p)
            }
            TokenKind::QuotedIdent => self
                .prev_code(p)
                .and_then(|pp| self.word(pp))
                .is_some_and(|pw| matches!(pw.as_str(), "into" | "table")),
            TokenKind::Punct => !matches!(it.text.as_str(), "(" | "[" | "." | "::"),
            _ => true,
        }
    }

    /// `INTO schema.t (` — `p` is the last name part.
    fn insert_target(&self, p: usize) -> bool {
        let mut k = p;
        while let Some(pp) = self.prev_code(k) {
            if self.is(pp, ".") {
                k = match self.prev_code(pp) {
                    Some(x) => x,
                    None => return false,
                };
                continue;
            }
            return self.word(pp).is_some_and(|w| w == "into" || w == "table");
        }
        false
    }

    fn space_before(&self, i: usize, prev: Option<usize>) -> bool {
        let Some(p) = prev else { return false };
        let cur = &self.items[i];
        let pi = &self.items[p];
        if cur.kind == TokenKind::Punct && matches!(cur.text.as_str(), "." | "::" | "[" | "]" | ";")
        {
            return false;
        }
        if pi.kind == TokenKind::Punct && matches!(pi.text.as_str(), "(" | "." | "::" | "[" | "@") {
            return false;
        }
        // Unary minus / plus: no space after.
        if pi.kind == TokenKind::Punct
            && matches!(pi.text.as_str(), "-" | "+" | "~")
            && (cur.kind != TokenKind::Punct || cur.text == "(")
        {
            let before = self.prev_code(p);
            let unary = match before {
                None => true,
                Some(b) => {
                    let bi = &self.items[b];
                    match bi.kind {
                        TokenKind::Punct => !matches!(bi.text.as_str(), ")" | "]"),
                        TokenKind::Word => self.d.is_keyword(&bi.text),
                        _ => false,
                    }
                }
            };
            if unary {
                return false;
            }
        }
        let _ = self.src;
        true
    }
}

fn is_comment(k: TokenKind) -> bool {
    matches!(k, TokenKind::LineComment | TokenKind::BlockComment)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fmt(s: &str, d: &str) -> String {
        format(s, d, &FormatStyle::default())
    }

    fn significant(s: &str, d: &str) -> Vec<String> {
        let dl = Dialect::get(d);
        tokenize(s, &dl.flavor)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace && t.text(s) != ";")
            .map(|t| {
                let x = t.text(s);
                if t.kind == TokenKind::LineComment {
                    x.trim_end().to_lowercase()
                } else {
                    x.to_lowercase()
                }
            })
            .collect::<Vec<_>>()
            .concat()
            .chars()
            .map(|c| c.to_string())
            .collect()
    }

    #[test]
    fn layout() {
        let out = fmt(
            "select o.id, c.name from orders o left join customers c on c.id=o.customer_id and c.active where o.total>10 and o.created_at between now()-interval '1 day' and now() order by o.id desc limit 5",
            "postgresql",
        );
        assert_eq!(
            out,
            "SELECT\n    o.id,\n    c.name\nFROM orders o\nLEFT JOIN customers c ON c.id = o.customer_id\n    AND c.active\nWHERE o.total > 10\n    AND o.created_at BETWEEN now() - interval '1 day' AND now()\nORDER BY o.id DESC\nLIMIT 5"
        );
    }

    #[test]
    fn subqueries_create_table_and_statements() {
        let out = fmt(
            "-- Top\nselect * from (select id from t where x in (1,2)) s;insert into t(a,b) values (1,-2);\n\n\ncreate table x(id int primary key, name varchar(10) not null)",
            "postgresql",
        );
        assert_eq!(
            out,
            "-- Top\nSELECT *\nFROM (\n    SELECT id\n    FROM t\n    WHERE x IN (1, 2)\n) s;\nINSERT INTO t (a, b)\nVALUES (1, -2);\n\nCREATE TABLE x (\n    id int PRIMARY key,\n    name varchar(10) NOT NULL\n)"
        );
    }

    #[test]
    fn comments_and_casts_survive() {
        let src = "select a::int, -- first\n  b /* inline */ from t -- end\n;\n-- after\n";
        let out = fmt(src, "postgresql");
        assert!(out.contains("-- first\n"), "{out}");
        assert!(out.contains("a::int"));
        assert!(out.contains("/* inline */"));
        assert!(out.ends_with("-- after\n"), "{out:?}");
        assert_eq!(fmt(&out, "postgresql"), out);
    }

    #[test]
    fn routines_and_delimiter_scripts_are_kept() {
        let src = "create function f() returns int as $$ select 1 $$ language sql;\nselect 1";
        assert!(
            fmt(src, "postgresql")
                .starts_with("create function f() returns int as $$ select 1 $$ language sql;")
        );
        let src =
            "DELIMITER //\ncreate procedure p() begin select 1; end //\nDELIMITER ;\nselect 1";
        let out = fmt(src, "mysql");
        assert!(
            out.contains("DELIMITER //\ncreate procedure p() begin select 1; end //\nDELIMITER ;"),
            "{out}"
        );
    }

    #[test]
    fn corpus_is_idempotent_and_preserves_tokens() {
        let corpus = [
            (
                "postgresql",
                "with a as (select 1 x), b as (select x from a) select * from b union all select 2",
            ),
            (
                "postgresql",
                "update orders o set total = total * 1.1, status = 'x' from customers c where c.id = o.customer_id returning o.id",
            ),
            (
                "postgresql",
                "select count(*) filter (where x > 0) over (partition by y order by z rows between unbounded preceding and current row) from t",
            ),
            (
                "postgresql",
                "select case when a = 1 then 'one' else 'many' end as n, array[1,2], j->>'k', j #> '{a}' from t where not exists(select 1)",
            ),
            (
                "mysql",
                "select `a`, b from t where c like '%x\\'%' limit 10, 5; insert into t set a = 1 on duplicate key update a = values(a)",
            ),
            (
                "sqlite",
                "select [a] , \"b\" from t join u using (id) where a is not null and b <> ? group by a having count(*) > 1",
            ),
            (
                "postgresql",
                "delete from t where id in (select id from u where x = $1) -- trailing\n",
            ),
            (
                "postgresql",
                "select 1;;select 2 /* c */ ; -- x\n-- y\n\nselect 3",
            ),
            (
                "postgresql",
                "SELECT a FROM t WHERE x >= -1 AND y<>-2 AND z = +3",
            ),
        ];
        for (d, src) in corpus {
            let once = fmt(src, d);
            let twice = fmt(&once, d);
            assert_eq!(
                once, twice,
                "not idempotent:\n{src}\n---\n{once}\n---\n{twice}"
            );
            assert_eq!(
                significant(src, d),
                significant(&once, d),
                "tokens changed:\n{src}\n---\n{once}"
            );
        }
    }
}
