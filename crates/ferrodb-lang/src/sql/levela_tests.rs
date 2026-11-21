//! T-SQL and Oracle at level A (phase 8.6): splitting (`GO`, blocks,
//! PL/SQL units and `/`), parameters, risks, name resolution of their
//! extensions, and the completion accuracy corpus (≥ 90 %, plan 2.10).

use ferrodb_api::{RiskKind, Severity};

use super::{
    Flavor, complete, document::Document, inspect, parameters, risks, schema::SchemaModel, split,
};

fn texts(src: &str, dialect: &str) -> Vec<String> {
    split(src, &Flavor::for_dialect(dialect))
        .iter()
        .map(|s| s.text(src).to_owned())
        .collect()
}

/// The shop schema; upper-case names for Oracle.
fn model(upper: bool) -> SchemaModel {
    let mut m = crate::sql::resolve::tests::shop(if upper { "APP" } else { "dbo" });
    if upper {
        for s in &mut m.schemas {
            s.name = s.name.to_uppercase();
            for t in &mut s.tables {
                t.name = t.name.to_uppercase();
                for c in &mut t.columns {
                    c.name = c.name.to_uppercase();
                }
                for f in &mut t.foreign_keys {
                    f.ref_table = f.ref_table.to_uppercase();
                    f.columns = f.columns.iter().map(|x| x.to_uppercase()).collect();
                    f.ref_columns = f.ref_columns.iter().map(|x| x.to_uppercase()).collect();
                }
            }
        }
        let names = std::mem::take(&mut m.schemas);
        return SchemaModel::new(names, vec!["APP".into()]);
    }
    m
}

// ------------------------------------------------------------- splitting

#[test]
fn tsql_go_batches_and_blocks() {
    let src = "SELECT 1\nGO\nSELECT 'GO'; SELECT 2\ngo 5\n-- c\nPRINT 'x'\nGO -- end\n";
    assert_eq!(
        texts(src, "tsql"),
        vec!["SELECT 1", "SELECT 'GO'", "SELECT 2", "PRINT 'x'"]
    );
    let src = "BEGIN TRY\n  INSERT INTO t VALUES (1);\n  UPDATE t SET a = 2;\nEND TRY\nBEGIN CATCH\n  SELECT ERROR_MESSAGE();\nEND CATCH;\nSELECT 3;";
    let t = texts(src, "tsql");
    assert_eq!(t.len(), 2, "{t:?}");
    assert!(t[0].ends_with("END CATCH"));
    let src = "IF @x > 0\nBEGIN\n  SELECT CASE WHEN a = 1 THEN 'y' END FROM t;\n  DELETE FROM t WHERE a = 1;\nEND\nELSE\nBEGIN\n  SELECT 0;\nEND;\nSELECT 4";
    assert_eq!(texts(src, "tsql").len(), 2);
    // A transaction BEGIN is not a block.
    assert_eq!(
        texts("BEGIN TRAN; DELETE FROM t WHERE id = 1; COMMIT;", "tsql").len(),
        3
    );
    // Routine bodies run to the end of the batch.
    let src = "CREATE OR ALTER PROCEDURE dbo.p @a int AS\nSET NOCOUNT ON;\nSELECT @a;\nUPDATE t SET x = @a;\nGO\nEXEC dbo.p 1;";
    let t = texts(src, "tsql");
    assert_eq!(t.len(), 2, "{t:?}");
    assert!(t[0].ends_with("UPDATE t SET x = @a;") || t[0].ends_with("UPDATE t SET x = @a"));
    let src = "CREATE TRIGGER trg ON t AFTER UPDATE AS BEGIN SELECT 1; END\nGO\nSELECT 1";
    assert_eq!(texts(src, "tsql").len(), 2);
    // Sybase ASE shares the rules.
    assert_eq!(texts("select 1\ngo\nselect 2", "sybase").len(), 2);
    // `[a;b]`, `#temp`, `@v` lex as one token.
    assert_eq!(
        texts("SELECT [a;b] INTO #t FROM x; SELECT @v", "tsql").len(),
        2
    );
}

