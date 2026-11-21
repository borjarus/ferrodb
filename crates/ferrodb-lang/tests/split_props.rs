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
        "go",
        "\ngo\n",
        "\nGO 3\n",
        "\n/\n",
        "declare",
        "procedure",
        "is",
        "loop",
        "@v",
        "#t",
        "q'[",
        "]'",
        "[",
        "]",
        "batch",
        "apply",
    ]);
    prop::collection::vec(atoms, 0..40).prop_map(|v| v.concat())
}

proptest! {
    #[test]
    fn ranges_are_ordered_and_in_bounds(src in sql_text()) {
        for dialect in ["postgresql", "mysql", "sqlite", "generic", "tsql", "oracle", "cql", "sqlpp", "googlesql"] {
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

fn plain() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec("select [a-z][a-z0-9 ,=]{0,20}[a-z0-9]", 1..8)
}

proptest! {
    /// T-SQL: `GO [n]` alone on its line separates statements, with or
    /// without `;`, and never ends up inside one.
    #[test]
    fn go_separates_batches(stmts in plain(), counts in prop::collection::vec(prop::option::of(1u8..9), 8), semis in prop::collection::vec(any::<bool>(), 8)) {
        let mut src = String::new();
        for (i, s) in stmts.iter().enumerate() {
            src.push_str(s);
            if semis[i] {
                src.push(';');
            }
            match counts[i] {
                Some(n) => src.push_str(&format!("\nGO {n}\n")),
                None => src.push_str("\ngo\n"),
            }
        }
        let got: Vec<String> = split(&src, &Flavor::for_dialect("tsql")).iter().map(|s| s.text(&src).to_owned()).collect();
        prop_assert_eq!(got, stmts);
    }

    /// Oracle: PL/SQL blocks separated by `/` lines stay whole (their `;`
    /// included), plain statements around them split at `;`.
    #[test]
    fn slash_ends_plsql_blocks(bodies in prop::collection::vec("[a-z]{1,8} := [0-9]{1,3}", 1..6), with_slash in prop::collection::vec(any::<bool>(), 6)) {
        let mut src = String::new();
        let mut want = Vec::new();
        for (i, b) in bodies.iter().enumerate() {
            let block = format!("BEGIN\n  {b};\n  IF 1 = 1 THEN NULL; END IF;\nEND;");
            src.push_str(&block);
            src.push_str(if with_slash[i] { "\n/\n" } else { "\n" });
            want.push(block);
            src.push_str("SELECT 1 FROM dual;\n");
            want.push("SELECT 1 FROM dual".to_owned());
        }
        let got: Vec<String> = split(&src, &Flavor::for_dialect("oracle")).iter().map(|s| s.text(&src).to_owned()).collect();
        prop_assert_eq!(got, want);
    }
}
