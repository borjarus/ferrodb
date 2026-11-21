//! `GO` batch separators. The console splits scripts on `GO` (the T-SQL
//! splitter of `ferrodb-lang`); the driver still accepts text containing
//! them (DDL scripts, a chunk that is just `GO` or `GO 5`) the way `sqlcmd`
//! does: a line holding only `GO [count]` (outside strings and comments)
//! ends a batch, which is sent `count` times.

/// One batch: its text, where it starts in the submitted text, and how many
/// times it runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Batch<'a> {
    pub text: &'a str,
    pub offset: usize,
    pub repeat: u32,
}

/// `GO`, `go 3`, `GO -- comment` → `Some(count)`.
fn go_line(line: &str) -> Option<u32> {
    let l = line.trim();
    let l = match l.find("--") {
        Some(i) => l[..i].trim_end(),
        None => l,
    };
    let rest = l.get(..2).filter(|g| g.eq_ignore_ascii_case("go"))?;
    let _ = rest;
    let tail = l[2..].trim();
    if tail.is_empty() {
        return Some(1);
    }
    if !l[2..].starts_with(char::is_whitespace) {
        return None;
    }
    tail.parse::<u32>().ok().filter(|n| *n > 0)
}

/// Splits `text` on `GO` lines. Empty batches are dropped; a text without
/// separators is one batch.
pub(crate) fn split(text: &str) -> Vec<Batch<'_>> {
    #[derive(PartialEq)]
    enum S {
        Code,
        Str,
        Ident,
        Block(u32),
    }
    let mut out = Vec::new();
    let mut state = S::Code;
    let mut start = 0;
    let mut line_start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    fn push<'a>(text: &'a str, from: usize, to: usize, repeat: u32, out: &mut Vec<Batch<'a>>) {
        let t = &text[from..to];
        if !t.trim().is_empty() {
            let lead = t.len() - t.trim_start().len();
            out.push(Batch {
                text: t.trim(),
                offset: from + lead,
                repeat,
            });
        }
    }
    while i <= bytes.len() {
        let at_end = i == bytes.len();
        if at_end || bytes[i] == b'\n' {
            if state == S::Code
                && let Some(n) = go_line(&text[line_start..i])
            {
                push(text, start, line_start, n, &mut out);
                start = (i + 1).min(bytes.len());
            }
            if at_end {
                break;
            }
            line_start = i + 1;
            i += 1;
            continue;
        }
        let c = bytes[i];
        let next = bytes.get(i + 1).copied();
        match state {
            S::Code => match (c, next) {
                (b'\'', _) => state = S::Str,
                (b'[', _) => state = S::Ident,
                (b'-', Some(b'-')) => {
                    // Line comment: skip to the end of the line.
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                (b'/', Some(b'*')) => {
                    state = S::Block(1);
                    i += 1;
                }
                _ => {}
            },
            S::Str if c == b'\'' => {
                if next == Some(b'\'') {
                    i += 1;
                } else {
                    state = S::Code;
                }
            }
            S::Ident if c == b']' => {
                if next == Some(b']') {
                    i += 1;
                } else {
                    state = S::Code;
                }
            }
            S::Block(n) => match (c, next) {
                (b'*', Some(b'/')) => {
                    state = if n == 1 { S::Code } else { S::Block(n - 1) };
                    i += 1;
                }
                (b'/', Some(b'*')) => {
                    state = S::Block(n + 1);
                    i += 1;
                }
                _ => {}
            },
            _ => {}
        }
        i += 1;
    }
    if start < text.len() {
        push(text, start, text.len(), 1, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(s: &str) -> Vec<(&str, u32)> {
        split(s).into_iter().map(|b| (b.text, b.repeat)).collect()
    }

    #[test]
    fn splits_on_go_lines() {
        assert_eq!(texts("select 1"), vec![("select 1", 1)]);
        assert_eq!(
            texts("create table t (a int)\nGO\ninsert t values (1)\ngo 3 -- thrice\nselect 1\n"),
            vec![
                ("create table t (a int)", 1),
                ("insert t values (1)", 3),
                ("select 1", 1)
            ]
        );
        assert_eq!(texts("GO"), Vec::<(&str, u32)>::new());
        assert_eq!(texts("GO 5"), Vec::<(&str, u32)>::new());
        // Not separators: inside strings and comments, or part of a word.
        assert_eq!(texts("select 'a\nGO\nb'").len(), 1);
        assert_eq!(texts("/* x\nGO\n */ select 1").len(), 1);
        assert_eq!(texts("select 1 as [x\nGO\n]").len(), 1);
        assert_eq!(texts("goto_label:\nselect 1\nGOTO x").len(), 1);
        let b = split("  \nselect 1\nGO\n  select 2");
        assert_eq!(b[1].offset, "  \nselect 1\nGO\n  ".len());
    }

    #[test]
    fn go_counts() {
        assert_eq!(go_line(" go "), Some(1));
        assert_eq!(go_line("GO 10"), Some(10));
        assert_eq!(go_line("GO10"), None);
        assert_eq!(go_line("GO x"), None);
        assert_eq!(go_line("GOTO"), None);
    }
}
