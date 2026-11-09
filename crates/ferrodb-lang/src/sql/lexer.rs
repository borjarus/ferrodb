//! A permissive SQL tokenizer shared by the splitter, the parameter finder and
//! the statement analyser. It never fails: unterminated strings and comments
//! run to the end of the input, unknown bytes become punctuation.

use std::ops::Range;

/// Lexical differences between dialects that matter for splitting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flavor {
    /// `$tag$ … $tag$` strings (PostgreSQL).
    pub dollar_quotes: bool,
    /// `` `identifier` `` (MySQL, SQLite).
    pub backtick_idents: bool,
    /// `[identifier]` (SQLite, T-SQL).
    pub bracket_idents: bool,
    /// `# comment` (MySQL).
    pub hash_comments: bool,
    /// `\'` inside string literals (MySQL).
    pub backslash_escapes: bool,
    /// Nested `/* /* */ */` (PostgreSQL).
    pub nested_comments: bool,
    /// `DELIMITER //` client command (MySQL).
    pub delimiter_command: bool,
    /// Positional `?` placeholders (MySQL, SQLite). PostgreSQL uses `?` as a
    /// JSON operator, so it is not a parameter there.
    pub question_params: bool,
    /// `$1` placeholders (PostgreSQL).
    pub dollar_params: bool,
}

impl Flavor {
    pub const GENERIC: Self = Self {
        dollar_quotes: false,
        backtick_idents: false,
        bracket_idents: false,
        hash_comments: false,
        backslash_escapes: false,
        nested_comments: false,
        delimiter_command: false,
        question_params: true,
        dollar_params: false,
    };

    /// Flavor for a dialect id (`SqlDialectId`). Unknown dialects get the
    /// generic flavor; derived dialects map to their base.
    pub fn for_dialect(dialect: &str) -> Self {
        match dialect {
            "postgresql" | "greenplum" | "cockroachdb" | "redshift" => Self {
                dollar_quotes: true,
                nested_comments: true,
                question_params: false,
                dollar_params: true,
                ..Self::GENERIC
            },
            "mysql" | "mariadb" => Self {
                backtick_idents: true,
                hash_comments: true,
                backslash_escapes: true,
                delimiter_command: true,
                ..Self::GENERIC
            },
            "sqlite" => Self {
                backtick_idents: true,
                bracket_idents: true,
                ..Self::GENERIC
            },
            "tsql" | "sybase" => Self {
                bracket_idents: true,
                ..Self::GENERIC
            },
            "clickhouse" | "hiveql" | "bigquery" | "databricks" => Self {
                backtick_idents: true,
                ..Self::GENERIC
            },
            _ => Self::GENERIC,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// Keyword or bare identifier.
    Word,
    /// `"x"`, `` `x` ``, `[x]`.
    QuotedIdent,
    /// `'x'`, `E'x'`, `$$x$$`.
    String,
    Number,
    LineComment,
    BlockComment,
    Whitespace,
    /// `:name`, `$1`, `?`.
    Param,
    /// Any other single byte (or a UTF-8 sequence outside the above).
    Punct,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub range: Range<usize>,
}

impl Token {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.range.clone()]
    }
    pub fn is_trivia(&self) -> bool {
        matches!(
            self.kind,
            TokenKind::Whitespace | TokenKind::LineComment | TokenKind::BlockComment
        )
    }
    pub fn is_word(&self, src: &str, word: &str) -> bool {
        self.kind == TokenKind::Word && self.text(src).eq_ignore_ascii_case(word)
    }
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Tokenizes `src` starting at `from`.
pub fn tokenize(src: &str, flavor: &Flavor) -> Vec<Token> {
    Lexer {
        src,
        b: src.as_bytes(),
        pos: 0,
        flavor,
    }
    .run()
}

struct Lexer<'a> {
    src: &'a str,
    b: &'a [u8],
    pos: usize,
    flavor: &'a Flavor,
}

