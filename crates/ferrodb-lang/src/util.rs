//! Small helpers shared by the non-SQL language modules (CQL, SQL++,
//! PartiQL, Redis): candidate ranking, the word under the cursor, lexical
//! checks over the SQL tokenizer.

use std::ops::Range;

use ferrodb_api::{CompletionItem, CompletionKind, Diagnostic, Severity};

use crate::sql::{
    lexer::{Token, TokenKind},
    matcher,
};

/// A completion candidate before ranking.
#[derive(Clone, Debug)]
pub struct Cand {
    pub label: String,
    pub insert: String,
    pub kind: CompletionKind,
    pub detail: Option<String>,
    /// Added to the match score.
    pub prio: i32,
}

impl Cand {
    pub fn new(label: impl Into<String>, kind: CompletionKind, prio: i32) -> Self {
        let label = label.into();
        Self {
            insert: label.clone(),
            label,
            kind,
            detail: None,
            prio,
        }
    }

    pub fn insert(mut self, text: impl Into<String>) -> Self {
        self.insert = text.into();
        self
    }

    pub fn detail(mut self, text: impl Into<String>) -> Self {
        self.detail = Some(text.into());
        self
    }
}

pub const MAX_ITEMS: usize = 200;

/// Candidates matching `prefix`, best first, without duplicates.
pub fn rank(cands: Vec<Cand>, prefix: &str, replace: Range<usize>) -> Vec<CompletionItem> {
    let mut scored: Vec<(i32, Cand)> = cands
        .into_iter()
        .filter_map(|c| matcher::score(&c.label, prefix).map(|s| (s + c.prio, c)))
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

/// Merges two ranked lists: `first` wins on equal labels.
pub fn merge(first: Vec<CompletionItem>, second: Vec<CompletionItem>) -> Vec<CompletionItem> {
    let mut out = first;
    for it in second {
        if !out.iter().any(|o| o.label.eq_ignore_ascii_case(&it.label)) {
            out.push(it);
        }
    }
    out.truncate(MAX_ITEMS);
    out
}

/// Index of the token containing or ending at `offset` among `toks`.
pub fn token_at(toks: &[Token], offset: usize) -> Option<usize> {
    let i = toks.partition_point(|t| t.range.end < offset);
    (i < toks.len() && toks[i].range.start <= offset).then_some(i)
}

/// Inside a string or a comment at `offset` (no completion there).
pub fn in_literal(src: &str, toks: &[Token], offset: usize) -> bool {
    let Some(i) = token_at(toks, offset) else {
        return false;
    };
    let t = &toks[i];
    if t.range.start >= offset {
        return false;
    }
    match t.kind {
        TokenKind::LineComment | TokenKind::BlockComment => true,
        TokenKind::String => {
            let s = t.text(src);
            let closed = s.len() >= 2 && s.ends_with(&s[..1]);
            offset < t.range.end || !closed
        }
        _ => false,
    }
}

/// The identifier being typed at `offset`: (replace range, prefix).
pub fn prefix_at(src: &str, toks: &[Token], offset: usize) -> (Range<usize>, String) {
    let i = toks.partition_point(|t| t.range.end < offset);
    for k in [i, i + 1] {
        if let Some(t) = toks.get(k)
            && t.range.start < offset
            && offset <= t.range.end
            && matches!(t.kind, TokenKind::Word | TokenKind::QuotedIdent)
        {
            let raw = &src[t.range.start..offset];
            let p = if t.kind == TokenKind::QuotedIdent {
                raw.trim_start_matches(['"', '`', '[']).to_owned()
            } else {
                raw.to_owned()
            };
            return (t.range.start..offset, p);
        }
    }
    (offset..offset, String::new())
}

/// Unterminated strings, quoted names and comments; unbalanced brackets.
pub fn lexical(src: &str, toks: &[Token], brackets: &[(char, char)]) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut stack: Vec<(char, Range<usize>)> = Vec::new();
    for t in toks {
        let s = t.text(src);
        match t.kind {
            TokenKind::String | TokenKind::QuotedIdent => {
                let open = s.chars().next().unwrap_or('\'');
                let open = if matches!(open, 'q' | 'Q' | 'n' | 'N' | 'e' | 'E' | 'x' | 'X') {
                    s.chars().find(|c| *c == '\'').unwrap_or('\'')
                } else {
                    open
                };
                let close = if open == '[' { ']' } else { open };
                let body = s.strip_prefix(|c: char| c != open).unwrap_or(s);
                if body.len() < 2 || !body.ends_with(close) {
                    out.push(Diagnostic {
                        range: t.range.clone(),
                        severity: Severity::Error,
                        message: if t.kind == TokenKind::String {
                            "Unterminated string literal".into()
                        } else {
                            "Unterminated quoted name".into()
                        },
                    });
                }
            }
            TokenKind::BlockComment if !s.ends_with("*/") || s.len() < 4 => {
                out.push(Diagnostic {
                    range: t.range.clone(),
                    severity: Severity::Error,
                    message: "Unterminated comment".into(),
                });
            }
            TokenKind::Punct => {
                let c = s.chars().next().unwrap_or(' ');
                if brackets.iter().any(|(o, _)| *o == c) {
                    stack.push((c, t.range.clone()));
                } else if let Some((o, _)) = brackets.iter().find(|(_, cl)| *cl == c) {
                    match stack.last() {
                        Some((top, _)) if top == o => {
                            stack.pop();
                        }
                        _ => out.push(Diagnostic {
                            range: t.range.clone(),
                            severity: Severity::Error,
                            message: format!("Unmatched '{c}'"),
                        }),
                    }
                }
            }
            _ => {}
        }
    }
    for (c, r) in stack {
        out.push(Diagnostic {
            range: r,
            severity: Severity::Error,
            message: format!("'{c}' is not closed"),
        });
    }
    out
}

/// Lower-cased first word of a statement's tokens.
pub fn first_word(src: &str, toks: &[Token]) -> Option<String> {
    toks.iter()
        .find(|t| !t.is_trivia())
        .filter(|t| t.kind == TokenKind::Word)
        .map(|t| t.text(src).to_ascii_lowercase())
}

/// `WHERE` at bracket depth 0 among significant tokens.
pub fn has_top_level(src: &str, toks: &[Token], word: &str) -> bool {
    let mut depth = 0i32;
    for t in toks.iter().filter(|t| !t.is_trivia()) {
        match t.text(src) {
            "(" | "[" | "{" => depth += 1,
            ")" | "]" | "}" => depth -= 1,
            _ if depth == 0 && t.is_word(src, word) => return true,
            _ => {}
        }
    }
    false
}

/// Closest of `words` to `w` for "did you mean".
pub fn closest<'a>(w: &str, words: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    words
        .into_iter()
        .filter_map(|k| crate::sql::resolve::close(w, k).map(|d| (d, k)))
        .min()
        .map(|(_, k)| k)
}
