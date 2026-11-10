//! MongoDB console language (plan, phase 4b): a subset of mongosh syntax.
//!
//! - [`split`] — statements of a console (`;` or a line break outside
//!   brackets, unless the next line continues a method chain);
//! - [`parse`] — a statement into a [`Command`] with BSON arguments
//!   (relaxed JSON and helpers such as `ObjectId`, `ISODate`, `NumberDecimal`);
//! - [`risks`] — `deleteMany({})`, `updateMany({}, …)`, `drop()`…;
//! - [`complete`] — collections, methods, operators, field names;
//! - [`format`] — canonical layout of statements.

mod command;
pub mod complete;
mod lexer;
mod value;

use std::ops::Range;

pub use command::{
    COLLECTION_METHODS, CURSOR_METHODS, Command, DB_METHODS, FindAndModify, Op, parse,
};
use ferrodb_api::{Risk, RiskKind};
pub use lexer::{Tok, Token, lex};
pub use value::{ParseError, parse_value, print_inline, print_pretty, quote};

/// A single value in shell syntax (`{ a: 1 }`, `ObjectId("…")`).
pub fn value(text: &str) -> Result<bson::Bson, ParseError> {
    let (toks, errs) = lex(text);
    if let Some(e) = errs.into_iter().next() {
        return Err(ParseError::new(e.message, e.range));
    }
    let mut t = value::Tokens::new(&toks, text.len());
    let v = parse_value(&mut t)?;
    if !t.at_end() {
        return Err(ParseError::new(
            "unexpected input after the value",
            t.here(),
        ));
    }
    Ok(v)
}

/// One statement of a console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Statement {
    /// The statement without a trailing `;`.
    pub range: Range<usize>,
    /// `// title` on the line above names the result tab.
    pub title: Option<String>,
}

impl Statement {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.range.clone()]
    }
}

/// Splits a console into statements.
pub fn split(src: &str) -> Vec<Statement> {
    let (toks, _) = lex(src);
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    let mut end = 0;
    let mut depth: i32 = 0;
    let mut last_sig: Option<&Tok> = None;
    let mut title: Option<String> = None;
    let mut pending_title: Option<String> = None;
    let finish = |out: &mut Vec<Statement>,
                  start: &mut Option<usize>,
                  end: usize,
                  title: &mut Option<String>| {
        if let Some(s) = start.take() {
            out.push(Statement {
                range: s..end,
                title: title.take(),
            });
        }
    };
    for (i, t) in toks.iter().enumerate() {
        match &t.tok {
            Tok::Comment => {
                if start.is_none() {
                    let text = &src[t.range.clone()];
                    pending_title = text
                        .strip_prefix("//")
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty());
                }
            }
            Tok::Newline => {
                if start.is_some() && depth <= 0 {
                    let continues = toks[i + 1..]
                        .iter()
                        .find(|x| !matches!(x.tok, Tok::Newline | Tok::Comment))
                        .is_some_and(|x| x.tok == Tok::Punct('.'));
                    let open = matches!(last_sig, Some(Tok::Punct('.' | ',' | ':')));
                    if !continues && !open {
                        finish(&mut out, &mut start, end, &mut title);
                    }
                } else if start.is_none() && i > 0 && toks[i - 1].tok == Tok::Newline {
                    // A blank line between the comment and the statement.
                    pending_title = None;
                }
            }
            Tok::Punct(';') if depth <= 0 => {
                finish(&mut out, &mut start, end, &mut title);
                last_sig = None;
                continue;
            }
            tok => {
                if start.is_none() {
                    start = Some(t.range.start);
                    title = pending_title.take();
                }
                match tok {
                    Tok::Punct('(' | '[' | '{') => depth += 1,
                    Tok::Punct(')' | ']' | '}') => depth -= 1,
                    _ => {}
                }
                end = t.range.end;
                last_sig = Some(tok);
            }
        }
    }
    finish(&mut out, &mut start, end, &mut title);
    out
}

/// Dangerous statements: `deleteMany({})` / `remove({})`, `updateMany({}, …)`,
/// `drop()`, `dropDatabase()`, `dropIndex(es)`; with `all_modifications`,
/// every write.
pub fn risks(src: &str, all_modifications: bool) -> Vec<Risk> {
    let mut out = Vec::new();
    for st in split(src) {
        let Ok(cmd) = parse(st.text(src)) else {
            continue;
        };
        let kind = match &cmd.op {
            Op::Delete {
                filter, many: true, ..
            } if filter.is_empty() => Some(RiskKind::DeleteAll),
            Op::Update {
                filter, many: true, ..
            } if filter.is_empty() => Some(RiskKind::UpdateAll),
            Op::DropCollection { .. } | Op::DropDatabase | Op::DropIndex { .. } => {
                Some(RiskKind::Drop)
            }
            Op::RunCommand { command, .. }
                if command.keys().next().is_some_and(|k| k.starts_with("drop")) =>
            {
                Some(RiskKind::Drop)
            }
            _ if all_modifications && cmd.is_write() => Some(RiskKind::Modification),
            _ => None,
        };
        if let Some(kind) = kind {
            out.push(Risk {
                kind,
                range: st.range.clone(),
            });
        }
    }
    out
}

