//! Masking of secrets in text that may leave the machine (crash reports, the
//! log tail kept for them). Plan, section 3: "secret masking in logs".
//!
//! There was no masking code in the workspace before phase 10 (secrets are
//! kept in the keyring and passed around as `secret://` references); this
//! is the one place that scrubs free text. It is deliberately greedy: a
//! false positive costs a few characters of a crash report, a miss leaks a
//! password.
//!
//! Masked:
//! - passwords in URIs (`scheme://user:pass@host` → `scheme://user:***@host`);
//! - `key=value` / `key: value` / `"key": "value"` pairs whose key names a
//!   secret (`password`, `pwd`, `passwd`, `secret`, `token`, `api_key`,
//!   `apikey`, `access_key`, `secret_key`, `client_secret`, `private_key`…),
//!   including ODBC braces (`Pwd={a;b}`);
//! - HTTP `Bearer` / `Basic` credentials;
//! - AWS access key ids (`AKIA…`, `ASIA…`);
//! - Anthropic (`sk-ant-…`) and other `sk-…` API keys.
//!
//! `secret://name` references are not secrets and stay as they are.

/// The replacement of a masked value.
pub const MASK: &str = "***";

/// Key names (lower case, compared without `-` / `_`) whose values are secret.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "pwd",
    "pass",
    "secret",
    "token",
    "apikey",
    "accesskey",
    "secretkey",
    "clientsecret",
    "privatekey",
    "authtoken",
    "accesstoken",
    "refreshtoken",
    "sessiontoken",
    "awssecretaccesskey",
    "xapikey",
    "credentials",
    "sharedaccesskey",
    "accountkey",
];

/// Masks every secret found in `text`.
pub fn mask_secrets(text: &str) -> String {
    let s = mask_uri_passwords(text);
    let s = mask_pairs(&s);
    let s = mask_auth_schemes(&s);
    let s = mask_prefixed_tokens(&s);
    mask_aws_key_ids(&s)
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-'
}

/// `scheme://user:pass@host` → `scheme://user:***@host`.
fn mask_uri_passwords(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while let Some(off) = s[i..].find("://") {
        let start = i + off + 3;
        // Authority: up to '/', '?', '#', whitespace or a quote.
        let end = s[start..]
            .find(|c: char| {
                matches!(c, '/' | '?' | '#' | '"' | '\'' | '<' | '>') || c.is_whitespace()
            })
            .map_or(s.len(), |e| start + e);
        let authority = &s[start..end];
        if let Some(at) = authority.rfind('@')
            && let Some(colon) = authority[..at].find(':')
            && colon + 1 < at
        {
            let pass_start = start + colon + 1;
            let pass_end = start + at;
            out.push_str(&s[last..pass_start]);
            out.push_str(MASK);
            last = pass_end;
        }
        i = end.max(start);
        if i >= b.len() {
            break;
        }
    }
    out.push_str(&s[last..]);
    out
}

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '_' && *c != '-' && *c != '.')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn is_secret_key(key: &str) -> bool {
    let k = normalized_key(key);
    SECRET_KEYS
        .iter()
        .any(|s| k == *s || (s.len() > 4 && k.ends_with(s)))
}

/// `key=value`, `key: value`, `"key": "value"` with a secret key.
fn mask_pairs(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        // Start of a word (key candidate).
        if !is_word(b[i]) || (i > 0 && is_word(b[i - 1])) {
            i += 1;
            continue;
        }
        let key_start = i;
        let mut j = i;
        while j < b.len() && (is_word(b[j]) || b[j] == b'.') {
            j += 1;
        }
        let key = &s[key_start..j];
        let mut k = j;
        // Closing quote of a JSON / TOML key.
        if k < b.len() && (b[k] == b'"' || b[k] == b'\'') {
            k += 1;
        }
        while k < b.len() && b[k] == b' ' {
            k += 1;
        }
        let sep = k < b.len() && (b[k] == b'=' || b[k] == b':');
        // `scheme://` is not a separator.
        if !sep || !is_secret_key(key) || s[k..].starts_with("://") {
            i = j.max(i + 1);
            continue;
        }
        k += 1;
        while k < b.len() && b[k] == b' ' {
            k += 1;
        }
        if k >= b.len() {
            break;
        }
        let (value_start, value_end) = match b[k] {
            q @ (b'"' | b'\'') => {
                let start = k + 1;
                let end = s[start..].find(q as char).map_or(b.len(), |e| start + e);
                (start, end)
            }
            b'{' => {
                let start = k + 1;
                let end = s[start..].find('}').map_or(b.len(), |e| start + e);
                (start, end)
            }
            _ => {
                let start = k;
                let end = s[start..]
                    .find(|c: char| {
                        matches!(c, ';' | '&' | ',' | '"' | '\'' | '}' | ')' | ']')
                            || c.is_whitespace()
                    })
                    .map_or(b.len(), |e| start + e);
                (start, end)
            }
        };
        let value = &s[value_start..value_end];
        // Keyring references and empty values are not secrets.
        if !value.is_empty() && !value.starts_with("secret://") && value != MASK {
            out.push_str(&s[last..value_start]);
            out.push_str(MASK);
            last = value_end;
        }
        i = value_end.max(k + 1);
    }
    out.push_str(&s[last..]);
    out
}

