//! Statement splitting.
//!
//! Statements end at `;` (or at a custom `DELIMITER` in MySQL) outside
//! strings, quoted identifiers, comments and routine bodies
//! (`CREATE TRIGGER … BEGIN … END`, `BEGIN ATOMIC … END`, MySQL compound
//! statements). A comment on the line right before a statement names its
//! result tab.

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
            } else if t.kind == TokenKind::Punct && t.text(self.src) == ";" && cur.depth == 0 {
                self.finish(&mut cur);
                i += 1;
                continue;
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

    fn next_word(&self, from: usize) -> Option<&Token> {
        self.toks[from + 1..].iter().find(|t| !t.is_trivia())
    }

    fn track_blocks(&self, cur: &mut Current, i: usize) {
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
