//! Dialects as data (plan 5.8, ADR-009).
//!
//! Every dialect is a TOML file in `assets/dialects/` embedded at build
//! time: keywords, data types, built-in functions with signatures, the
//! identifier quote and how unquoted names are folded. A dialect may
//! `inherits` another one; lists are merged, scalar fields overridden.
//! Lexical rules that are not data (dollar quotes, `DELIMITER`…) stay in
//! [`Flavor`].

use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

use serde::Deserialize;

use super::lexer::Flavor;

const SOURCES: &[(&str, &str)] = &[
    ("sql", include_str!("../../../../assets/dialects/sql.toml")),
    (
        "postgresql",
        include_str!("../../../../assets/dialects/postgresql.toml"),
    ),
    (
        "mysql",
        include_str!("../../../../assets/dialects/mysql.toml"),
    ),
    (
        "mariadb",
        include_str!("../../../../assets/dialects/mariadb.toml"),
    ),
    (
        "sqlite",
        include_str!("../../../../assets/dialects/sqlite.toml"),
    ),
    (
        "tsql",
        include_str!("../../../../assets/dialects/tsql.toml"),
    ),
    (
        "oracle",
        include_str!("../../../../assets/dialects/oracle.toml"),
    ),
    (
        "duckdb",
        include_str!("../../../../assets/dialects/duckdb.toml"),
    ),
];

/// How unquoted identifiers are stored by the database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Folding {
    Lower,
    Upper,
    /// Spelling kept; lookups are case-insensitive.
    None,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFunction {
    name: String,
    signatures: Vec<String>,
    #[serde(default)]
    doc: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDialect {
    id: String,
    name: String,
    #[serde(default)]
    inherits: Option<String>,
    #[serde(default)]
    identifier_quote: Option<char>,
    #[serde(default)]
    case_folding: Option<Folding>,
    #[serde(default)]
    default_schema: Option<String>,
    #[serde(default)]
    parser: Option<String>,
    #[serde(default)]
    keywords: Vec<String>,
    #[serde(default)]
    soft_keywords: Vec<String>,
    #[serde(default)]
    types: Vec<String>,
    #[serde(default)]
    functions: Vec<RawFunction>,
}

/// A built-in function with its overloads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Function {
    pub name: String,
    /// `name(arg type, …) -> type`
    pub signatures: Vec<String>,
    pub doc: Option<String>,
}

#[derive(Debug)]
pub struct Dialect {
    pub id: String,
    pub name: String,
    /// Opening quote of identifiers (`"`, `` ` ``, `[`).
    pub quote: char,
    pub folding: Folding,
    /// Schema of unqualified names when the catalog does not say.
    pub default_schema: Option<String>,
    /// sqlparser dialect for syntax diagnostics (`None` = lexical checks only).
    pub parser: Option<String>,
    /// Reserved words, lower case.
    pub keywords: HashSet<String>,
    /// Non-reserved keywords, lower case.
    pub soft_keywords: HashSet<String>,
    pub types: Vec<String>,
    pub functions: Vec<Function>,
    function_index: HashMap<String, usize>,
    pub flavor: Flavor,
}

