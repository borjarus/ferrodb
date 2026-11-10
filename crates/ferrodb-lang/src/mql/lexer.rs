//! Tokens of the MongoDB shell subset: identifiers (with `$`), numbers,
//! strings (`"…"`, `'…'`, `` `…` ``), regular expressions, punctuation and
//! comments. Positions are byte offsets in the source.

use std::ops::Range;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Number(String),
    Str(String),
    /// `/pattern/flags`
    Regex(String, String),
    Punct(char),
    /// `//…` or `/* … */` (kept for the splitter and the formatter).
    Comment,
    /// A line break outside brackets (statement separator candidate).
    Newline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub range: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LexError {
    pub message: String,
    pub range: Range<usize>,
}

/// Whether `/` at this point starts a regular expression (a value is
/// expected) rather than being a division (not supported anyway).
fn regex_allowed(prev: Option<&Tok>) -> bool {
    match prev {
        None => true,
        Some(Tok::Punct(c)) => {
            matches!(c, '(' | ',' | ':' | '[' | '{' | '=' | '!' | '&' | '|' | '?')
        }
        Some(Tok::Newline | Tok::Comment) => true,
        _ => false,
    }
}

pub fn lex(src: &str) -> (Vec<Token>, Vec<LexError>) {
    let b = src.as_bytes();
    let mut out: Vec<Token> = Vec::new();
    let mut errors = Vec::new();
    let mut i = 0;
    let mut depth: i32 = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        match c {
            b'\n' => {
                if depth <= 0 {
                    out.push(Token {
                        tok: Tok::Newline,
                        range: i..i + 1,
                    });
                }
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Comment,
                    range: start..i,
                });
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                if i + 1 >= b.len() {
                    errors.push(LexError {
                        message: "unterminated comment".into(),
                        range: start..b.len(),
                    });
                    i = b.len();
                } else {
                    i += 2;
                }
                out.push(Token {
                    tok: Tok::Comment,
                    range: start..i,
                });
            }
            b'/' if regex_allowed(
                out.iter()
                    .rev()
                    .find(|t| t.tok != Tok::Comment)
                    .map(|t| &t.tok),
            ) =>
            {
                i += 1;
                let mut class = false;
                let mut pat = String::new();
                let mut closed = false;
                while i < b.len() {
                    let ch = src[i..].chars().next().unwrap_or('\0');
                    match ch {
                        '\\' => {
                            pat.push('\\');
                            i += 1;
                            if let Some(n) = src[i..].chars().next() {
                                pat.push(n);
                                i += n.len_utf8();
                            }
                            continue;
                        }
                        '[' => class = true,
                        ']' => class = false,
                        '/' if !class => {
                            closed = true;
                            i += 1;
                            break;
                        }
                        '\n' => break,
                        _ => {}
                    }
                    pat.push(ch);
                    i += ch.len_utf8();
                }
                if !closed {
                    errors.push(LexError {
                        message: "unterminated regular expression".into(),
                        range: start..i,
                    });
                }
                let fstart = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                out.push(Token {
                    tok: Tok::Regex(pat, src[fstart..i].to_owned()),
                    range: start..i,
                });
            }
            b'"' | b'\'' | b'`' => {
                let q = c;
                i += 1;
                let mut s = String::new();
                let mut closed = false;
                while i < b.len() {
                    let ch = src[i..].chars().next().unwrap_or('\0');
                    if ch as u32 == q as u32 {
                        closed = true;
                        i += 1;
                        break;
                    }
                    if ch == '\n' && q != b'`' {
                        break;
                    }
                    if ch == '\\' {
                        i += 1;
                        let Some(e) = src[i..].chars().next() else {
                            break;
                        };
                        i += e.len_utf8();
                        match e {
                            'n' => s.push('\n'),
                            't' => s.push('\t'),
                            'r' => s.push('\r'),
                            'b' => s.push('\u{8}'),
                            'f' => s.push('\u{c}'),
                            '0' => s.push('\0'),
                            'u' => {
                                let (code, used) = unicode_escape(&src[i..]);
                                i += used;
                                match code.and_then(char::from_u32) {
                                    Some(ch) => s.push(ch),
                                    None => errors.push(LexError {
                                        message: "invalid \\u escape".into(),
                                        range: i - used - 2..i,
                                    }),
                                }
                            }
                            other => s.push(other),
                        }
                        continue;
                    }
                    s.push(ch);
                    i += ch.len_utf8();
                }
                if !closed {
                    errors.push(LexError {
                        message: "unterminated string".into(),
                        range: start..i,
                    });
                }
                out.push(Token {
                    tok: Tok::Str(s),
                    range: start..i,
                });
            }
            b'0'..=b'9' => {
                i = number_end(b, i);
                out.push(Token {
                    tok: Tok::Number(src[start..i].to_owned()),
                    range: start..i,
                });
            }
            b'.' if b.get(i + 1).is_some_and(u8::is_ascii_digit)
                && !matches!(
                    out.last().map(|t| &t.tok),
                    Some(Tok::Ident(_) | Tok::Punct(')' | ']'))
                ) =>
            {
                i = number_end(b, i);
                out.push(Token {
                    tok: Tok::Number(src[start..i].to_owned()),
                    range: start..i,
                });
            }
            _ if c == b'_' || c == b'$' || c.is_ascii_alphabetic() || c >= 0x80 => {
                while i < b.len() {
                    let ch = src[i..].chars().next().unwrap_or('\0');
                    if ch == '_' || ch == '$' || ch.is_alphanumeric() {
                        i += ch.len_utf8();
                    } else {
                        break;
                    }
                }
                out.push(Token {
                    tok: Tok::Ident(src[start..i].to_owned()),
                    range: start..i,
                });
            }
            _ => {
                let ch = src[i..].chars().next().unwrap_or('\0');
                match ch {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' => depth -= 1,
                    _ => {}
                }
                out.push(Token {
                    tok: Tok::Punct(ch),
                    range: i..i + ch.len_utf8(),
                });
                i += ch.len_utf8();
            }
        }
    }
    (out, errors)
}

