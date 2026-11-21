//! The Redis command language (plan 2.10, level B): one command per line,
//! arguments split like `redis-cli` does (double quotes with `\n`, `\xHH`
//! escapes, single quotes with `\'`), a command catalog with arity, flags,
//! group, summary and argument syntax (`assets/dialects/redis-commands.toml`:
//! Redis 7/8 core, RedisJSON and RediSearch basics), completion of commands,
//! subcommands, options and key names, signature help, diagnostics for
//! unknown commands and wrong arity, and the classification the driver and
//! the core use (`is_write`, `is_dangerous`, `is_blocking`).

use std::{collections::HashMap, ops::Range, sync::OnceLock};

use ferrodb_api::{
    CompletionItem, CompletionKind, Diagnostic, Hover, Risk, RiskKind, Severity, Signature,
    SignatureHelp,
};
use serde::Deserialize;

use crate::util::{Cand, rank};

mod tests;

const CATALOG: &str = include_str!("../../../../assets/dialects/redis-commands.toml");

/// Built-in live templates (abbreviation, description, body).
pub const TEMPLATES: &[(&str, &str, &str)] = &[
    ("get", "GET key", "GET $key$$END$"),
    (
        "set",
        "SET key value EX seconds",
        "SET $key$ $value$ EX $seconds$$END$",
    ),
    ("hgetall", "HGETALL key", "HGETALL $key$$END$"),
    (
        "hset",
        "HSET key field value",
        "HSET $key$ $field$ $value$$END$",
    ),
    (
        "scan",
        "SCAN with MATCH and COUNT",
        "SCAN 0 MATCH $pattern$* COUNT 100$END$",
    ),
    (
        "zrange",
        "ZRANGE … WITHSCORES",
        "ZRANGE $key$ 0 -1 WITHSCORES$END$",
    ),
    ("lrange", "LRANGE key 0 -1", "LRANGE $key$ 0 -1$END$"),
    (
        "xadd",
        "XADD stream * field value",
        "XADD $stream$ * $field$ $value$$END$",
    ),
    ("multi", "MULTI … EXEC", "MULTI\n$SELECTION$$END$\nEXEC"),
    ("jget", "JSON.GET key path", "JSON.GET $key$ $$$END$"),
    (
        "ftsearch",
        "FT.SEARCH index query",
        "FT.SEARCH $index$ \"$query$\" LIMIT 0 10$END$",
    ),
];

/// A command of the catalog.
#[derive(Clone, Debug, Deserialize)]
pub struct CommandInfo {
    /// Upper case; subcommands are `"CONFIG GET"`.
    pub name: String,
    pub group: String,
    pub summary: String,
    /// As `COMMAND INFO`: counts the command name (both words for a
    /// subcommand); negative = at least `-arity`.
    pub arity: i32,
    pub flags: Vec<String>,
    /// Arguments after the name, as in the Redis documentation.
    pub syntax: String,
    /// First key, last key (negative: from the end), step; zeros = none.
    pub keys: [i32; 3],
    pub since: String,
}

impl CommandInfo {
    pub fn has_flag(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }

    /// A container (`CONFIG`, `CLIENT`…) whose subcommands are entries.
    pub fn is_container(&self) -> bool {
        catalog().subcommands.contains_key(&self.name)
    }

    /// Words of the name (1 or 2).
    pub fn words(&self) -> usize {
        self.name.split(' ').count()
    }

    /// Whether argument `idx` (0 = the command name) is a key.
    pub fn is_key_position(&self, idx: usize, argc: usize) -> bool {
        let [first, last, step] = self.keys;
        if first <= 0 {
            return false;
        }
        let last = if last < 0 { argc as i32 + last } else { last };
        let i = idx as i32;
        i >= first && i <= last.max(first) && (step <= 1 || (i - first) % step == 0)
    }

    /// `GET key`.
    pub fn usage(&self) -> String {
        if self.syntax.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.name, self.syntax)
        }
    }
}

#[derive(Deserialize)]
struct File {
    commands: Vec<CommandInfo>,
}

struct Catalog {
    commands: Vec<CommandInfo>,
    by_name: HashMap<String, usize>,
    /// Container → subcommand words.
    subcommands: HashMap<String, Vec<String>>,
}