#[test]
fn oracle_plsql_units_and_slash() {
    let src = "DECLARE\n  v NUMBER := 1;\n  CURSOR c IS SELECT id FROM t;\nBEGIN\n  FOR r IN c LOOP\n    IF r.id > 0 THEN\n      UPDATE t SET x = 1 WHERE id = r.id;\n    END IF;\n  END LOOP;\nEND;\n/\nSELECT * FROM dual;";
    let t = texts(src, "oracle");
    assert_eq!(t.len(), 2, "{t:?}");
    assert!(t[0].starts_with("DECLARE") && t[0].ends_with("END;") || t[0].ends_with("END"));
    assert_eq!(t[1], "SELECT * FROM dual");
    // Without `/`: the block still ends at its END.
    let src = "BEGIN\n  NULL;\nEND;\nSELECT 1 FROM dual;";
    assert_eq!(texts(src, "oracle").len(), 2);
    let src = "CREATE OR REPLACE PACKAGE BODY pkg AS\n  g NUMBER;\n  PROCEDURE p(a NUMBER) IS\n    v NUMBER;\n  BEGIN\n    SELECT 1 INTO v FROM dual;\n    CASE a WHEN 1 THEN NULL; ELSE NULL; END CASE;\n  END p;\n  FUNCTION f RETURN NUMBER IS BEGIN RETURN 1; END;\nBEGIN\n  g := 0;\nEND pkg;\n/\nCREATE OR REPLACE PACKAGE pkg AS\n  PROCEDURE p(a NUMBER);\n  FUNCTION f RETURN NUMBER;\nEND pkg;\n/\nCREATE OR REPLACE TRIGGER trg BEFORE INSERT ON t FOR EACH ROW\nBEGIN\n  :new.id := seq.NEXTVAL;\nEND;\n/\nCREATE TYPE pt AS OBJECT (x NUMBER, y NUMBER);\n/\nCREATE OR REPLACE TYPE BODY pt AS\n  MEMBER FUNCTION len RETURN NUMBER IS BEGIN RETURN 0; END;\nEND;\n/\nCREATE OR REPLACE FUNCTION f(a IN NUMBER) RETURN NUMBER AS\nBEGIN\n  RETURN a;\nEND f;\n/\nSELECT q'[it's; here]' FROM dual;";
    let t = texts(src, "oracle");
    assert_eq!(t.len(), 7, "{t:#?}");
    assert!(t[0].starts_with("CREATE OR REPLACE PACKAGE BODY") && t[0].ends_with("END pkg;"));
    assert!(t[1].ends_with("END pkg;"), "{}", t[1]);
    assert!(t[2].contains(":new.id"));
    assert_eq!(t[3], "CREATE TYPE pt AS OBJECT (x NUMBER, y NUMBER)");
    assert_eq!(t[6], "SELECT q'[it's; here]' FROM dual");
    // `/` inside an expression is division.
    assert_eq!(texts("SELECT 4\n/ 2 FROM dual;", "oracle").len(), 1);
}

// ----------------------------------------------------------- parameters

#[test]
fn parameters_by_dialect() {
    let names = |src: &str, d: &str| -> Vec<String> {
        parameters(src, &Flavor::for_dialect(d))
            .into_iter()
            .map(|p| p.name)
            .collect()
    };
    assert_eq!(
        names(
            "DECLARE @n int = 1, @s varchar(10);\nSELECT * FROM t WHERE id = @id AND n = @n AND x = @@ROWCOUNT AND s = '@no'",
            "tsql"
        ),
        vec!["id"]
    );
    assert!(
        names(
            "CREATE PROCEDURE p @a int, @b int OUTPUT AS SELECT @a, @b",
            "tsql"
        )
        .is_empty()
    );
    assert_eq!(
        names("SELECT * FROM emp WHERE empno = :id AND x = ':y'", "oracle"),
        vec!["id"]
    );
    assert!(
        names(
            "CREATE TRIGGER t BEFORE INSERT ON x FOR EACH ROW BEGIN :new.a := :old.a; END;",
            "oracle"
        )
        .is_empty()
    );
}

