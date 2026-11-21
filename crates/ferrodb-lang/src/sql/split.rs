//! Statement splitting.
//!
//! Statements end at `;` (or at a custom `DELIMITER` in MySQL) outside
//! strings, quoted identifiers, comments and routine bodies
//! (`CREATE TRIGGER … BEGIN … END`, `BEGIN ATOMIC … END`, MySQL compound
//! statements). A comment on the line right before a statement names its
//! result tab.
//!
//! Dialect rules that are not data:
//! - T-SQL: `GO [n]` alone on a line ends a batch (and the statement);
//!   `BEGIN … END` blocks (`BEGIN TRY … END CATCH` too) keep their `;`; a
//!   `CREATE|ALTER PROCEDURE/FUNCTION/TRIGGER` body runs to the end of the
//!   batch, as the server reads it;
//! - Oracle: anonymous PL/SQL blocks (`DECLARE`/`BEGIN … END;`) and stored
//!   units (`CREATE [OR REPLACE] PROCEDURE/FUNCTION/PACKAGE [BODY]/TRIGGER/
//!   TYPE BODY`) are one statement; `/` alone on a line ends one, as in
//!   SQL*Plus;
//! - CQL: `BEGIN BATCH … APPLY BATCH` is one statement.

use std::ops::Range;

use super::lexer::{Flavor, Token, TokenKind, tokenize};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// Statement text without the terminator and surrounding trivia.
    pub body: Range<usize>,
    /// `body` extended backwards over the title comment, if any. This is the
    /// range the editor highlights and sends for "statement under cursor",
    /// so that the title travels with the statement.
    pub range: Range<usize>,
    /// Result title from the preceding comment (`-- Orders by month`).
    pub title: Option<String>,
}

impl Statement {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.body.clone()]
    }
}

/// Splits `src` into statements.
pub fn split(src: &str, flavor: &Flavor) -> Vec<Statement> {
    let toks = tokenize(src, flavor);
    Splitter {
        src,
        toks: &toks,
        flavor,
        out: Vec::new(),
    }
    .run()
}

struct Splitter<'a> {
    src: &'a str,
    toks: &'a [Token],
    flavor: &'a Flavor,
    out: Vec<Statement>,
}

/// Per-statement state while scanning.
#[derive(Default)]
struct Current {
    /// Index of the first significant token.
    first: Option<usize>,
    /// Index of the last significant token.
    last: Option<usize>,
    /// Statement is `CREATE … {TRIGGER|FUNCTION|PROCEDURE|EVENT}`: `BEGIN`
    /// opens a block whose `;` do not terminate the statement.
    routine: bool,
    /// Nesting of BEGIN/CASE … END blocks.
    depth: usize,
    /// Significant words seen so far (only the first few matter).
    words: usize,
    /// The previous word was `END` (`END CASE` does not open a CASE).
    after_end: bool,
    /// T-SQL routine: the body runs to the end of the batch.
    batch_body: bool,
    /// PL/SQL block or stored unit.
    plsql: bool,
    /// Open PL/SQL blocks; `true` = declaration section before its `BEGIN`.
    blocks: Vec<bool>,
    /// A `PROCEDURE`/`FUNCTION` header was seen: its `IS`/`AS` opens a
    /// declaration section.
    pending_sub: bool,
    /// Parenthesis depth (PL/SQL headers).
    paren: i32,
    /// Inside `BEGIN BATCH … APPLY BATCH` (CQL).
    batch: bool,
}

impl Current {
    /// `;` here does not end the statement.
    fn in_block(&self) -> bool {
        self.depth > 0 || self.batch_body || !self.blocks.is_empty() || self.batch
    }
}