/// Parse errors of every statement, with absolute ranges.
pub fn diagnostics(src: &str) -> Vec<(Range<usize>, String)> {
    let (_, lex_errors) = lex(src);
    let mut out: Vec<(Range<usize>, String)> = lex_errors
        .into_iter()
        .map(|e| (e.range, e.message))
        .collect();
    for st in split(src) {
        if let Err(e) = parse(st.text(src)) {
            let r =
                st.range.start + e.range.start..st.range.start + e.range.end.max(e.range.start + 1);
            let r = r.start.min(src.len())..r.end.min(src.len());
            if !out.iter().any(|(x, _)| x.start == r.start) {
                out.push((r, e.message));
            }
        }
    }
    out.sort_by_key(|(r, _)| r.start);
    out
}

/// Statement in canonical layout: `db.coll.method(args).modifier(args)`,
/// arguments broken over lines when longer than 80 characters. `None` for
/// statements that do not parse.
pub fn format_statement(src: &str) -> Option<String> {
    let (toks, errs) = lex(src);
    if !errs.is_empty() || toks.iter().any(|t| t.tok == Tok::Comment) {
        return None;
    }
    parse(src).ok()?;
    // Rebuild from tokens: values are re-printed, the chain keeps its shape.
    let mut t = value::Tokens::new(&toks, src.len());
    let mut out = String::new();
    while let Some(tok) = t.peek() {
        match &tok.tok {
            Tok::Punct('(') => {
                t.next();
                let mut args = Vec::new();
                loop {
                    if t.eat(')') {
                        break;
                    }
                    args.push(parse_value(&mut t).ok()?);
                    if !t.eat(',') {
                        t.expect(')').ok()?;
                        break;
                    }
                }
                let flat: Vec<String> = args.iter().map(print_inline).collect();
                let one_line = format!("({})", flat.join(", "));
                if out.len() + one_line.len() <= 80 {
                    out.push_str(&one_line);
                } else {
                    let pretty: Vec<String> = args.iter().map(|a| print_pretty(a, 78)).collect();
                    out.push('(');
                    out.push_str(&pretty.join(", "));
                    out.push(')');
                }
            }
            Tok::Punct('[') => {
                t.next();
                let name = match t.next()?.tok.clone() {
                    Tok::Str(s) => s,
                    _ => return None,
                };
                t.expect(']').ok()?;
                out.push_str(&format!("[{}]", quote(&name)));
            }
            Tok::Punct(';') => {
                t.next();
            }
            Tok::Punct(c) => {
                out.push(*c);
                t.next();
            }
            Tok::Ident(s) => {
                if !out.is_empty() && !out.ends_with('.') {
                    out.push(' ');
                }
                out.push_str(s);
                t.next();
            }
            Tok::Str(s) => {
                out.push(' ');
                out.push_str(&quote(s));
                t.next();
            }
            _ => return None,
        }
    }
    Some(out)
}

/// Formats every statement that parses; text between statements
/// (comments, blank lines) and statements with comments stay as they are.
pub fn format(src: &str) -> String {
    let mut out = String::new();
    let mut at = 0;
    for st in split(src) {
        out.push_str(&src[at..st.range.start]);
        let text = st.text(src);
        match format_statement(text) {
            Some(f) => out.push_str(&f),
            None => out.push_str(text),
        }
        at = st.range.end;
    }
    out.push_str(&src[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_statements() {
        let src = "// active\ndb.c.find({\n  a: 1\n})\n  .sort({b: 1})\ndb.c.countDocuments(); show dbs\n\nuse x";
        let st = split(src);
        let texts: Vec<&str> = st.iter().map(|s| s.text(src)).collect();
        assert_eq!(
            texts,
            vec![
                "db.c.find({\n  a: 1\n})\n  .sort({b: 1})",
                "db.c.countDocuments()",
                "show dbs",
                "use x"
            ]
        );
        assert_eq!(st[0].title.as_deref(), Some("active"));
        assert_eq!(st[1].title, None);
        // A trailing dot continues on the next line.
        assert_eq!(split("db.c.\nfind()").len(), 1);
    }

    #[test]
    fn risks_and_diagnostics() {
        let src = "db.c.deleteMany({})\ndb.c.deleteMany({a: 1})\ndb.c.updateMany({}, {$set: {a: 1}})\ndb.c.drop()\ndb.c.insertOne({})";
        let kinds: Vec<RiskKind> = risks(src, false).into_iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![RiskKind::DeleteAll, RiskKind::UpdateAll, RiskKind::Drop]
        );
        assert_eq!(risks(src, true).len(), 5);
        let d = diagnostics("db.c.find({a: 1)\ndb.c.find()");
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].0.start, 15);
    }

    #[test]
    fn format_is_idempotent() {
        let src = "db.c.find({a:1,'b c':\"x\"},{_id:0}).sort({ts:-1}).limit(5);\n// keep\ndb.getCollection('x y').insertOne({ n: NumberLong(5), d: ISODate('2024-01-02T00:00:00Z') })";
        let f = format(src);
        assert_eq!(
            f,
            "db.c.find({ a: 1, \"b c\": \"x\" }, { _id: 0 }).sort({ ts: -1 }).limit(5);\n// keep\ndb.getCollection(\"x y\").insertOne({ n: NumberLong(\"5\"), d: ISODate(\"2024-01-02T00:00:00Z\") })"
        );
        assert_eq!(format(&f), f);
        let long = "db.c.aggregate([{$match: {status: 'active', total: {$gt: 100}}}, {$group: {_id: '$customer', n: {$sum: 1}}}])";
        let f = format(long);
        assert!(f.contains("\n  {"), "{f}");
        assert_eq!(format(&f), f);
        assert_eq!(parse(&f).unwrap(), parse(long).unwrap());
    }
}