// ----------------------------------------------------------------- risks

#[test]
fn risks_inside_batches_and_blocks() {
    let kinds = |src: &str, d: &str| -> Vec<RiskKind> {
        risks(src, d, false).into_iter().map(|r| r.kind).collect()
    };
    assert_eq!(kinds("DELETE FROM t", "tsql"), vec![RiskKind::DeleteAll]);
    assert_eq!(
        kinds("UPDATE TOP (10) t SET a = 1", "tsql"),
        vec![RiskKind::UpdateAll]
    );
    assert!(kinds("DELETE FROM t WHERE id = 1", "tsql").is_empty());
    // Without semicolons in one batch.
    let src = "SELECT * FROM t\nDELETE FROM t\nSELECT 1 FROM x WHERE y = 1";
    let r = risks(src, "tsql", false);
    assert_eq!(r.len(), 1);
    assert_eq!(&src[r[0].range.clone()], "DELETE FROM t");
    let src = "BEGIN TRY\n  UPDATE t SET a = 1;\n  UPDATE t SET a = 2 WHERE b = 1;\nEND TRY\nBEGIN CATCH\nEND CATCH";
    assert_eq!(kinds(src, "tsql"), vec![RiskKind::UpdateAll]);
    // Not statements: trigger events, MERGE actions, IF UPDATE(col), FOR UPDATE.
    let src =
        "CREATE TRIGGER trg ON t AFTER INSERT, UPDATE, DELETE AS BEGIN IF UPDATE(a) SELECT 1 END";
    assert!(kinds(src, "tsql").is_empty(), "{:?}", kinds(src, "tsql"));
    let src = "MERGE INTO t USING s ON t.id = s.id WHEN MATCHED THEN UPDATE SET a = s.a WHEN NOT MATCHED BY SOURCE THEN DELETE;";
    assert!(kinds(src, "tsql").is_empty(), "{:?}", kinds(src, "tsql"));
    let src = "BEGIN\n  DELETE FROM emp;\nEND;\n/\nSELECT * FROM emp FOR UPDATE;\nCREATE TRIGGER x BEFORE DELETE OR UPDATE ON emp FOR EACH ROW BEGIN NULL; END;\n/";
    assert_eq!(kinds(src, "oracle"), vec![RiskKind::DeleteAll]);
    assert_eq!(
        kinds("DROP TABLE t; TRUNCATE TABLE u", "oracle"),
        vec![RiskKind::Drop, RiskKind::Truncate]
    );
}

// ------------------------------------------------------ name resolution

fn errors(src: &str, dialect: &str, m: &SchemaModel) -> Vec<String> {
    let doc = Document::analyze(src, dialect, m, None);
    inspect::diagnostics(&doc)
        .into_iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", &src[d.range.clone()], d.message))
        .collect()
}