/// `Bearer <token>`, `Basic <credentials>` (any case).
fn mask_auth_schemes(s: &str) -> String {
    // ASCII lower-casing keeps byte offsets.
    let lower = s.to_ascii_lowercase();
    let lower = lower.as_bytes();
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while i < b.len() {
        let hit = ["bearer ", "basic "]
            .iter()
            .find(|p| lower[i..].starts_with(p.as_bytes()))
            .filter(|_| i == 0 || !is_word(b[i - 1]));
        let Some(p) = hit else {
            i += 1;
            continue;
        };
        let start = i + p.len();
        let end = s[start..]
            .find(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | ',' | ';'))
            .map_or(s.len(), |e| start + e);
        // A word of at least 8 characters: "basic auth" stays readable.
        if end - start >= 8 && &s[start..end] != MASK {
            out.push_str(&s[last..start]);
            out.push_str(MASK);
            last = end;
        }
        i = end.max(start);
    }
    out.push_str(&s[last..]);
    out
}

/// `sk-ant-…`, `sk-…` (20+ characters) API keys.
fn mask_prefixed_tokens(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while let Some(off) = s[i..].find("sk-") {
        let start = i + off;
        if start > 0 && is_word(b[start - 1]) {
            i = start + 3;
            continue;
        }
        let mut end = start + 3;
        while end < b.len() && is_word(b[end]) {
            end += 1;
        }
        let prefix = if s[start..].starts_with("sk-ant-") {
            "sk-ant-"
        } else {
            "sk-"
        };
        if end - start >= prefix.len() + 16 {
            out.push_str(&s[last..start]);
            out.push_str(prefix);
            out.push_str(MASK);
            last = end;
        }
        i = end;
    }
    out.push_str(&s[last..]);
    out
}

/// `AKIA` / `ASIA` + 16 upper-case letters or digits.
fn mask_aws_key_ids(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut last = 0;
    let mut i = 0;
    while i + 20 <= b.len() {
        let candidate = &b[i..i + 20];
        let prefix_ok = candidate.starts_with(b"AKIA") || candidate.starts_with(b"ASIA");
        let boundary_before = i == 0 || !b[i - 1].is_ascii_alphanumeric();
        let boundary_after = i + 20 == b.len() || !b[i + 20].is_ascii_alphanumeric();
        if prefix_ok
            && boundary_before
            && boundary_after
            && candidate[4..]
                .iter()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            out.push_str(&s[last..i + 4]);
            out.push_str(MASK);
            last = i + 20;
            i += 20;
        } else {
            i += 1;
        }
    }
    out.push_str(&s[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::mask_secrets as m;

    #[test]
    fn uri_passwords() {
        assert_eq!(
            m("connect postgres://app:s3cr3t@db.local:5432/shop failed"),
            "connect postgres://app:***@db.local:5432/shop failed"
        );
        assert_eq!(
            m("mongodb+srv://u:p%40ss@cluster0.x.net/?w=1"),
            "mongodb+srv://u:***@cluster0.x.net/?w=1"
        );
        // No password, no change.
        assert_eq!(m("redis://cache:6379/0"), "redis://cache:6379/0");
        assert_eq!(m("https://user@host/x"), "https://user@host/x");
    }

    #[test]
    fn key_value_pairs() {
        assert_eq!(
            m("Server=db;User Id=sa;Password=Pa$$w0rd;Encrypt=true"),
            "Server=db;User Id=sa;Password=***;Encrypt=true"
        );
        assert_eq!(m("Driver={x};Pwd={a;b}c;"), "Driver={x};Pwd={***}c;");
        assert_eq!(
            m(r#"{"user":"a","password": "hunter2","port":1}"#),
            r#"{"user":"a","password": "***","port":1}"#
        );
        assert_eq!(m("api_key=abc123 next"), "api_key=*** next");
        assert_eq!(m("X-Api-Key: abcdef"), "X-Api-Key: ***");
        assert_eq!(
            m("aws_secret_access_key = wJalrXUtnFEMI/K7MDENG"),
            "aws_secret_access_key = ***"
        );
        assert_eq!(m("client_secret='x y'"), "client_secret='***'");
    }

    #[test]
    fn references_and_harmless_words_stay() {
        assert_eq!(
            m("password = secret://pg-prod"),
            "password = secret://pg-prod"
        );
        assert_eq!(m("the password was wrong"), "the password was wrong");
        assert_eq!(m("tokenizer=fast"), "tokenizer=fast");
        assert_eq!(m("basic auth failed"), "basic auth failed");
    }

    #[test]
    fn bearer_and_keys() {
        assert_eq!(
            m("Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.e30.x"),
            "Authorization: Bearer ***"
        );
        assert_eq!(
            m("key sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv_wx-yz used"),
            "key sk-ant-*** used"
        );
        assert_eq!(m("sk-proj-abcdefghijklmnopqrstu"), "sk-***");
        assert_eq!(m("task-1 sk-short"), "task-1 sk-short");
        assert_eq!(m("id AKIAIOSFODNN7EXAMPLE in use"), "id AKIA*** in use");
        assert_eq!(m("AKIAshort"), "AKIAshort");
    }

    #[test]
    fn unicode_is_preserved() {
        assert_eq!(
            m("hasło: zażółć password=gęś; koniec"),
            "hasło: zażółć password=***; koniec"
        );
    }
}
