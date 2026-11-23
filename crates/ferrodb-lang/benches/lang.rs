//! Regression benchmarks of the editor's language features (plan, section
//! 3: key response < 16 ms in a 50k-line file, completion < 50 ms with 10k
//! objects). Budgets: `benches/budgets.toml`; `cargo xtask bench-check`.

use std::sync::{Arc, LazyLock};

use divan::{Bencher, black_box, counter::BytesCount};
use ferrodb_api::{DocSnapshot, FormatStyle, QueryLanguage};
use ferrodb_lang::{
    Engine,
    fuzzing::sample_model,
    mql,
    service::{self, NoSqlSchema},
    sql::{self, Flavor, schema::SchemaModel},
};

fn main() {
    divan::main();
}

const POSTGRES_BLOCK: &str = "\
-- Orders of a customer
SELECT o.id, o.col_1, c.col_3
  FROM table_12 o
  JOIN table_11 c ON c.id = o.col_2
 WHERE o.id > :min AND c.col_1 LIKE 'a%' -- comment ; not a separator
 ORDER BY 1;
INSERT INTO table_3 (id, col_1) VALUES (1, 'it''s'), (2, E'x\\ny');
CREATE FUNCTION f_x() RETURNS int AS $$ BEGIN RETURN 1; END $$ LANGUAGE plpgsql;
UPDATE table_4 SET col_1 = /* inline; */ 'v' WHERE id = $1;
";

const TSQL_BLOCK: &str = "\
SELECT TOP 10 [a b], @v FROM dbo.t WITH (NOLOCK) WHERE x = N'ż'
GO
BEGIN TRY
  UPDATE t SET a = 1 WHERE id = @id;
END TRY
BEGIN CATCH
  SELECT ERROR_MESSAGE();
END CATCH;
DECLARE @n int = 3;
GO
";

const MQL_BLOCK: &str = "\
// users over 30
db.users.find({ age: { $gt: 30 }, name: /^a/i }).sort({ age: -1 }).limit(5)
db.orders.aggregate([
  { $match: { status: \"A\" } },
  { $group: { _id: \"$cust_id\", total: { $sum: \"$amount\" } } }
]);
db.users.updateOne({ _id: ObjectId(\"64b7f0c2a1b2c3d4e5f60718\") }, { $set: { a: 1 } })
";

/// About `lines` lines of `block` repeated.
fn script(block: &str, lines: usize) -> String {
    let per = block.lines().count();
    block.repeat(lines.div_ceil(per))
}

static PG_50K: LazyLock<String> = LazyLock::new(|| script(POSTGRES_BLOCK, 50_000));
static TSQL_50K: LazyLock<String> = LazyLock::new(|| script(TSQL_BLOCK, 50_000));
static MQL_50K: LazyLock<String> = LazyLock::new(|| script(MQL_BLOCK, 50_000));
static MODEL_10K: LazyLock<SchemaModel> = LazyLock::new(|| sample_model(10_000));

fn doc(text: &str, language: QueryLanguage) -> DocSnapshot {
    DocSnapshot {
        text: Arc::from(text),
        language,
        data_source: None,
        schema: None,
        version: 1,
    }
}

/// Statement splitting of a 50k-line script (run on every edit).
mod split {
    use super::*;

    #[divan::bench]
    fn postgresql_50k_lines(b: Bencher) {
        let f = Flavor::for_dialect("postgresql");
        b.counter(BytesCount::of_str(&*PG_50K))
            .bench(|| sql::split(black_box(&PG_50K), &f));
    }

    #[divan::bench]
    fn tsql_50k_lines(b: Bencher) {
        let f = Flavor::for_dialect("tsql");
        b.counter(BytesCount::of_str(&*TSQL_50K))
            .bench(|| sql::split(black_box(&TSQL_50K), &f));
    }

    #[divan::bench]
    fn mql_50k_lines(b: Bencher) {
        b.counter(BytesCount::of_str(&*MQL_50K))
            .bench(|| mql::split(black_box(&MQL_50K)));
    }
}

/// Lexing of a 50k-line script (highlighting, analysis input).
mod tokenize {
    use super::*;

    #[divan::bench]
    fn postgresql_50k_lines(b: Bencher) {
        let f = Flavor::for_dialect("postgresql");
        b.counter(BytesCount::of_str(&*PG_50K))
            .bench(|| sql::lexer::tokenize(black_box(&PG_50K), &f));
    }

    #[divan::bench]
    fn mql_50k_lines(b: Bencher) {
        b.counter(BytesCount::of_str(&*MQL_50K))
            .bench(|| mql::lex(black_box(&MQL_50K)));
    }
}

/// The formatter on a 2k-line script ("Reformat code" on a whole console).
mod format {
    use super::*;

    #[divan::bench]
    fn postgresql_2k_lines(b: Bencher) {
        let text = script(POSTGRES_BLOCK, 2_000);
        let style = FormatStyle::default();
        b.counter(BytesCount::of_str(&text))
            .bench(|| sql::format::format(black_box(&text), "postgresql", &style));
    }

    #[divan::bench]
    fn mql_2k_lines(b: Bencher) {
        let text = script(MQL_BLOCK, 2_000);
        b.bench(|| mql::format(black_box(&text)));
    }
}

/// Completion with 10k tables in the schema model (budget: 50 ms).
mod complete {
    use super::*;

    fn run(b: Bencher, text: &str) {
        let nosql = NoSqlSchema::default();
        let engine = Engine::new(&MODEL_10K, &[]).with_nosql(&nosql);
        let d = doc(text, QueryLanguage::sql("postgresql"));
        let offset = text.len();
        b.bench(|| engine.complete(black_box(&d), offset));
    }

    /// Table names: every one of the 10k tables is a candidate.
    #[divan::bench]
    fn tables_10k(b: Bencher) {
        run(b, "SELECT * FROM tab");
    }

    /// Columns of an aliased table after a join.
    #[divan::bench]
    fn columns_10k(b: Bencher) {
        run(
            b,
            "SELECT * FROM table_42 o JOIN table_41 c ON c.id = o.col_2 WHERE o.",
        );
    }

    /// The cursor in the middle of a 50k-line console.
    #[divan::bench]
    fn in_50k_line_console(b: Bencher) {
        let mut text = PG_50K.clone();
        text.push_str("SELECT * FROM table_7 t WHERE t.");
        run(b, &text);
    }
}

/// Inspections of a 1k-line script with the 10k-table model.
#[divan::bench]
fn diagnostics_1k_lines(b: Bencher) {
    let text = script(POSTGRES_BLOCK, 1_000);
    let engine = Engine::new(&MODEL_10K, &[]);
    let d = doc(&text, QueryLanguage::sql("postgresql"));
    b.bench(|| engine.diagnostics(black_box(&d)));
}

/// Statement spans through the service (what the console asks on edit).
#[divan::bench]
fn statements_service_50k_lines(b: Bencher) {
    let d = doc(&PG_50K, QueryLanguage::sql("postgresql"));
    b.bench(|| service::statements(black_box(&d)));
}