fn catalog() -> &'static Catalog {
    static CAT: OnceLock<Catalog> = OnceLock::new();
    CAT.get_or_init(|| {
        let f: File = toml::from_str(CATALOG).expect("assets/dialects/redis-commands.toml");
        let mut by_name = HashMap::new();
        let mut subcommands: HashMap<String, Vec<String>> = HashMap::new();
        for (i, c) in f.commands.iter().enumerate() {
            by_name.insert(c.name.clone(), i);
            if let Some((container, sub)) = c.name.split_once(' ') {
                subcommands
                    .entry(container.to_owned())
                    .or_default()
                    .push(sub.to_owned());
            }
        }
        Catalog {
            commands: f.commands,
            by_name,
            subcommands,
        }
    })
}

/// Every command of the catalog (containers and subcommands).
pub fn commands() -> &'static [CommandInfo] {
    &catalog().commands
}

/// The catalog entry for a name (`"get"`, `"config get"`).
pub fn command(name: &str) -> Option<&'static CommandInfo> {
    let c = catalog();
    c.by_name
        .get(&name.to_ascii_uppercase())
        .map(|&i| &c.commands[i])
}

// ---------------------------------------------------------------- lexing

/// An argument of a command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Arg {
    /// Unquoted, unescaped value.
    pub value: String,
    /// Source range, quotes included.
    pub range: Range<usize>,
    /// Quoted (never a command or an option keyword).
    pub quoted: bool,
    /// The closing quote is missing.
    pub unterminated: bool,
}

/// One command: its arguments (the first is the command name).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// From the first to the last argument.
    pub range: Range<usize>,
    pub args: Vec<Arg>,
}

impl Line {
    /// Resolved catalog entry and the number of name words it consumed.
    pub fn command(&self) -> Option<&'static CommandInfo> {
        resolve(&self.args)
    }

    pub fn name(&self) -> String {
        self.args
            .first()
            .map(|a| a.value.to_ascii_uppercase())
            .unwrap_or_default()
    }
}

fn resolve(args: &[Arg]) -> Option<&'static CommandInfo> {
    let first = args.first()?;
    let name = first.value.to_ascii_uppercase();
    if let Some(sub) = args.get(1)
        && let Some(c) = command(&format!("{name} {}", sub.value.to_ascii_uppercase()))
    {
        return Some(c);
    }
    command(&name)
}

/// Splits a console into commands. A command is a line; a quoted argument
/// may span lines. Lines starting with `#` or `//` are comments.
pub fn parse(src: &str) -> Vec<Line> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut args: Vec<Arg> = Vec::new();
    let mut i = 0;
    let flush = |args: &mut Vec<Arg>, out: &mut Vec<Line>| {
        if let (Some(f), Some(l)) = (args.first(), args.last()) {
            out.push(Line {
                range: f.range.start..l.range.end,
                args: std::mem::take(args),
            });
        }
    };
    while i < b.len() {
        match b[i] {
            b'\n' => {
                flush(&mut args, &mut out);
                i += 1;
            }
            b' ' | b'\t' | b'\r' => i += 1,
            b'#' if args.is_empty() => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if args.is_empty() && b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            q @ (b'"' | b'\'') => {
                let start = i;
                i += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while i < b.len() {
                    let c = b[i];
                    if c == b'\\' && i + 1 < b.len() {
                        let n = b[i + 1];
                        if q == b'"' {
                            match n {
                                b'n' => value.push(b'\n'),
                                b'r' => value.push(b'\r'),
                                b't' => value.push(b'\t'),
                                b'b' => value.push(8),
                                b'a' => value.push(7),
                                b'x' if i + 3 < b.len()
                                    && b[i + 2].is_ascii_hexdigit()
                                    && b[i + 3].is_ascii_hexdigit() =>
                                {
                                    let h = std::str::from_utf8(&b[i + 2..i + 4]).unwrap_or("0");
                                    value.push(u8::from_str_radix(h, 16).unwrap_or(0));
                                    i += 4;
                                    continue;
                                }
                                other => value.push(other),
                            }
                            i += 2;
                            continue;
                        } else if n == b'\'' {
                            value.push(b'\'');
                            i += 2;
                            continue;
                        }
                    }
                    if c == q {
                        closed = true;
                        i += 1;
                        break;
                    }
                    value.push(c);
                    i += 1;
                }
                args.push(Arg {
                    value: String::from_utf8_lossy(&value).into_owned(),
                    range: start..i,
                    quoted: true,
                    unterminated: !closed,
                });
            }
            _ => {
                let start = i;
                while i < b.len() && !matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
                    i += 1;
                }
                // Keep char boundaries.
                while !src.is_char_boundary(i) {
                    i += 1;
                }
                args.push(Arg {
                    value: src[start..i].to_owned(),
                    range: start..i,
                    quoted: false,
                    unterminated: false,
                });
            }
        }
    }
    flush(&mut args, &mut out);
    out
}