#[test]
fn tsql_extensions_resolve() {
    let m = model(false);
    for ok in [
        "SELECT TOP (10) PERCENT WITH TIES o.id, o.total FROM orders o ORDER BY o.total",
        "SELECT TOP 5 c.name, x.total FROM customers c CROSS APPLY (SELECT TOP 1 o.total FROM orders o WHERE o.customer_id = c.id) x",
        "SELECT c.name FROM customers c OUTER APPLY (SELECT 1 AS one) a",
        "SELECT o.id FROM orders o WITH (NOLOCK) JOIN customers AS c WITH (NOLOCK, INDEX(ix)) ON c.id = o.customer_id",
        "INSERT INTO orders (customer_id, total) OUTPUT inserted.id, inserted.total VALUES (1, 2)",
        "UPDATE orders SET total = 0 OUTPUT deleted.total, inserted.total WHERE id = 1",
        "DELETE FROM orders OUTPUT deleted.id WHERE id = 1",
        "SELECT id, total INTO #big FROM orders WHERE total > 100",
        "CREATE TABLE #t (id int, name nvarchar(10)); SELECT id, name FROM #t",
        "SELECT * FROM ##global_tmp",
        "SELECT x.a FROM @tv x",
        "MERGE INTO orders AS t USING customers AS s ON t.customer_id = s.id WHEN MATCHED THEN UPDATE SET total = 0 WHEN NOT MATCHED BY TARGET THEN INSERT (customer_id, total) VALUES (s.id, 0) OUTPUT $action, inserted.id;",
        "SELECT name FROM sys.objects; SELECT TABLE_NAME FROM INFORMATION_SCHEMA.TABLES",
        "SELECT o.id FROM dbo.orders o OPTION (MAXDOP 1, RECOMPILE)",
        "INSERT orders (customer_id) VALUES (1)",
    ] {
        let e = errors(ok, "tsql", &m);
        assert!(e.is_empty(), "{ok}\n{e:?}");
    }
    // Errors are still found.
    assert!(
        !errors(
            "SELECT o.nme FROM orders o CROSS APPLY (SELECT 1 AS x) a",
            "tsql",
            &m
        )
        .is_empty()
    );
    assert!(
        !errors(
            "MERGE INTO ordrs t USING customers s ON 1 = 1 WHEN MATCHED THEN DELETE;",
            "tsql",
            &m
        )
        .is_empty()
    );
    assert!(
        !errors(
            "INSERT INTO orders (customer_id) OUTPUT inserted.nope VALUES (1)",
            "tsql",
            &m
        )
        .is_empty()
    );
}

#[test]
fn oracle_extensions_resolve() {
    let m = model(true);
    for ok in [
        "SELECT ROWNUM, o.id FROM orders o WHERE ROWNUM <= 10",
        "SELECT SYSDATE, USER FROM dual",
        "SELECT order_seq.NEXTVAL FROM dual",
        "INSERT INTO orders (id, customer_id) VALUES (order_seq.NEXTVAL, 1)",
        "SELECT LEVEL, id, name FROM customers START WITH id = 1 CONNECT BY PRIOR id = id",
        "SELECT id FROM customers CONNECT BY NOCYCLE PRIOR id = id",
        "SELECT table_name FROM user_tables UNION ALL SELECT table_name FROM all_tables",
        "SELECT sid FROM v$session",
        "SELECT * FROM sys.dba_users",
        "MERGE INTO orders t USING (SELECT id, name FROM customers) s ON (t.customer_id = s.id) WHEN MATCHED THEN UPDATE SET t.total = 0 WHEN NOT MATCHED THEN INSERT (customer_id) VALUES (s.id)",
        "SELECT o.id FROM orders o FETCH FIRST 10 ROWS ONLY",
        "SELECT q'[x; y]' FROM dual",
        "BEGIN pkg.proc(1); END;",
        "SELECT id INTO v_id FROM orders WHERE id = 1",
    ] {
        let e = errors(ok, "oracle", &m);
        assert!(e.is_empty(), "{ok}\n{e:?}");
    }
    assert!(!errors("SELECT nme FROM customers", "oracle", &m).is_empty());
    assert!(!errors("SELECT * FROM custmers", "oracle", &m).is_empty());
}

#[test]
fn update_delete_without_where_inspection() {
    let m = model(false);
    let doc = Document::analyze(
        "DELETE FROM orders; UPDATE orders SET total = 1 OUTPUT inserted.id",
        "tsql",
        &m,
        None,
    );
    let w: Vec<_> = inspect::diagnostics(&doc)
        .into_iter()
        .filter(|d| d.severity == Severity::Warning)
        .collect();
    assert_eq!(w.len(), 2, "{w:?}");
    let m = model(true);
    let doc = Document::analyze("DELETE FROM orders", "oracle", &m, None);
    assert!(
        inspect::diagnostics(&doc)
            .iter()
            .any(|d| d.message.contains("without WHERE"))
    );
}

// ---------------------------------------------------- completion corpus

