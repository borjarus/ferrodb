//! Live templates: built-in ones per language and user-defined ones from
//! `templates.toml` in the configuration directory:
//!
//! ```toml
//! [[template]]
//! abbreviation = "lim"
//! description = "LIMIT clause"
//! body = "LIMIT $count$$END$"
//! language = "sql"   # sql | mql | any (default: sql)
//! ```
//!
//! `$NAME$` is a variable (its name is the placeholder text, the first one
//! is selected after expansion), `$END$` the final cursor, `$SELECTION$`
//! the selected text when surrounding, `$$` a literal `$`.

use std::ops::Range;

use ferrodb_api::{LiveTemplate, QueryLanguage, TemplateExpansion};
use serde::Deserialize;

const SQL: &[(&str, &str, &str)] = &[
    ("sel", "SELECT * FROM table", "SELECT * FROM $table$$END$"),
    ("selw", "SELECT … WHERE", "SELECT * FROM $table$ WHERE $END$"),
    ("selc", "SELECT count(*)", "SELECT count(*) FROM $table$ WHERE $END$"),
    ("ins", "INSERT INTO … VALUES", "INSERT INTO $table$ ($columns$) VALUES ($END$)"),
    ("upd", "UPDATE … SET … WHERE", "UPDATE $table$ SET $column$ = $END$ WHERE "),
    ("del", "DELETE FROM … WHERE", "DELETE FROM $table$ WHERE $END$"),
    ("crt", "CREATE TABLE", "CREATE TABLE $name$ (\n    id $type$ PRIMARY KEY,\n    $END$\n)"),
    ("cte", "WITH … AS (…)", "WITH $name$ AS (\n    $SELECTION$$END$\n)\nSELECT * FROM $name$"),
    ("exi", "EXISTS (subquery)", "EXISTS (SELECT 1 FROM $table$ WHERE $END$)"),
    ("case", "CASE WHEN … END", "CASE WHEN $condition$ THEN $END$ ELSE NULL END"),
    ("lj", "LEFT JOIN … ON", "LEFT JOIN $table$ ON $END$"),
    ("gb", "GROUP BY with count", "SELECT $column$, count(*) FROM $table$ GROUP BY $column$$END$"),
];

const MQL: &[(&str, &str, &str)] = &[
    ("find", "db.collection.find()", "db.$collection$.find({$END$})"),
    ("agg", "aggregate pipeline", "db.$collection$.aggregate([\n    { $$match: {$END$} }\n])"),
    ("ins", "insertOne", "db.$collection$.insertOne({$END$})"),
    ("upd", "updateOne with $set", "db.$collection$.updateOne({$filter$}, { $$set: {$END$} })"),
];

fn lang_key(lang: &QueryLanguage) -> Option<&'static str> {
    match lang {
        QueryLanguage::Sql(_) => Some("sql"),
        QueryLanguage::Mql => Some("mql"),
        _ => None,
    }
}

/// Built-in templates of a language.
pub fn builtin(lang: &QueryLanguage) -> Vec<LiveTemplate> {
    let list = match lang_key(lang) {
        Some("sql") => SQL,
        Some("mql") => MQL,
        _ => &[],
    };
    list.iter()
        .map(|(a, d, b)| LiveTemplate {
            abbreviation: (*a).into(),
            description: (*d).into(),
            body: (*b).into(),
            builtin: true,
        })
        .collect()
}

#[derive(Deserialize)]
struct UserFile {
    #[serde(default)]
    template: Vec<UserTemplate>,
}

#[derive(Deserialize)]
struct UserTemplate {
    abbreviation: String,
    #[serde(default)]
    description: String,
    body: String,
    #[serde(default = "default_language")]
    language: String,
}

fn default_language() -> String {
    "sql".into()
}

/// User templates (language key, template) from a `templates.toml` text.
pub fn parse_user(text: &str) -> Result<Vec<(String, LiveTemplate)>, String> {
    let f: UserFile = toml::from_str(text).map_err(|e| e.to_string())?;
    Ok(f.template
        .into_iter()
        .map(|t| {
            (
                t.language.to_ascii_lowercase(),
                LiveTemplate {
                    abbreviation: t.abbreviation,
                    description: t.description,
                    body: t.body,
                    builtin: false,
                },
            )
        })
        .collect())
}

