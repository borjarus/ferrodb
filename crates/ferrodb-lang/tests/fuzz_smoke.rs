//! Fuzz smoke tests (plan, section 9): the harness of the cargo-fuzz
//! targets `lang` and `mql_value` (see `fuzz/`) on random and structured
//! input with stable Rust, plus a replay of the checked-in seed corpus and
//! of the crashes found so far. `PROPTEST_CASES` scales the run
//! (`cargo xtask fuzz-smoke` raises it).

use std::path::Path;

use ferrodb_lang::fuzzing::{language_service, language_service_bytes, languages, mql_value};
use proptest::prelude::*;

const ATOMS: &[&str] = &[
    // SQL of every dialect
    "select",
    "SELECT",
    " ",
    "\n",
    "\t",
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
    "procedure",
    "trigger",
    "function",
    "case",
    "when",
    "then",
    "if",
    "loop",
    "declare",
    "is",
    "as",
    ":p",
    "?",
    "$1",
    "@v",
    "#t",
    "q'[",
    "]'",
    "[",
    "]",
    "(",
    ")",
    ",",
    ".",
    "*",
    "=",
    "from",
    "where",
    "join",
    "on",
    "group by",
    "order by",
    "insert into",
    "values",
    "update",
    "set",
    "delete",
    "with",
    "union",
    "t",
    "public.",
    "table_1",
    "col_1",
    "id",
    "o",
    "1",
    "1.5e3",
    "\ngo\n",
    "GO 2",
    "\n/\n",
    "DELIMITER //\n",
    "//",
    "\\",
    "e'\\''",
    "x'ff'",
    "N'ü'",
    "é",
    "😀",
    "\u{feff}",
    "\r\n",
    "batch",
    "apply",
    "allow filtering",
    "top 10",
    "nvl(",
    "rownum",
    "calc_total(",
    "returning",
    // MQL
    "db.",
    "users",
    ".find(",
    ".aggregate([",
    "{",
    "}",
    "a:",
    "$gt",
    "$match",
    "ObjectId(\"",
    "ISODate(\"",
    "NumberDecimal(\"",
    "/re/i",
    "// t\n",
    "true",
    "null",
    // SQL++, PartiQL
    "`travel`.`inventory`.`airline`",
    "META()",
    "MISSING",
    "\"Music\"",
    "Artist",
    "<<",
    ">>",
    // Redis
    "GET ",
    "SET ",
    "HSET ",
    "user:1",
    "MULTI",
    "EXEC",
    "FLUSHALL",
    "KEYS *",
    "\"quoted\\\"\"",
];

fn structured() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(ATOMS), 0..48).prop_map(|v| v.concat())
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Every language feature runs on each input: keep the default run short.
        cases: std::env::var("PROPTEST_CASES").ok().and_then(|s| s.parse().ok()).unwrap_or(96),
        ..ProptestConfig::default()
    })]

    #[test]
    fn random_bytes(data in prop::collection::vec(any::<u8>(), 0..400)) {
        language_service_bytes(&data);
    }

    #[test]
    fn random_text(lang in 0..languages().len(), text in ".{0,200}", cursor in 0usize..400) {
        language_service(&languages()[lang], &text, cursor);
    }

    #[test]
    fn structured_text(lang in 0..languages().len(), text in structured(), cursor in 0usize..600) {
        language_service(&languages()[lang], &text, cursor);
    }

    #[test]
    fn mql_values(text in structured()) {
        mql_value(&text);
    }

    #[test]
    fn mql_values_random(text in ".{0,120}") {
        mql_value(&text);
    }
}

/// Every file of a corpus directory (seed inputs and minimized crashes).
fn replay(dir: &str, f: impl Fn(&[u8])) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz")
        .join(dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!("missing corpus {}", dir.display());
    };
    for e in entries.flatten() {
        let data = std::fs::read(e.path()).unwrap();
        f(&data);
    }
}

#[test]
fn seed_corpus() {
    replay("corpus/lang", language_service_bytes);
    replay("corpus/mql_value", |d| {
        mql_value(&String::from_utf8_lossy(d))
    });
}

#[test]
fn every_language_on_every_seed_text() {
    // The seeds without their 3-byte header, in every language.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/lang");
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let data = std::fs::read(e.path()).unwrap();
        let text = String::from_utf8_lossy(data.get(3..).unwrap_or_default());
        for lang in languages() {
            for cursor in [0, text.len() / 2, text.len()] {
                language_service(&lang, &text, cursor);
            }
        }
    }
}