/// Statement ranges (one per command).
pub fn split(src: &str) -> Vec<Range<usize>> {
    parse(src).into_iter().map(|l| l.range).collect()
}

// ------------------------------------------------------- classification

fn line_command(stmt: &str) -> Option<&'static CommandInfo> {
    parse(stmt).first().and_then(Line::command)
}

/// Writes data (`SET`, `DEL`, `HSET`, `FLUSHDB`, `JSON.SET`…). Unknown
/// commands count as writes: the safe answer for read-only sources.
pub fn is_write(stmt: &str) -> bool {
    let Some(l) = parse(stmt).into_iter().next() else {
        return false;
    };
    match l.command() {
        Some(c) => {
            c.has_flag("write")
                || (c.name == "EVAL" || c.name == "EVALSHA" || c.name == "FCALL")
                || (c.is_container() && c.words() == 1)
        }
        None => true,
    }
}

/// Destroys data or disturbs the server (`FLUSHALL`, `KEYS`, `SHUTDOWN`,
/// `CONFIG SET`…): confirmed before running.
pub fn is_dangerous(stmt: &str) -> bool {
    line_command(stmt).is_some_and(|c| c.has_flag("dangerous"))
}

/// May block the connection (`BLPOP`, `XREAD BLOCK`, `SUBSCRIBE`,
/// `MONITOR`): runs on a dedicated connection with no timeout.
pub fn is_blocking(stmt: &str) -> bool {
    let Some(l) = parse(stmt).into_iter().next() else {
        return false;
    };
    let Some(c) = l.command() else {
        return false;
    };
    if matches!(c.name.as_str(), "XREAD" | "XREADGROUP") {
        return l.args.iter().any(|a| a.value.eq_ignore_ascii_case("block"));
    }
    c.has_flag("blocking")
}

/// Pub/sub commands that switch the connection into subscriber mode.
pub fn is_subscribe(stmt: &str) -> bool {
    line_command(stmt)
        .is_some_and(|c| matches!(c.name.as_str(), "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE"))
}

/// Commands that need confirmation.
pub fn risks(src: &str, all_modifications: bool) -> Vec<Risk> {
    parse(src)
        .into_iter()
        .filter_map(|l| {
            let c = l.command()?;
            let kind = if matches!(c.name.as_str(), "FLUSHALL" | "FLUSHDB") {
                RiskKind::Truncate
            } else if c.name == "FT.DROPINDEX" {
                RiskKind::Drop
            } else if (c.has_flag("dangerous") && c.name != "KEYS")
                || (all_modifications && c.has_flag("write"))
            {
                RiskKind::Modification
            } else {
                return None;
            };
            Some(Risk {
                kind,
                range: l.range,
            })
        })
        .collect()
}

// ----------------------------------------------------------- diagnostics

/// Unknown commands and subcommands, wrong number of arguments,
/// unterminated quotes; `KEYS` is flagged as a full keyspace scan.
pub fn diagnostics(src: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for l in parse(src) {
        for a in &l.args {
            if a.unterminated {
                out.push(Diagnostic {
                    range: a.range.clone(),
                    severity: Severity::Error,
                    message: "Unterminated quoted argument".into(),
                });
            }
        }
        let first = &l.args[0];
        let Some(c) = l.command() else {
            let name = l.name();
            let names = catalog().commands.iter().filter(|c| c.words() == 1);
            let hint = crate::util::closest(&name, names.map(|c| c.name.as_str()))
                .map(|s| format!("; did you mean {s}?"))
                .unwrap_or_default();
            out.push(Diagnostic {
                range: first.range.clone(),
                severity: Severity::Error,
                message: format!("Unknown command '{}'{hint}", first.value),
            });
            continue;
        };
        if c.is_container() && c.words() == 1 && c.name != "COMMAND" {
            let range = l
                .args
                .get(1)
                .map_or(first.range.clone(), |a| a.range.clone());
            let subs = catalog()
                .subcommands
                .get(&c.name)
                .cloned()
                .unwrap_or_default();
            let given = l.args.get(1).map(|a| a.value.to_ascii_uppercase());
            out.push(Diagnostic {
                range,
                severity: Severity::Error,
                message: match given {
                    Some(g) => format!(
                        "Unknown subcommand '{g}' of {}; expected one of {}",
                        c.name,
                        subs.join(", ")
                    ),
                    None => format!("{} needs a subcommand: {}", c.name, subs.join(", ")),
                },
            });
            continue;
        }
        let argc = l.args.len() as i32;
        let ok = if c.arity >= 0 {
            argc == c.arity
        } else {
            argc >= -c.arity
        };
        if !ok {
            let expected = if c.arity >= 0 {
                format!("{} argument(s)", c.arity - c.words() as i32)
            } else {
                format!("at least {} argument(s)", -c.arity - c.words() as i32)
            };
            out.push(Diagnostic {
                range: l.range.clone(),
                severity: Severity::Error,
                message: format!(
                    "Wrong number of arguments for {}: expected {expected} ({})",
                    c.name,
                    c.usage()
                ),
            });
        }
        if c.name == "KEYS" {
            out.push(Diagnostic {
                range: first.range.clone(),
                severity: Severity::Warning,
                message: "KEYS scans the whole keyspace and blocks the server; prefer SCAN".into(),
            });
        }
    }
    out
}