/// (console with `|` at the cursor, label among the first three suggestions).
const TSQL_CORPUS: &[(&str, &str)] = &[
    ("SELECT * FROM |", "customers"),
    ("SELECT * FROM ord|", "orders"),
    ("SELECT * FROM cust|", "customers"),
    ("SELECT TOP 10 * FROM ord|", "orders"),
    ("SELECT TOP (5) o.| FROM orders o", "customer_id"),
    ("SELECT o.tot| FROM orders o", "total"),
    ("SELECT * FROM orders o WHERE o.st|", "status"),
    (
        "SELECT * FROM orders o JOIN customers c ON c.id = o.cu|",
        "customer_id",
    ),
    (
        "SELECT * FROM orders o WITH (NOLOCK) WHERE o.|",
        "customer_id",
    ),
    (
        "SELECT * FROM orders o CROSS APPLY (SELECT TOP 1 * FROM order_items i WHERE i.|",
        "order_id",
    ),
    ("SELECT c.name FROM customers c CROSS APPLY |", "customers"),
    (
        "SELECT c.name FROM customers c OUTER APPLY (SELECT * FROM ord|",
        "orders",
    ),
    ("INSERT INTO orders (cust|", "customer_id"),
    (
        "INSERT INTO orders (customer_id) OUTPUT inserted.to| VALUES (1)",
        "total",
    ),
    ("UPDATE orders SET st|", "status"),
    (
        "UPDATE orders SET total = 0 OUTPUT deleted.| WHERE id = 1",
        "customer_id",
    ),
    ("DELETE FROM ord|", "orders"),
    ("DELETE FROM orders WHERE stat|", "status"),
    ("MERGE INTO |", "customers"),
    ("MERGE INTO orders AS t USING |", "customers"),
    (
        "MERGE INTO orders AS t USING customers AS s ON t.customer_id = s.| ",
        "id",
    ),
    (
        "MERGE INTO orders AS t USING customers AS s ON t.customer_id = s.id WHEN MATCHED THEN UPDATE SET to|",
        "total",
    ),
    ("SELECT * INTO #t FROM ord|", "orders"),
    ("SELECT getd|", "getdate"),
    (
        "SELECT dateadd(day, 1, o.created_at), o.| FROM orders o",
        "customer_id",
    ),
    ("SELECT isn|", "isnull"),
    ("SELECT string_ag|", "string_agg"),
    ("SELECT * FROM orders ORDER BY tot|", "total"),
    (
        "SELECT customer_id, count(*) FROM orders GROUP BY cust|",
        "customer_id",
    ),
    ("SELECT * FROM orders o JOIN |", "customers"),
    (
        "SELECT * FROM orders o LEFT JOIN order_items i ON i.|",
        "order_id",
    ),
    ("SELECT * FROM audit.|", "log"),
    ("SELECT * FROM dbo.ord|", "orders"),
    ("DECLARE @n int;\nSELECT @n = count(*) FROM ord|", "orders"),
    (
        "BEGIN TRY\n  SELECT * FROM orders o WHERE o.|\nEND TRY\nBEGIN CATCH\nEND CATCH",
        "customer_id",
    ),
    (
        "IF EXISTS (SELECT 1 FROM orders)\nBEGIN\n  UPDATE orders SET sta|\nEND",
        "status",
    ),
    (
        "CREATE PROCEDURE p AS\nBEGIN\n  SELECT * FROM cust|\nEND",
        "customers",
    ),
    (
        "CREATE PROCEDURE p @id int AS\nSELECT c.| FROM customers c WHERE c.id = @id",
        "email",
    ),
    ("SELECT 1\nGO\nSELECT * FROM ord|", "orders"),
    (
        "WITH big AS (SELECT * FROM orders WHERE total > 100) SELECT * FROM b|",
        "big",
    ),
    (
        "SELECT * FROM orders WHERE total > (SELECT avg(price) FROM order_items i WHERE i.|)",
        "price",
    ),
    (
        "SELECT * FROM orders WHERE EXISTS (SELECT 1 FROM customers c WHERE c.|)",
        "email",
    ),
    (
        "SELECT c.name, o.total FROM customers c JOIN orders o ON o.customer_id = c.id WHERE c.em|",
        "email",
    ),
    ("SELECT * FROM products p WHERE p.cat|", "category_id"),
    ("SELECT ne|", "newid"),
];