impl Splitter<'_> {
    fn run(mut self) -> Vec<Statement> {
        let mut cur = Current::default();
        let mut delimiter: Option<String> = None;
        let mut i = 0;
        while i < self.toks.len() {
            let t = &self.toks[i];
            if t.is_trivia() {
                i += 1;
                continue;
            }
            // `DELIMITER xx` client command at the start of a statement.
            if self.flavor.delimiter_command
                && cur.first.is_none()
                && t.is_word(self.src, "delimiter")
                && self.at_line_start(t.range.start)
            {
                let line_end = self.src[t.range.end..]
                    .find('\n')
                    .map_or(self.src.len(), |n| t.range.end + n);
                let d = self.src[t.range.end..line_end].trim();
                if !d.is_empty() {
                    delimiter = (d != ";").then(|| d.to_owned());
                }
                while i < self.toks.len() && self.toks[i].range.start < line_end {
                    i += 1;
                }
                continue;
            }
            // `GO [n]` (T-SQL) and `/` (Oracle) alone on their line.
            if self.flavor.go_batches
                && t.is_word(self.src, "go")
                && self.at_line_start(t.range.start)
                && self.rest_of_line_is(t.range.end, |r| r.bytes().all(|b| b.is_ascii_digit()))
            {
                self.finish(&mut cur);
                i = self.skip_line(i);
                continue;
            }
            if self.flavor.plsql
                && t.kind == TokenKind::Punct
                && t.text(self.src) == "/"
                && self.at_line_start(t.range.start)
                && self.rest_of_line_is(t.range.end, str::is_empty)
            {
                self.finish(&mut cur);
                i = self.skip_line(i);
                continue;
            }
            // Terminator.
            if let Some(d) = &delimiter {
                let code = matches!(
                    t.kind,
                    TokenKind::Punct | TokenKind::Word | TokenKind::Param
                );
                if code && self.src[t.range.start..].starts_with(d.as_str()) {
                    let end = t.range.start + d.len();
                    self.finish(&mut cur);
                    while i < self.toks.len() && self.toks[i].range.start < end {
                        i += 1;
                    }
                    continue;
                }
            } else if t.kind == TokenKind::Punct && t.text(self.src) == ";" {
                if !cur.in_block() {
                    // PL/SQL needs the `;` after its final `END`.
                    if cur.plsql && cur.first.is_some() {
                        cur.last = Some(i);
                    }
                    self.finish(&mut cur);
                    i += 1;
                    continue;
                }
                // A forward declaration `PROCEDURE p;` has no body.
                cur.pending_sub = false;
            }
            self.track_blocks(&mut cur, i);
            if cur.first.is_none() {
                cur.first = Some(i);
            }
            cur.last = Some(i);
            i += 1;
        }
        self.finish(&mut cur);
        self.out
    }

    fn at_line_start(&self, pos: usize) -> bool {
        self.src[..pos]
            .rsplit('\n')
            .next()
            .is_none_or(|l| l.trim().is_empty())
    }

    /// The rest of the line after `pos`, without a trailing `--` comment and
    /// surrounding blanks, satisfies `ok`.
    fn rest_of_line_is(&self, pos: usize, ok: impl Fn(&str) -> bool) -> bool {
        let end = self.src[pos..]
            .find('\n')
            .map_or(self.src.len(), |n| pos + n);
        let rest = &self.src[pos..end];
        let rest = rest.find("--").map_or(rest, |c| &rest[..c]);
        ok(rest.trim())
    }

    /// Index of the first token on the line after token `i`.
    fn skip_line(&self, mut i: usize) -> usize {
        let start = self.toks[i].range.end;
        let end = self.src[start..]
            .find('\n')
            .map_or(self.src.len(), |n| start + n);
        while i < self.toks.len() && self.toks[i].range.start < end {
            i += 1;
        }
        i
    }

    /// Index of the next significant token after `from`.
    fn next_index(&self, from: usize) -> Option<usize> {
        (from + 1..self.toks.len()).find(|&j| !self.toks[j].is_trivia())
    }

    fn next_word(&self, from: usize) -> Option<&Token> {
        self.next_index(from).map(|j| &self.toks[j])
    }

    fn word_is(&self, t: Option<&Token>, words: &[&str]) -> bool {
        t.is_some_and(|t| words.iter().any(|w| t.is_word(self.src, w)))
    }

    /// T-SQL: blocks anywhere, routine bodies to the end of the batch.
    fn track_tsql(&self, cur: &mut Current, i: usize) {
        let t = &self.toks[i];
        if t.kind != TokenKind::Word {
            return;
        }
        let w = t.text(self.src);
        cur.words += 1;
        let after_end = std::mem::replace(&mut cur.after_end, w.eq_ignore_ascii_case("end"));
        if cur.words <= 5
            && cur
                .first
                .is_some_and(|f| self.word_is(Some(&self.toks[f]), &["create", "alter"]))
            && ["procedure", "proc", "function", "trigger"]
                .iter()
                .any(|k| w.eq_ignore_ascii_case(k))
        {
            cur.batch_body = true;
        }
        if w.eq_ignore_ascii_case("begin") {
            let next = self.next_word(i);
            let not_block = next.is_none_or(|n| n.text(self.src) == ";")
                || self.word_is(
                    next,
                    &[
                        "tran",
                        "transaction",
                        "distributed",
                        "dialog",
                        "conversation",
                    ],
                );
            if !not_block {
                cur.depth += 1;
            }
        } else if cur.depth > 0 && !after_end && w.eq_ignore_ascii_case("case") {
            cur.depth += 1;
        } else if w.eq_ignore_ascii_case("end") && cur.depth > 0 {
            cur.depth -= 1;
        }
    }

    /// Oracle: PL/SQL blocks and stored units.
    fn track_plsql(&self, cur: &mut Current, i: usize) {
        let t = &self.toks[i];
        match (t.kind, t.text(self.src)) {
            (TokenKind::Punct, "(") => cur.paren += 1,
            (TokenKind::Punct, ")") => cur.paren -= 1,
            _ => {}
        }
        if t.kind != TokenKind::Word {
            return;
        }
        let w = t.text(self.src).to_ascii_lowercase();
        cur.words += 1;
        let after_end = std::mem::replace(&mut cur.after_end, w == "end");
        let first = cur.first.is_none();
        if first && (w == "declare" || w == "begin") {
            cur.plsql = true;
        }
        if cur.words <= 6
            && !first
            && !cur.plsql
            && cur
                .first
                .is_some_and(|f| self.toks[f].is_word(self.src, "create"))
        {
            match w.as_str() {
                "procedure" | "function" | "package" => {
                    cur.plsql = true;
                    cur.pending_sub = true;
                    return;
                }
                "trigger" => {
                    cur.plsql = true;
                    return;
                }
                // `CREATE TYPE BODY`; a type specification is a plain statement.
                "type" if self.word_is(self.next_word(i), &["body"]) => {
                    cur.plsql = true;
                    cur.pending_sub = true;
                    return;
                }
                _ => {}
            }
        }
        if !cur.plsql {
            return;
        }
        let in_decl = cur.blocks.last() == Some(&true);
        match w.as_str() {
            "procedure" | "function" if in_decl => cur.pending_sub = true,
            "is" | "as" if cur.pending_sub && cur.paren == 0 => {
                cur.pending_sub = false;
                cur.blocks.push(true);
            }
            "declare" => cur.blocks.push(true),
            "begin" => {
                if in_decl {
                    if let Some(b) = cur.blocks.last_mut() {
                        *b = false;
                    }
                } else {
                    cur.blocks.push(false);
                }
            }
            "case" if !after_end && !cur.blocks.is_empty() => cur.blocks.push(false),
            // `END IF` / `END LOOP` close blocks that were not counted.
            "end"
                if !cur.blocks.is_empty() && !self.word_is(self.next_word(i), &["if", "loop"]) =>
            {
                cur.blocks.pop();
            }
            _ => {}
        }
    }

    /// CQL: `BEGIN [UNLOGGED|COUNTER] BATCH … APPLY BATCH`.
    fn track_cql(&self, cur: &mut Current, i: usize) {
        let t = &self.toks[i];
        if t.kind != TokenKind::Word {
            return;
        }
        let next = self.next_index(i);
        let next_tok = next.map(|j| &self.toks[j]);
        if cur.first.is_none() && t.is_word(self.src, "begin") {
            let after = next.and_then(|j| self.next_word(j));
            cur.batch = self.word_is(next_tok, &["batch"])
                || (self.word_is(next_tok, &["unlogged", "counter"])
                    && self.word_is(after, &["batch"]));
        } else if cur.batch && t.is_word(self.src, "apply") && self.word_is(next_tok, &["batch"]) {
            cur.batch = false;
        }
    }

    fn track_blocks(&self, cur: &mut Current, i: usize) {
        if self.flavor.go_batches {
            return self.track_tsql(cur, i);
        }
        if self.flavor.plsql {
            return self.track_plsql(cur, i);
        }
        if self.flavor.cql_batches {
            return self.track_cql(cur, i);
        }
        let t = &self.toks[i];
        if t.kind != TokenKind::Word {
            return;
        }
        let w = t.text(self.src);
        cur.words += 1;
        let after_end = std::mem::replace(&mut cur.after_end, w.eq_ignore_ascii_case("end"));
        if cur.words <= 6
            && cur
                .first
                .is_some_and(|f| self.toks[f].is_word(self.src, "create"))
            && ["trigger", "function", "procedure", "event"]
                .iter()
                .any(|k| w.eq_ignore_ascii_case(k))
        {
            cur.routine = true;
        }
        if !cur.routine {
            return;
        }
        if w.eq_ignore_ascii_case("begin")
            || (cur.depth > 0 && !after_end && w.eq_ignore_ascii_case("case"))
        {
            cur.depth += 1;
        } else if w.eq_ignore_ascii_case("end") && cur.depth > 0 {
            // `END IF` / `END LOOP` … close blocks that were not counted.
            let closes_other = self.next_word(i).is_some_and(|n| {
                ["if", "loop", "while", "repeat"]
                    .iter()
                    .any(|k| n.is_word(self.src, k))
            });
            if !closes_other {
                cur.depth -= 1;
            }
        }
    }

    fn finish(&mut self, cur: &mut Current) {
        let c = std::mem::take(cur);
        let (Some(first), Some(last)) = (c.first, c.last) else {
            return;
        };
        let body = self.toks[first].range.start..self.toks[last].range.end;
        let (title, start) = self.title_before(first);
        self.out.push(Statement {
            range: start.unwrap_or(body.start)..body.end,
            body,
            title,
        });
    }

    /// The comment ending on the line right before token `first`.
    fn title_before(&self, first: usize) -> (Option<String>, Option<usize>) {
        let mut j = first;
        while j > 0 {
            j -= 1;
            let t = &self.toks[j];
            match t.kind {
                TokenKind::Whitespace => {
                    if t.text(self.src).matches('\n').count() > 1 {
                        return (None, None);
                    }
                }
                TokenKind::LineComment | TokenKind::BlockComment => {
                    // The comment must sit on its own line(s), right above.
                    let gap = &self.src[t.range.end..self.toks[first].range.start];
                    let newlines = gap.matches('\n').count();
                    let own_line = self.at_line_start(t.range.start);
                    let ok = own_line
                        && (newlines == 1 || (t.kind == TokenKind::LineComment && newlines == 0));
                    if !ok {
                        return (None, None);
                    }
                    let title = comment_text(t.text(self.src));
                    return match title {
                        Some(s) => (Some(s), Some(t.range.start)),
                        None => (None, None),
                    };
                }
                _ => return (None, None),
            }
        }
        (None, None)
    }
}