// ------------------------------------------------------------ completion

/// Upper-case option words of a syntax string (`NX`, `WITHSCORES`, `MATCH`).
pub fn option_words(syntax: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in syntax.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-')) {
        let ok = w.len() >= 2
            && w.chars().any(|c| c.is_ascii_uppercase())
            && w.chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-');
        if ok && !out.iter().any(|x| x == w) {
            out.push(w.to_owned());
        }
    }
    out
}

/// Line containing `offset` and the index of the argument under it (the
/// argument count when the cursor is after the last one).
fn arg_at(src: &str, offset: usize) -> (Option<Line>, usize, Range<usize>, String) {
    let line_start = src[..offset].rfind('\n').map_or(0, |i| i + 1);
    let line_end = src[offset..].find('\n').map_or(src.len(), |i| offset + i);
    let line = parse(&src[line_start..line_end])
        .into_iter()
        .next()
        .map(|mut l| {
            l.range = l.range.start + line_start..l.range.end + line_start;
            for a in &mut l.args {
                a.range = a.range.start + line_start..a.range.end + line_start;
            }
            l
        });
    let Some(l) = line else {
        return (None, 0, offset..offset, String::new());
    };
    for (i, a) in l.args.iter().enumerate() {
        if a.range.start < offset && offset <= a.range.end {
            let raw = &src[a.range.start..offset];
            let prefix = raw.trim_start_matches(['"', '\'']).to_owned();
            return (Some(l.clone()), i, a.range.start..offset, prefix);
        }
    }
    let idx = l.args.iter().filter(|a| a.range.end <= offset).count();
    (Some(l), idx, offset..offset, String::new())
}

/// Completion at `offset`; `keys` are key names known to the explorer.
pub fn complete(src: &str, offset: usize, keys: &[String]) -> Vec<CompletionItem> {
    let offset = offset.min(src.len());
    let (line, idx, replace, prefix) = arg_at(src, offset);
    let mut cands = Vec::new();
    let cat = catalog();
    if idx == 0 {
        for c in cat.commands.iter().filter(|c| c.words() == 1) {
            cands.push(
                Cand::new(c.name.clone(), CompletionKind::Method, 0)
                    .detail(format!("{} · {}", c.summary, c.group)),
            );
        }
        return rank(cands, &prefix, replace);
    }
    let Some(line) = line else {
        return Vec::new();
    };
    let name = line.name();
    if idx == 1
        && let Some(subs) = cat.subcommands.get(&name)
    {
        for s in subs {
            let full = format!("{name} {s}");
            let detail = command(&full)
                .map(|c| c.summary.clone())
                .unwrap_or_default();
            cands.push(Cand::new(s.clone(), CompletionKind::Method, 0).detail(detail));
        }
        return rank(cands, &prefix, replace);
    }
    let Some(c) = resolve(&line.args) else {
        return Vec::new();
    };
    let argc = line.args.len().max(idx + 1);
    if c.is_key_position(idx, argc) {
        add_keys(&mut cands, keys, &prefix);
    }
    for w in option_words(&c.syntax) {
        cands.push(Cand::new(w, CompletionKind::Keyword, -10));
    }
    rank(cands, &prefix, replace)
}

