//! Tabular edits → SQL (plan, section 5.6): one `UPDATE` per edited row
//! with `WHERE` on the row key (original values), `INSERT` for new rows
//! (columns left empty take their defaults), `DELETE` by key. Every
//! statement must change exactly one row; the executor checks it.

use ferrodb_api::{Change, ChangeSet};

use crate::{
    sql::{Family, Sql},
    value::{Value, ValueType},
};

/// The table behind a result.
#[derive(Clone, Debug, PartialEq)]
pub struct EditTable {
    /// Qualified, quoted table name.
    pub table: String,
    /// Per result column: the table column (`None` = computed).
    pub columns: Vec<Option<String>>,
    /// Per result column: type edited values are converted to.
    pub types: Vec<ValueType>,
    /// Result columns identifying a row.
    pub key: Vec<usize>,
}

/// A generated statement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    pub sql: String,
    /// The statement must affect exactly one row.
    pub expect_one: bool,
}

/// Plans `changes`. `original(row, column)` gives the value a result cell
/// had when it was read (`None` = the row is not known any more).
pub fn plan(
    sql: &Sql,
    t: &EditTable,
    changes: &ChangeSet,
    original: impl Fn(usize, usize) -> Option<Value>,
) -> Result<Vec<Planned>, String> {
    let column = |c: usize| -> Result<&str, String> {
        t.columns
            .get(c)
            .ok_or_else(|| format!("column {} is out of range", c + 1))?
            .as_deref()
            .ok_or_else(|| format!("column {} is computed and cannot be edited", c + 1))
    };
    let key_of = |row: usize| -> Result<Vec<(String, Value)>, String> {
        if t.key.is_empty() {
            return Err("the result has no columns identifying its rows".into());
        }
        t.key
            .iter()
            .map(|&k| {
                let v = original(row, k)
                    .ok_or_else(|| format!("row {} is no longer available for editing", row + 1))?;
                Ok((column(k)?.to_owned(), v))
            })
            .collect()
    };
    let value = |c: usize, json: &str| -> Result<Value, String> {
        let ty = t.types.get(c).copied().unwrap_or(ValueType::Any);
        Value::from_cell_json(json, ty).map_err(|e| format!("{}: {e}", column(c).unwrap_or("?")))
    };

    // UPDATE: cells grouped by row, in row order.
    let mut updates: std::collections::BTreeMap<usize, Vec<(String, Value)>> = Default::default();
    let mut inserts = Vec::new();
    let mut deletes = Vec::new();
    for ch in &changes.changes {
        match ch {
            Change::SetCell {
                row,
                column: c,
                value: v,
            } => {
                let name = column(*c)?.to_owned();
                let val = value(*c, v)?;
                let set = updates.entry(*row).or_default();
                set.retain(|(n, _)| n != &name);
                set.push((name, val));
            }
            Change::InsertRow { values } => {
                let mut cols = Vec::new();
                let mut vals = Vec::new();
                for (c, v) in values.iter().enumerate() {
                    let Some(v) = v else { continue };
                    cols.push(column(c)?.to_owned());
                    vals.push(value(c, v)?);
                }
                inserts.push(if cols.is_empty() {
                    match sql.family {
                        Family::MySql => format!("INSERT INTO {} () VALUES ()", t.table),
                        _ => format!("INSERT INTO {} DEFAULT VALUES", t.table),
                    }
                } else {
                    sql.insert_row(&t.table, &cols, vals)
                });
            }
            Change::DeleteRow { row } => deletes.push(*row),
            other => return Err(format!("{other:?} is not a table change")),
        }
    }
    let mut out = Vec::new();
    for (row, set) in updates {
        if deletes.contains(&row) {
            continue;
        }
        out.push(Planned {
            sql: sql.update(&t.table, &set, &key_of(row)?, true),
            expect_one: true,
        });
    }
    out.extend(inserts.into_iter().map(|s| Planned {
        sql: s,
        expect_one: true,
    }));
    for row in deletes {
        out.push(Planned {
            sql: sql.delete(&t.table, &key_of(row)?, true),
            expect_one: true,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_statements() {
        let sql = Sql::new("postgresql");
        let t = EditTable {
            table: "public.orders".into(),
            columns: vec![Some("id".into()), Some("status".into()), None],
            types: vec![ValueType::Int, ValueType::Text, ValueType::Int],
            key: vec![0],
        };
        let changes = ChangeSet {
            changes: vec![
                Change::SetCell {
                    row: 1,
                    column: 1,
                    value: "\"paid\"".into(),
                },
                Change::SetCell {
                    row: 1,
                    column: 0,
                    value: "20".into(),
                },
                Change::InsertRow {
                    values: vec![None, Some("\"new\"".into()), None],
                },
                Change::InsertRow {
                    values: vec![None, None, None],
                },
                Change::DeleteRow { row: 0 },
            ],
        };
        let orig = |r: usize, c: usize| match (r, c) {
            (0, 0) => Some(Value::Int(1)),
            (1, 0) => Some(Value::Int(2)),
            _ => None,
        };
        let p: Vec<String> = plan(&sql, &t, &changes, orig)
            .unwrap()
            .into_iter()
            .map(|p| p.sql)
            .collect();
        assert_eq!(
            p,
            vec![
                "UPDATE public.orders SET status = 'paid', id = 20 WHERE id = 2",
                "INSERT INTO public.orders (status) VALUES ('new')",
                "INSERT INTO public.orders DEFAULT VALUES",
                "DELETE FROM public.orders WHERE id = 1",
            ]
        );
        let bad = ChangeSet {
            changes: vec![Change::SetCell {
                row: 0,
                column: 2,
                value: "1".into(),
            }],
        };
        assert!(plan(&sql, &t, &bad, orig).unwrap_err().contains("computed"));
        let gone = ChangeSet {
            changes: vec![Change::DeleteRow { row: 9 }],
        };
        assert!(plan(&sql, &t, &gone, orig).is_err());
    }
}