impl Dialect {
    /// Dialect for a `SqlDialectId`. Derived dialects without their own file
    /// map to their base; unknown ids get the base SQL dialect.
    pub fn get(id: &str) -> &'static Dialect {
        let all = registry();
        let key = match id {
            "greenplum" | "cockroachdb" | "redshift" => "postgresql",
            "azuresql" | "sybase" | "sqlserver" => "tsql",
            other => other,
        };
        all.get(key)
            .or_else(|| all.get("sql"))
            .expect("base SQL dialect")
    }

    /// Ids of the dialects with a file.
    pub fn ids() -> Vec<&'static str> {
        SOURCES.iter().map(|(id, _)| *id).collect()
    }

    pub fn is_keyword(&self, word: &str) -> bool {
        self.keywords.contains(&word.to_ascii_lowercase())
    }

    pub fn is_soft_keyword(&self, word: &str) -> bool {
        self.soft_keywords.contains(&word.to_ascii_lowercase())
    }

    pub fn is_type(&self, word: &str) -> bool {
        self.types.iter().any(|t| t.eq_ignore_ascii_case(word))
    }

    pub fn function(&self, name: &str) -> Option<&Function> {
        self.function_index
            .get(&name.to_ascii_lowercase())
            .map(|&i| &self.functions[i])
    }

    /// The name an unquoted identifier is stored under.
    pub fn fold(&self, ident: &str) -> String {
        match self.folding {
            Folding::Lower => ident.to_lowercase(),
            Folding::Upper => ident.to_uppercase(),
            Folding::None => ident.to_owned(),
        }
    }

    /// `name` as an identifier: quoted when folding would change it, when
    /// it is a reserved word or when it is not a plain identifier.
    pub fn quote_if_needed(&self, name: &str) -> String {
        let plain = name
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_alphanumeric() || c == '_');
        let folds = match self.folding {
            Folding::Lower => name.chars().any(char::is_uppercase),
            Folding::Upper => name.chars().any(char::is_lowercase),
            Folding::None => false,
        };
        if plain && !folds && !self.is_keyword(name) {
            return name.to_owned();
        }
        self.quote(name)
    }

    pub fn quote(&self, name: &str) -> String {
        match self.quote {
            '[' => format!("[{}]", name.replace(']', "]]")),
            q => {
                let qs = q.to_string();
                format!("{q}{}{q}", name.replace(q, &format!("{qs}{qs}")))
            }
        }
    }
}

fn registry() -> &'static HashMap<String, Dialect> {
    static REG: OnceLock<HashMap<String, Dialect>> = OnceLock::new();
    REG.get_or_init(|| {
        let raw: HashMap<String, RawDialect> = SOURCES
            .iter()
            .map(|(id, src)| {
                let d: RawDialect = toml::from_str(src)
                    .unwrap_or_else(|e| panic!("assets/dialects/{id}.toml: {e}"));
                assert_eq!(&d.id, id, "dialect id must match the file name");
                (d.id.clone(), d)
            })
            .collect();
        raw.keys().map(|id| (id.clone(), build(id, &raw))).collect()
    })
}

/// Resolves the inheritance chain of `id` (base first).
fn chain<'a>(id: &str, raw: &'a HashMap<String, RawDialect>) -> Vec<&'a RawDialect> {
    let mut out = Vec::new();
    let mut cur = raw.get(id);
    while let Some(d) = cur {
        assert!(out.len() < 8, "dialect inheritance cycle at {id}");
        out.push(d);
        cur = d.inherits.as_deref().and_then(|p| raw.get(p));
    }
    out.reverse();
    out
}

fn build(id: &str, raw: &HashMap<String, RawDialect>) -> Dialect {
    let chain = chain(id, raw);
    let last = chain.last().expect("dialect exists");
    let mut d = Dialect {
        id: last.id.clone(),
        name: last.name.clone(),
        quote: '"',
        folding: Folding::None,
        default_schema: None,
        parser: None,
        keywords: HashSet::new(),
        soft_keywords: HashSet::new(),
        types: Vec::new(),
        functions: Vec::new(),
        function_index: HashMap::new(),
        flavor: Flavor::for_dialect(id),
    };
    for r in chain {
        if let Some(q) = r.identifier_quote {
            d.quote = q;
        }
        if let Some(f) = r.case_folding {
            d.folding = f;
        }
        if r.default_schema.is_some() {
            d.default_schema.clone_from(&r.default_schema);
        }
        if let Some(p) = &r.parser {
            d.parser = (!p.is_empty()).then(|| p.clone());
        }
        d.keywords
            .extend(r.keywords.iter().map(|k| k.to_ascii_lowercase()));
        d.soft_keywords
            .extend(r.soft_keywords.iter().map(|k| k.to_ascii_lowercase()));
        for t in &r.types {
            if !d.types.iter().any(|x| x.eq_ignore_ascii_case(t)) {
                d.types.push(t.to_ascii_lowercase());
            }
        }
        for f in &r.functions {
            let key = f.name.to_ascii_lowercase();
            let func = Function {
                name: key.clone(),
                signatures: f.signatures.clone(),
                doc: f.doc.clone(),
            };
            match d.function_index.get(&key) {
                Some(&i) => d.functions[i] = func,
                None => {
                    d.function_index.insert(key, d.functions.len());
                    d.functions.push(func);
                }
            }
        }
    }
    // A reserved word in the dialect is not "soft" there.
    let reserved = d.keywords.clone();
    d.soft_keywords.retain(|k| !reserved.contains(k));
    d
}

