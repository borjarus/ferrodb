//! Property tests for the splitter (plan, section 9: statement splitting).

use ferrodb_lang::sql::{Flavor, parameters, split, substitute};
use proptest::prelude::*;

fn sql_text() -> impl Strategy<Value = String> {
    let atoms = prop::sample::select(vec![
        "select",
        " ",
        "\n",
        ";",
        "'",
        "''",
        "\"",
        "`",
        "--",
        "/*",
        "*/",
        "$$",
        "$a$",
        "begin",
        "end",
        "create",
        "trigger",
        "case",
        "if",
        ":p",
        "?",
        "$1",
        "é",
        "x",
        "1",
        "(",
        ")",
        "DELIMITER //\n",
        "//",
        "\\",
    ]);
    prop::collection::vec(atoms, 0..40).prop_map(|v| v.concat())
}

proptest! {
    #[test]
    fn ranges_are_ordered_and_in_bounds(src in sql_text()) {
        for dialect in ["postgresql", "mysql", "sqlite", "generic"] {
            let st = split(&src, &Flavor::for_dialect(dialect));
            let mut last = 0;
            for s in &st {
                prop_assert!(s.range.start <= s.body.start && s.body.end == s.range.end);
                prop_assert!(s.body.start < s.body.end && s.body.end <= src.len());
                prop_assert!(s.range.start >= last);
                prop_assert!(src.is_char_boundary(s.body.start) && src.is_char_boundary(s.body.end));
                prop_assert_eq!(s.text(&src).trim_start(), s.text(&src));
                last = s.body.end;
            }
        }
    }

    #[test]
    fn plain_statements_roundtrip(stmts in prop::collection::vec("[a-z][a-z0-9 ,=]{0,20}[a-z0-9]", 1..8)) {
        let src = stmts.join(";\n");
        let got: Vec<String> = split(&src, &Flavor::GENERIC).iter().map(|s| s.text(&src).to_owned()).collect();
        prop_assert_eq!(got, stmts);
    }

    #[test]
    fn substitution_removes_all_placeholders(src in sql_text()) {
        for dialect in ["postgresql", "mysql"] {
            let f = Flavor::for_dialect(dialect);
            let vals: Vec<_> = parameters(&src, &f).into_iter().map(|p| (p.name, Some("v".to_owned()))).collect();
            let (out, map) = substitute(&src, &f, &vals).unwrap();
            prop_assert!(parameters(&out, &f).is_empty() || out.contains("'v'"));
            prop_assert!(map.to_original(out.len()) <= src.len());
        }
    }
}