const ORACLE_CORPUS: &[(&str, &str)] = &[
    ("SELECT * FROM |", "CUSTOMERS"),
    ("SELECT * FROM ord|", "ORDERS"),
    ("SELECT * FROM cust|", "CUSTOMERS"),
    ("SELECT o.| FROM orders o", "CUSTOMER_ID"),
    ("SELECT o.tot| FROM orders o", "TOTAL"),
    ("SELECT * FROM orders o WHERE o.st|", "STATUS"),
    (
        "SELECT * FROM orders o WHERE ROWNUM <= 10 AND o.|",
        "CUSTOMER_ID",
    ),
    (
        "SELECT * FROM orders o JOIN customers c ON c.id = o.cu|",
        "CUSTOMER_ID",
    ),
    ("SELECT nv|", "nvl"),
    ("SELECT to_ch|", "to_char"),
    ("SELECT dec|", "decode"),
    ("SELECT listag|", "listagg"),
    ("SELECT sysd|", "SYSDATE"),
    ("SELECT * FROM du|", "dual"),
    ("SELECT order_seq.| FROM dual", "NEXTVAL"),
    (
        "SELECT id, name FROM customers START WITH id = 1 CONNECT BY PRIOR id = |",
        "ID",
    ),
    (
        "SELECT c.name FROM customers c CROSS APPLY (SELECT * FROM orders o WHERE o.|)",
        "CUSTOMER_ID",
    ),
    ("INSERT INTO orders (cust|", "CUSTOMER_ID"),
    ("UPDATE orders SET st|", "STATUS"),
    ("DELETE FROM ord|", "ORDERS"),
    (
        "MERGE INTO orders t USING customers s ON (t.customer_id = s.|)",
        "ID",
    ),
    (
        "MERGE INTO orders t USING customers s ON (t.customer_id = s.id) WHEN MATCHED THEN UPDATE SET t.to|",
        "TOTAL",
    ),
    ("SELECT * FROM orders ORDER BY tot|", "TOTAL"),
    (
        "SELECT * FROM orders o LEFT JOIN order_items i ON i.|",
        "ORDER_ID",
    ),
    ("SELECT * FROM audit.|", "LOG"),
    ("SELECT * FROM orders o ORDER BY o.tot|", "TOTAL"),
    ("BEGIN\n  UPDATE orders SET sta|\nEND;", "STATUS"),
    (
        "DECLARE\n  v NUMBER;\nBEGIN\n  SELECT count(*) INTO v FROM ord|;\nEND;",
        "ORDERS",
    ),
    (
        "DECLARE\n  v NUMBER;\nBEGIN\n  SELECT o.| INTO v FROM orders o;\nEND;",
        "CUSTOMER_ID",
    ),
    ("DECLARE\n  v customers.|", "EMAIL"),
    (
        "CREATE OR REPLACE PROCEDURE p AS\nBEGIN\n  DELETE FROM orders WHERE stat|;\nEND;",
        "STATUS",
    ),
    (
        "BEGIN\n  FOR r IN (SELECT * FROM cust|) LOOP\n    NULL;\n  END LOOP;\nEND;",
        "CUSTOMERS",
    ),
    (
        "BEGIN\n  IF 1 = 1 THEN\n    INSERT INTO orders (tot|) VALUES (1);\n  END IF;\nEND;",
        "TOTAL",
    ),
    ("WITH big AS (SELECT * FROM orders) SELECT * FROM b|", "big"),
    (
        "SELECT * FROM orders WHERE EXISTS (SELECT 1 FROM customers c WHERE c.|)",
        "EMAIL",
    ),
    ("SELECT c.name FROM customers c WHERE c.em|", "EMAIL"),
    ("SELECT * FROM products p WHERE p.cat|", "CATEGORY_ID"),
    ("SELECT add_mo|", "add_months"),
    ("SELECT regexp_su|", "regexp_substr"),
    ("SELECT * FROM orders o JOIN |", "CUSTOMERS"),
];