fn comment_text(c: &str) -> Option<String> {
    let inner = if let Some(s) = c.strip_prefix("--") {
        s
    } else if let Some(s) = c.strip_prefix('#') {
        s
    } else {
        c.strip_prefix("/*")
            .map(|s| s.strip_suffix("*/").unwrap_or(s))
            .unwrap_or(c)
    };
    let t = inner.trim().trim_matches('-').trim();
    (!t.is_empty() && !t.contains('\n')).then(|| t.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str, dialect: &str) -> Vec<String> {
        split(src, &Flavor::for_dialect(dialect))
            .iter()
            .map(|s| s.text(src).to_owned())
            .collect()
    }

    #[test]
    fn basic_split_skips_empty_statements() {
        assert_eq!(
            texts("select 1; ;\n select 2 ;; -- trailing\n", "postgresql"),
            vec!["select 1", "select 2"]
        );
        assert!(texts("  -- only a comment\n ; ", "postgresql").is_empty());
    }

    #[test]
    fn semicolons_inside_literals_and_comments() {
        let src = "select ';', \"a;b\", $f$ x; y $f$ /* ; */; select 2";
        assert_eq!(texts(src, "postgresql").len(), 2);
        let src = "select `a;b`, 'it\\'s;' # c;\n; select 2";
        assert_eq!(texts(src, "mysql").len(), 2);
    }

    #[test]
    fn mysql_delimiter_blocks() {
        let src = "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END //\nDELIMITER ;\nCALL p();";
        assert_eq!(
            texts(src, "mysql"),
            vec![
                "CREATE PROCEDURE p() BEGIN SELECT 1; SELECT 2; END",
                "CALL p()"
            ]
        );
    }

    #[test]
    fn routine_bodies_without_delimiter() {
        let src = "CREATE TRIGGER t AFTER INSERT ON a BEGIN UPDATE b SET x = 1; DELETE FROM c; END; select 1";
        assert_eq!(texts(src, "sqlite").len(), 2);
        let src = "create procedure p() begin if x then select 1; end if; case when 1 then select 2; end case; end; select 3;";
        assert_eq!(texts(src, "mysql").len(), 2);
        let src =
            "CREATE FUNCTION f() RETURNS int LANGUAGE sql BEGIN ATOMIC SELECT 1; END; SELECT 2";
        assert_eq!(texts(src, "postgresql").len(), 2);
        // A transaction BEGIN is not a block.
        assert_eq!(
            texts("BEGIN; insert into t values (1); COMMIT;", "postgresql").len(),
            3
        );
    }

    #[test]
    fn titles_from_comment_right_above() {
        let src = "-- Orders by month\nselect 1;\n\n/* Top customers */\nselect 2;\n-- far away\n\nselect 3; select 4 -- x\n;";
        let st = split(src, &Flavor::for_dialect("postgresql"));
        let titles: Vec<_> = st.iter().map(|s| s.title.clone()).collect();
        assert_eq!(
            titles,
            vec![
                Some("Orders by month".to_owned()),
                Some("Top customers".to_owned()),
                None,
                None
            ]
        );
        assert_eq!(&src[st[0].range.clone()], "-- Orders by month\nselect 1");
        assert_eq!(st[0].text(src), "select 1");
        // Re-splitting the range sent by the editor keeps the title.
        let again = split(
            &src[st[0].range.clone()],
            &Flavor::for_dialect("postgresql"),
        );
        assert_eq!(again[0].title.as_deref(), Some("Orders by month"));
        // A trailing comment on the same line as code is not a title.
        let st = split("select 1; -- note\nselect 2", &Flavor::GENERIC);
        assert_eq!(st[1].title, None);
    }
}
