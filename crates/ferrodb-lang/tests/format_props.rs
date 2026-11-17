//! The SQL formatter is idempotent and keeps every significant token, for
//! random scripts built from SQL fragments, comments and whitespace.

use ferrodb_api::FormatStyle;
use ferrodb_lang::sql::{format::format, lexer::{TokenKind, tokenize}, Flavor};
use proptest::prelude::*;

const FRAGMENTS: &[&str] = &[
    "select", "a", "b.c", "*", ",", "from", "t", "u x", "join", "left join", "on", "=", "and",
    "or", "where", "group by", "order by", "desc", "limit 5", "(", ")", "select 1", "'s;tr'",
    "1.5", "-", "+", "<>", ">=", "::int", "in (1, 2)", "between 1 and 2", "case when", "then",
    "else", "end", "count(*)", "union all", "insert into t (a) values (1)", "update t set a = 1",
    "delete from t", "with q as (select 1)", ";", "-- note\n", "/* c */", "\n", "  ", "\n\n",
    "\"Quoted\"", "$1", "is null", "not", "exists", "distinct",
];

fn significant(s: &str, dialect: &str) -> String {
    let f = Flavor::for_dialect(dialect);
    tokenize(s, &f)
        .into_iter()
        .filter(|t| t.kind != TokenKind::Whitespace && t.text(s) != ";")
        .map(|t| {
            let x = t.text(s);
            if t.kind == TokenKind::LineComment {
                x.trim_end().to_lowercase()
            } else {
                x.to_lowercase()
            }
        })
        .collect::<Vec<_>>()
        .concat()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(400))]

    #[test]
    fn idempotent_and_token_preserving(
        parts in prop::collection::vec(prop::sample::select(FRAGMENTS), 1..40),
        dialect in prop::sample::select(&["postgresql", "mysql", "sqlite"][..]),
    ) {
        let src = parts.join(" ");
        let style = FormatStyle::default();
        let once = format(&src, dialect, &style);
        let twice = format(&once, dialect, &style);
        prop_assert_eq!(&once, &twice, "source: {:?}", src);
        prop_assert_eq!(significant(&src, dialect), significant(&once, dialect), "source: {:?}", src);
    }
}