fn accuracy(dialect: &str, corpus: &[(&str, &str)], m: &SchemaModel) {
    let mut misses = Vec::new();
    for (case, expected) in corpus {
        let offset = case.find('|').expect("cursor");
        let text = case.replace('|', "");
        let doc = Document::analyze(&text, dialect, m, None);
        let items = complete::complete(&doc, offset, &[]);
        let top: Vec<&str> = items.iter().take(3).map(|s| s.label.as_str()).collect();
        // A join suggestion `customers c ON …` counts for `customers`.
        let hit = |l: &&str| {
            l.eq_ignore_ascii_case(expected)
                || l.to_ascii_lowercase()
                    .starts_with(&format!("{} ", expected.to_ascii_lowercase()))
        };
        if !top.iter().any(hit) {
            misses.push(format!("{case:?}: expected {expected:?}, got {top:?}"));
        }
    }
    let acc = 1.0 - misses.len() as f64 / corpus.len() as f64;
    println!(
        "{dialect} suggestion accuracy: {:.1} % ({} cases)\n{}",
        acc * 100.0,
        corpus.len(),
        misses.join("\n")
    );
    assert!(
        acc >= 0.9,
        "{dialect}: accuracy {acc:.3} below 90 %:\n{}",
        misses.join("\n")
    );
}

#[test]
fn tsql_suggestion_accuracy() {
    accuracy("tsql", TSQL_CORPUS, &model(false));
}

#[test]
fn oracle_suggestion_accuracy() {
    accuracy("oracle", ORACLE_CORPUS, &model(true));
}

// ----------------------------------------------------------- performance

fn timed(dialect: &str, unit: &str) {
    let mut src = String::new();
    while src.lines().count() < 5000 {
        src.push_str(unit);
    }
    let lines = src.lines().count();
    let m = model(dialect == "oracle");
    // Warm up the dialect registry.
    let _ = Document::analyze("SELECT 1", dialect, &m, None);
    // First run: every statement parsed; then edits re-parse only the
    // edited statement (one line changed per run).
    let t = std::time::Instant::now();
    let doc = Document::analyze(&src, dialect, &m, None);
    std::hint::black_box(inspect::diagnostics(&doc));
    let cold = t.elapsed();
    let mut best = std::time::Duration::MAX;
    for k in 0..3 {
        let edited = src.replacen("o.total > 10", &format!("o.total > {}", 11 + k), 1);
        let t = std::time::Instant::now();
        let doc = Document::analyze(&edited, dialect, &m, None);
        let d = inspect::diagnostics(&doc);
        std::hint::black_box(d);
        best = best.min(t.elapsed());
    }
    println!("{dialect}: diagnostics for {lines} lines in {best:?} after an edit ({cold:?} cold)");
    // Debug builds are several times slower than release; the budget is
    // for release, so debug gets a proportional allowance.
    let budget = if cfg!(debug_assertions) { 1000 } else { 100 };
    assert!(
        best.as_millis() < budget,
        "{dialect}: {best:?} for {lines} lines"
    );
}

#[test]
fn tsql_diagnostics_latency_5k_lines() {
    timed(
        "tsql",
        "SELECT TOP 10 o.id, o.total, c.name\nFROM orders o WITH (NOLOCK)\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'new' AND o.total > 10;\nUPDATE orders SET status = 'x' OUTPUT inserted.id WHERE id = 1;\nGO\nBEGIN TRY\n  DELETE FROM order_items WHERE order_id = 1;\nEND TRY\nBEGIN CATCH\n  SELECT ERROR_MESSAGE();\nEND CATCH;\n",
    );
}

#[test]
fn oracle_diagnostics_latency_5k_lines() {
    timed(
        "oracle",
        "SELECT o.id, o.total, c.name\nFROM orders o\nJOIN customers c ON c.id = o.customer_id\nWHERE o.status = 'new' AND ROWNUM <= 10;\nBEGIN\n  UPDATE orders SET status = 'x' WHERE id = 1;\n  COMMIT;\nEND;\n/\nSELECT order_seq.NEXTVAL FROM dual;\n",
    );
}