/// Key names; with many keys, the next `:` segment groups them.
fn add_keys(cands: &mut Vec<Cand>, keys: &[String], prefix: &str) {
    let mut segments: Vec<String> = Vec::new();
    for k in keys {
        if !prefix.is_empty()
            && !k.starts_with(prefix)
            && crate::sql::matcher::score(k, prefix).is_none()
        {
            continue;
        }
        if cands.len() > 400 {
            break;
        }
        let quoted = if k
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
            || k.is_empty()
        {
            format!("\"{}\"", k.replace('\\', "\\\\").replace('"', "\\\""))
        } else {
            k.clone()
        };
        cands.push(
            Cand::new(k.clone(), CompletionKind::Key, 50)
                .insert(quoted)
                .detail("key"),
        );
        // `user:` for `user:1:name` when typing `us`.
        if let Some(rest) = k.strip_prefix(prefix)
            && let Some(i) = rest.find(':')
        {
            let seg = format!("{prefix}{}", &rest[..=i]);
            if !segments.contains(&seg) {
                segments.push(seg);
            }
        }
    }
    for s in segments.into_iter().take(50) {
        cands.push(Cand::new(s, CompletionKind::Key, 40).detail("key prefix"));
    }
}

// ------------------------------------------------- signature help, hover

/// Top-level tokens of a syntax string: `key`, `[NX | XX]`, `[key ...]`.
fn syntax_parts(syntax: &str) -> Vec<Range<usize>> {
    let b = syntax.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start: Option<usize> = None;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'[' | b'(' => {
                if depth == 0 && start.is_none() {
                    start = Some(i);
                }
                depth += 1;
            }
            b']' | b')' => depth -= 1,
            b' ' if depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(s..i);
                }
            }
            _ => {
                if start.is_none() {
                    start = Some(i);
                }
            }
        }
    }
    if let Some(s) = start {
        out.push(s..b.len());
    }
    out
}

pub fn signature_help(src: &str, offset: usize) -> Option<SignatureHelp> {
    let (line, idx, _, _) = arg_at(src, offset.min(src.len()));
    let line = line?;
    let c = resolve(&line.args)?;
    let label = c.usage();
    let shift = c.name.len() + 1;
    let parts: Vec<Range<usize>> = syntax_parts(&c.syntax)
        .into_iter()
        .map(|r| r.start + shift..r.end + shift)
        .collect();
    let arg = idx.saturating_sub(c.words());
    let active = if parts.is_empty() {
        0
    } else {
        // A repeated last part (`key [key ...]`) stays active.
        arg.min(parts.len() - 1)
    };
    Some(SignatureHelp {
        signatures: vec![Signature {
            label,
            parameters: parts,
            documentation: Some(c.summary.clone()),
        }],
        active_signature: 0,
        active_parameter: active,
    })
}

pub fn hover(src: &str, offset: usize) -> Option<Hover> {
    let (line, idx, _, _) = arg_at(src, offset.min(src.len()));
    let line = line?;
    let c = resolve(&line.args)?;
    if idx >= c.words() && line.args.get(idx).is_none_or(|a| a.range.start > offset) {
        return None;
    }
    let range = line.args[0].range.start..line.args[c.words() - 1].range.end;
    let flags = if c.flags.is_empty() {
        String::new()
    } else {
        format!(" · {}", c.flags.join(", "))
    };
    Some(Hover {
        range,
        contents: format!(
            "**{}**\n\n`{}`\n\n{}\n\n*{}, since {}{flags}*",
            c.name,
            c.usage(),
            c.summary,
            c.group,
            c.since
        ),
    })
}

/// Upper-cases command names and puts one space between arguments;
/// arguments, comments and line breaks are kept.
pub fn format(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut last = 0;
    for l in parse(src) {
        let words = l.command().map_or(1, CommandInfo::words);
        for (i, a) in l.args.iter().enumerate() {
            let gap = &src[last..a.range.start];
            if i == 0 {
                // Before the command: lines, comments and indentation.
                let keep = gap.rfind('\n').map_or(0, |n| n + 1);
                out.push_str(&gap[..keep]);
                if keep == 0 && last > 0 {
                    out.push_str(gap);
                }
            } else {
                out.push(' ');
            }
            let text = &src[a.range.clone()];
            if i < words && !a.quoted {
                out.push_str(&text.to_ascii_uppercase());
            } else {
                out.push_str(text);
            }
            last = a.range.end;
        }
    }
    out.push_str(&src[last..]);
    out
}