/// A signature split into its parts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedSignature {
    pub name: String,
    /// Parameter texts (`x numeric`); a trailing `...` is its own entry.
    pub params: Vec<String>,
    pub returns: Option<String>,
    pub variadic: bool,
}

/// Parses `name(a int, b text, ...) -> type`.
pub fn parse_signature(sig: &str) -> ParsedSignature {
    let (head, returns) = match sig.rsplit_once("->") {
        Some((h, r)) => (h.trim(), Some(r.trim().to_owned())),
        None => (sig.trim(), None),
    };
    let open = head.find('(').unwrap_or(head.len());
    let name = head[..open].trim().to_owned();
    let inner = head[open..]
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();
    let mut params = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                params.push(cur.trim().to_owned());
                cur.clear();
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    if !cur.trim().is_empty() {
        params.push(cur.trim().to_owned());
    }
    let variadic = params.last().is_some_and(|p| p == "...");
    ParsedSignature {
        name,
        params,
        returns,
        variadic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dialect_loads_and_inherits() {
        for id in Dialect::ids() {
            let d = Dialect::get(id);
            assert_eq!(d.id, id);
            assert!(d.keywords.contains("select"), "{id}");
            assert!(d.function("count").is_some(), "{id}");
            for f in &d.functions {
                for s in &f.signatures {
                    let p = parse_signature(s);
                    assert_eq!(p.name, f.name, "{id}: {s}");
                }
            }
        }
        let maria = Dialect::get("mariadb");
        assert_eq!(maria.quote, '`');
        assert!(maria.function("group_concat").is_some());
        assert!(maria.function("json_detailed").is_some());
        assert_eq!(Dialect::get("greenplum").id, "postgresql");
        assert_eq!(Dialect::get("nonsense").id, "sql");
        // Reserved in PostgreSQL, so not soft there.
        assert!(!Dialect::get("postgresql").is_soft_keyword("limit"));
    }

    #[test]
    fn quoting_follows_folding() {
        let pg = Dialect::get("postgresql");
        assert_eq!(pg.quote_if_needed("orders"), "orders");
        assert_eq!(pg.quote_if_needed("Orders"), "\"Orders\"");
        assert_eq!(pg.quote_if_needed("user"), "\"user\"");
        assert_eq!(pg.quote_if_needed("a b"), "\"a b\"");
        let my = Dialect::get("mysql");
        assert_eq!(my.quote_if_needed("Orders"), "Orders");
        assert_eq!(my.quote_if_needed("key"), "`key`");
        assert_eq!(Dialect::get("tsql").quote("a]b"), "[a]]b]");
    }

    #[test]
    fn signatures_parse() {
        let p = parse_signature("round(x numeric, digits int) -> numeric");
        assert_eq!(p.params, vec!["x numeric", "digits int"]);
        assert_eq!(p.returns.as_deref(), Some("numeric"));
        let p = parse_signature("coalesce(value any, ...) -> any");
        assert!(p.variadic);
        assert!(parse_signature("now() -> timestamptz").params.is_empty());
    }
}