impl Lexer<'_> {
    fn peek(&self, off: usize) -> Option<u8> {
        self.b.get(self.pos + off).copied()
    }

    /// Advances to the next char boundary at or after `pos`.
    fn boundary(&self, mut pos: usize) -> usize {
        pos = pos.min(self.b.len());
        while !self.src.is_char_boundary(pos) {
            pos += 1;
        }
        pos
    }

    fn run(mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while self.pos < self.b.len() {
            let start = self.pos;
            let kind = self.next_kind();
            self.pos = self.boundary(self.pos.max(start + 1));
            out.push(Token {
                kind,
                range: start..self.pos,
            });
        }
        out
    }

    fn next_kind(&mut self) -> TokenKind {
        let c = self.b[self.pos];
        let f = *self.flavor;
        match c {
            b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c => {
                while matches!(
                    self.peek(0),
                    Some(b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c)
                ) {
                    self.pos += 1;
                }
                TokenKind::Whitespace
            }
            b'-' if self.peek(1) == Some(b'-') => self.line_comment(),
            b'#' if f.hash_comments => self.line_comment(),
            b'/' if self.peek(1) == Some(b'*') => self.block_comment(),
            b'\'' => self.quoted(b'\'', f.backslash_escapes, TokenKind::String),
            b'"' => self.quoted(b'"', f.backslash_escapes, TokenKind::QuotedIdent),
            b'`' if f.backtick_idents => self.quoted(b'`', false, TokenKind::QuotedIdent),
            b'[' if f.bracket_idents => {
                self.pos += 1;
                while let Some(b) = self.peek(0) {
                    self.pos += 1;
                    if b == b']' {
                        break;
                    }
                }
                TokenKind::QuotedIdent
            }
            b'$' if f.dollar_params && self.peek(1).is_some_and(|b| b.is_ascii_digit()) => {
                self.pos += 1;
                while self.peek(0).is_some_and(|b| b.is_ascii_digit()) {
                    self.pos += 1;
                }
                TokenKind::Param
            }
            b'$' if f.dollar_quotes => self.dollar_string().unwrap_or_else(|| {
                self.pos += 1;
                TokenKind::Punct
            }),
            b'?' if f.question_params => {
                self.pos += 1;
                TokenKind::Param
            }
            b':' if self.peek(1).is_some_and(is_ident_start)
                && (self.pos == 0 || !matches!(self.b[self.pos - 1], b':')) =>
            {
                self.pos += 1;
                while self.peek(0).is_some_and(|b| is_ident_char(b) && b != b'$') {
                    self.pos += 1;
                }
                TokenKind::Param
            }
            b':' if self.peek(1) == Some(b':') => {
                // PostgreSQL cast `::type` — keep both colons together so the
                // type name is not mistaken for a parameter.
                self.pos += 2;
                TokenKind::Punct
            }
            b'0'..=b'9' => {
                while self
                    .peek(0)
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
                {
                    self.pos += 1;
                }
                TokenKind::Number
            }
            b'.' if self.peek(1).is_some_and(|b| b.is_ascii_digit()) => {
                self.pos += 1;
                while self.peek(0).is_some_and(|b| b.is_ascii_alphanumeric()) {
                    self.pos += 1;
                }
                TokenKind::Number
            }
            c if is_ident_start(c) => {
                // E'…' escape strings (PostgreSQL), N'…', X'…', B'…'.
                if matches!(c, b'e' | b'E' | b'n' | b'N' | b'x' | b'X' | b'b' | b'B')
                    && self.peek(1) == Some(b'\'')
                {
                    let escapes = f.backslash_escapes || matches!(c, b'e' | b'E');
                    self.pos += 1;
                    return self.quoted(b'\'', escapes, TokenKind::String);
                }
                while self.peek(0).is_some_and(is_ident_char) {
                    self.pos += 1;
                }
                TokenKind::Word
            }
            _ => {
                self.pos += 1;
                TokenKind::Punct
            }
        }
    }

    fn line_comment(&mut self) -> TokenKind {
        while let Some(b) = self.peek(0) {
            if b == b'\n' {
                break;
            }
            self.pos += 1;
        }
        TokenKind::LineComment
    }

    fn block_comment(&mut self) -> TokenKind {
        self.pos += 2;
        let mut depth = 1;
        while self.pos < self.b.len() {
            if self.peek(0) == Some(b'*') && self.peek(1) == Some(b'/') {
                self.pos += 2;
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if self.flavor.nested_comments
                && self.peek(0) == Some(b'/')
                && self.peek(1) == Some(b'*')
            {
                self.pos += 2;
                depth += 1;
            } else {
                self.pos += 1;
            }
        }
        TokenKind::BlockComment
    }

    /// Quoted literal with doubled-quote escapes (and optional backslashes).
    fn quoted(&mut self, q: u8, backslash: bool, kind: TokenKind) -> TokenKind {
        self.pos += 1;
        while let Some(b) = self.peek(0) {
            if backslash && b == b'\\' {
                self.pos += 2;
                continue;
            }
            self.pos += 1;
            if b == q {
                if self.peek(0) == Some(q) {
                    self.pos += 1;
                    continue;
                }
                break;
            }
        }
        self.pos = self.pos.min(self.b.len());
        kind
    }

    /// `$tag$ … $tag$`; `None` when `$` does not open a dollar string.
    fn dollar_string(&mut self) -> Option<TokenKind> {
        let start = self.pos;
        let mut end = start + 1;
        while end < self.b.len() && is_ident_char(self.b[end]) && self.b[end] != b'$' {
            end += 1;
        }
        if self.b.get(end) != Some(&b'$') {
            return None;
        }
        let tag = &self.src[start..=end];
        if tag.len() > 2 && self.b[start + 1].is_ascii_digit() {
            return None;
        }
        let body = end + 1;
        match self.src[body..].find(tag) {
            Some(i) => self.pos = body + i + tag.len(),
            None => self.pos = self.b.len(),
        }
        Some(TokenKind::String)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str, f: Flavor) -> Vec<(TokenKind, &str)> {
        tokenize(src, &f)
            .into_iter()
            .filter(|t| t.kind != TokenKind::Whitespace)
            .map(|t| (t.kind, &src[t.range]))
            .collect()
    }

    #[test]
    fn strings_comments_and_params() {
        let pg = Flavor::for_dialect("postgresql");
        let toks = kinds(
            "select 'a;b', $x$ ; $x$, E'\\';' -- c;\n/* /* ; */ */ :p::int, $1",
            pg,
        );
        assert_eq!(
            toks,
            vec![
                (TokenKind::Word, "select"),
                (TokenKind::String, "'a;b'"),
                (TokenKind::Punct, ","),
                (TokenKind::String, "$x$ ; $x$"),
                (TokenKind::Punct, ","),
                (TokenKind::String, "E'\\';'"),
                (TokenKind::LineComment, "-- c;"),
                (TokenKind::BlockComment, "/* /* ; */ */"),
                (TokenKind::Param, ":p"),
                (TokenKind::Punct, "::"),
                (TokenKind::Word, "int"),
                (TokenKind::Punct, ","),
                (TokenKind::Param, "$1"),
            ]
        );
    }

    #[test]
    fn mysql_specifics() {
        let my = Flavor::for_dialect("mysql");
        let toks = kinds("select `a;b`, 'it\\'s' # x;\n, ?", my);
        assert_eq!(
            toks,
            vec![
                (TokenKind::Word, "select"),
                (TokenKind::QuotedIdent, "`a;b`"),
                (TokenKind::Punct, ","),
                (TokenKind::String, "'it\\'s'"),
                (TokenKind::LineComment, "# x;"),
                (TokenKind::Punct, ","),
                (TokenKind::Param, "?"),
            ]
        );
    }

    #[test]
    fn unterminated_and_unicode_do_not_panic() {
        for f in [
            Flavor::GENERIC,
            Flavor::for_dialect("postgresql"),
            Flavor::for_dialect("mysql"),
        ] {
            for src in [
                "'abc",
                "/* x",
                "$a$ zz",
                "zażółć 'ą",
                "\"x",
                "[x",
                "`x",
                "$",
                "é$",
            ] {
                let toks = tokenize(src, &f);
                assert_eq!(toks.last().map(|t| t.range.end), Some(src.len()));
            }
        }
    }
}