/// Templates of `lang`: user ones first (they override built-ins with the
/// same abbreviation).
pub fn for_language(lang: &QueryLanguage, user: &[(String, LiveTemplate)]) -> Vec<LiveTemplate> {
    let key = lang_key(lang);
    let mut out: Vec<LiveTemplate> = user
        .iter()
        .filter(|(l, _)| l == "any" || Some(l.as_str()) == key)
        .map(|(_, t)| t.clone())
        .collect();
    for b in builtin(lang) {
        if !out.iter().any(|t| t.abbreviation == b.abbreviation) {
            out.push(b);
        }
    }
    out
}

/// Expands a body: (text, selection relative to the text).
pub fn expand_body(body: &str, selection: Option<&str>) -> (String, Range<usize>) {
    let mut out = String::new();
    let mut first_var: Option<Range<usize>> = None;
    let mut end: Option<usize> = None;
    let mut rest = body;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix('$') {
            out.push('$');
            rest = r;
            continue;
        }
        let Some(j) = rest.find('$') else {
            out.push('$');
            continue;
        };
        let name = &rest[..j];
        rest = &rest[j + 1..];
        match name {
            "END" => end = Some(out.len()),
            "SELECTION" => out.push_str(selection.unwrap_or_default()),
            n if !n.is_empty() && n.chars().all(|c| c.is_alphanumeric() || c == '_') => {
                let start = out.len();
                out.push_str(n);
                if first_var.is_none() {
                    first_var = Some(start..out.len());
                }
            }
            other => {
                out.push('$');
                out.push_str(other);
                out.push('$');
            }
        }
    }
    out.push_str(rest);
    let sel = first_var.unwrap_or_else(|| {
        let e = end.unwrap_or(out.len());
        e..e
    });
    (out, sel)
}

/// Expands the template whose abbreviation ends at `offset`, or surrounds
/// `selection` with the first template using `$SELECTION$`.
pub fn expand(
    text: &str,
    offset: usize,
    selection: Option<Range<usize>>,
    templates: &[LiveTemplate],
) -> Option<TemplateExpansion> {
    if let Some(sel) = selection.filter(|s| !s.is_empty() && s.end <= text.len()) {
        let t = templates.iter().find(|t| t.body.contains("$SELECTION$"))?;
        let (out, selection) = expand_body(&t.body, Some(&text[sel.clone()]));
        return Some(TemplateExpansion {
            replace: sel,
            text: out,
            selection,
        });
    }
    let offset = offset.min(text.len());
    let head = &text[..offset];
    let start = head
        .char_indices()
        .rev()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
        .last()
        .map_or(offset, |(i, _)| i);
    let abbr = &text[start..offset];
    if abbr.is_empty() {
        return None;
    }
    let t = templates
        .iter()
        .find(|t| t.abbreviation == abbr)
        .or_else(|| templates.iter().find(|t| t.abbreviation.eq_ignore_ascii_case(abbr)))?;
    let (out, selection) = expand_body(&t.body, None);
    Some(TemplateExpansion {
        replace: start..offset,
        text: out,
        selection,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_and_selection() {
        let (t, s) = expand_body("SELECT * FROM $table$ WHERE $END$", None);
        assert_eq!(t, "SELECT * FROM table WHERE ");
        assert_eq!(&t[s], "table");
        let (t, s) = expand_body("LIMIT 10$END$;", None);
        assert_eq!((t.as_str(), s), ("LIMIT 10;", 8..8));
        let (t, _) = expand_body("{ $$match: {$END$} }", None);
        assert_eq!(t, "{ $match: {} }");
    }

    #[test]
    fn expand_at_cursor_and_surround() {
        let ts = builtin(&QueryLanguage::Sql("postgresql".into()));
        let e = expand("x;\nsel", 6, None, &ts).unwrap();
        assert_eq!(e.replace, 3..6);
        assert!(e.text.starts_with("SELECT * FROM table"));
        let e = expand("select 1", 0, Some(0..8), &ts).unwrap();
        assert!(e.text.contains("select 1"), "{}", e.text);
        assert!(expand("nothing_here", 12, None, &ts).is_none());
    }

    #[test]
    fn user_templates_override() {
        let user = parse_user(
            "[[template]]\nabbreviation = \"sel\"\ndescription = \"mine\"\nbody = \"SELECT 1\"\n\n[[template]]\nabbreviation = \"x\"\nbody = \"y\"\nlanguage = \"mql\"\n",
        )
        .unwrap();
        let ts = for_language(&QueryLanguage::Sql("mysql".into()), &user);
        assert_eq!(ts[0].description, "mine");
        assert_eq!(ts.iter().filter(|t| t.abbreviation == "sel").count(), 1);
        assert!(!ts.iter().any(|t| t.abbreviation == "x"));
        assert!(parse_user("not toml [").is_err());
    }
}