fn number_end(b: &[u8], mut i: usize) -> usize {
    if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
        i += 2;
        while i < b.len() && b[i].is_ascii_hexdigit() {
            i += 1;
        }
        return i;
    }
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.' || b[i] == b'_') {
        i += 1;
    }
    if i < b.len() && matches!(b[i], b'e' | b'E') {
        let mut j = i + 1;
        if j < b.len() && matches!(b[j], b'+' | b'-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            i = j;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
    }
    i
}

fn unicode_escape(s: &str) -> (Option<u32>, usize) {
    if let Some(rest) = s.strip_prefix('{') {
        let end = rest.find('}').unwrap_or(rest.len());
        return (
            u32::from_str_radix(&rest[..end], 16).ok(),
            (end + 2).min(s.len()),
        );
    }
    let hex: String = s.chars().take(4).collect();
    (u32::from_str_radix(&hex, 16).ok(), hex.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).0.into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn lexes_shell_syntax() {
        assert_eq!(
            toks("db.c.find({a: 'x', $or: [1.5e3]}) // hi"),
            vec![
                Tok::Ident("db".into()),
                Tok::Punct('.'),
                Tok::Ident("c".into()),
                Tok::Punct('.'),
                Tok::Ident("find".into()),
                Tok::Punct('('),
                Tok::Punct('{'),
                Tok::Ident("a".into()),
                Tok::Punct(':'),
                Tok::Str("x".into()),
                Tok::Punct(','),
                Tok::Ident("$or".into()),
                Tok::Punct(':'),
                Tok::Punct('['),
                Tok::Number("1.5e3".into()),
                Tok::Punct(']'),
                Tok::Punct('}'),
                Tok::Punct(')'),
                Tok::Comment,
            ]
        );
        assert_eq!(
            toks("{n: /ab\\/c[/]/i}")[3],
            Tok::Regex("ab\\/c[/]".into(), "i".into())
        );
        assert_eq!(toks("\"a\\u00e9\\n\"")[0], Tok::Str("aé\n".into()));
        // Newlines inside brackets are not separators.
        assert_eq!(
            toks("f(\n1)\nx")
                .iter()
                .filter(|t| **t == Tok::Newline)
                .count(),
            1
        );
        assert!(!lex("'abc").1.is_empty());
    }
}
