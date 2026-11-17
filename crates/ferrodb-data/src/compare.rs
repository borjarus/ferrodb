//! Comparing two results (plan 2.6): rows matched by key columns (or by
//! position), numbers equal within a tolerance, chosen columns excluded.

use std::collections::HashMap;

use arrow_array::RecordBatch;

use crate::value::{Value, value_at};

/// A fully loaded result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

impl Rows {
    pub fn from_batches(columns: Vec<String>, batches: &[RecordBatch]) -> Self {
        let mut rows = Vec::with_capacity(batches.iter().map(RecordBatch::num_rows).sum());
        for b in batches {
            for r in 0..b.num_rows() {
                rows.push(
                    b.columns()
                        .iter()
                        .map(|c| value_at(c.as_ref(), r))
                        .collect(),
                );
            }
        }
        Self { columns, rows }
    }

    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name).or_else(|| {
            self.columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(name))
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompareOptions {
    /// Columns matching rows; empty = by position.
    pub key: Vec<String>,
    /// Columns not compared.
    pub exclude: Vec<String>,
    /// Numbers differing by at most this much are equal.
    pub tolerance: f64,
    pub ignore_case: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum RowDiff {
    OnlyLeft(usize),
    OnlyRight(usize),
    Changed {
        left: usize,
        right: usize,
        /// Names of the differing columns.
        columns: Vec<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Comparison {
    /// Rows equal on every compared column.
    pub same: usize,
    pub diffs: Vec<RowDiff>,
    pub only_left_columns: Vec<String>,
    pub only_right_columns: Vec<String>,
}

impl Comparison {
    pub fn summary(&self) -> String {
        let (mut l, mut r, mut c) = (0, 0, 0);
        for d in &self.diffs {
            match d {
                RowDiff::OnlyLeft(_) => l += 1,
                RowDiff::OnlyRight(_) => r += 1,
                RowDiff::Changed { .. } => c += 1,
            }
        }
        format!(
            "{} same · {c} changed · {l} only left · {r} only right",
            self.same
        )
    }
}

fn equal(a: &Value, b: &Value, o: &CompareOptions) -> bool {
    if let (Some(x), Some(y)) = (a.as_f64(), b.as_f64())
        && !matches!(a, Value::Bool(_))
        && !matches!(b, Value::Bool(_))
    {
        return (x - y).abs() <= o.tolerance;
    }
    match (a.to_text(), b.to_text()) {
        (None, None) => true,
        (Some(x), Some(y)) if o.ignore_case => x.to_lowercase() == y.to_lowercase(),
        (x, y) => x == y,
    }
}

fn key_text(row: &[Value], cols: &[usize]) -> String {
    cols.iter()
        .map(|&c| {
            row.get(c)
                .and_then(Value::to_text)
                .unwrap_or_else(|| "\u{0}".into())
        })
        .collect::<Vec<_>>()
        .join("\u{1f}")
}

pub fn compare(left: &Rows, right: &Rows, o: &CompareOptions) -> Comparison {
    let excluded = |n: &str| o.exclude.iter().any(|e| e.eq_ignore_ascii_case(n));
    // Compared columns: (left index, right index, name).
    let pairs: Vec<(usize, usize, String)> = left
        .columns
        .iter()
        .enumerate()
        .filter(|(_, n)| !excluded(n))
        .filter_map(|(i, n)| right.column(n).map(|j| (i, j, n.clone())))
        .collect();
    let mut out = Comparison {
        only_left_columns: left
            .columns
            .iter()
            .filter(|n| !excluded(n) && right.column(n).is_none())
            .cloned()
            .collect(),
        only_right_columns: right
            .columns
            .iter()
            .filter(|n| !excluded(n) && left.column(n).is_none())
            .cloned()
            .collect(),
        ..Default::default()
    };
    let check = |li: usize, ri: usize| -> Option<RowDiff> {
        let differing: Vec<String> = pairs
            .iter()
            .filter(|(a, b, _)| {
                !equal(
                    left.rows[li].get(*a).unwrap_or(&Value::Null),
                    right.rows[ri].get(*b).unwrap_or(&Value::Null),
                    o,
                )
            })
            .map(|(_, _, n)| n.clone())
            .collect();
        (!differing.is_empty()).then_some(RowDiff::Changed {
            left: li,
            right: ri,
            columns: differing,
        })
    };
    let record = |out: &mut Comparison, d: Option<RowDiff>| match d {
        Some(d) => out.diffs.push(d),
        None => out.same += 1,
    };
    let lk: Vec<usize> = o.key.iter().filter_map(|k| left.column(k)).collect();
    let rk: Vec<usize> = o.key.iter().filter_map(|k| right.column(k)).collect();
    if o.key.is_empty() || lk.len() != o.key.len() || rk.len() != o.key.len() {
        let n = left.rows.len().min(right.rows.len());
        for i in 0..n {
            record(&mut out, check(i, i));
        }
        out.diffs
            .extend((n..left.rows.len()).map(RowDiff::OnlyLeft));
        out.diffs
            .extend((n..right.rows.len()).map(RowDiff::OnlyRight));
        return out;
    }
    let mut index: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in right.rows.iter().enumerate() {
        index.entry(key_text(r, &rk)).or_default().push(i);
    }
    let mut matched = vec![false; right.rows.len()];
    for (li, r) in left.rows.iter().enumerate() {
        let candidates = index.get_mut(&key_text(r, &lk));
        match candidates.and_then(|v| (!v.is_empty()).then(|| v.remove(0))) {
            Some(ri) => {
                matched[ri] = true;
                record(&mut out, check(li, ri));
            }
            None => out.diffs.push(RowDiff::OnlyLeft(li)),
        }
    }
    out.diffs.extend(
        matched
            .iter()
            .enumerate()
            .filter(|(_, m)| !**m)
            .map(|(i, _)| RowDiff::OnlyRight(i)),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(cols: &[&str], data: &[&[Value]]) -> Rows {
        Rows {
            columns: cols.iter().map(|c| c.to_string()).collect(),
            rows: data.iter().map(|r| r.to_vec()).collect(),
        }
    }

    #[test]
    fn keyed_with_tolerance_and_exclusion() {
        let l = rows(
            &["id", "price", "updated"],
            &[
                &[Value::Int(1), Value::Float(10.0), Value::Text("a".into())],
                &[Value::Int(2), Value::Float(5.0), Value::Text("a".into())],
                &[Value::Int(3), Value::Float(1.0), Value::Null],
            ],
        );
        let r = rows(
            &["ID", "price", "updated", "extra"],
            &[
                &[
                    Value::Int(2),
                    Value::Decimal("5.004".into()),
                    Value::Text("b".into()),
                    Value::Null,
                ],
                &[
                    Value::Int(1),
                    Value::Float(11.0),
                    Value::Text("b".into()),
                    Value::Null,
                ],
                &[Value::Int(4), Value::Float(1.0), Value::Null, Value::Null],
            ],
        );
        let c = compare(
            &l,
            &r,
            &CompareOptions {
                key: vec!["id".into()],
                exclude: vec!["updated".into()],
                tolerance: 0.01,
                ignore_case: false,
            },
        );
        assert_eq!(c.same, 1);
        assert_eq!(
            c.diffs,
            vec![
                RowDiff::Changed {
                    left: 0,
                    right: 1,
                    columns: vec!["price".into()]
                },
                RowDiff::OnlyLeft(2),
                RowDiff::OnlyRight(2),
            ]
        );
        assert_eq!(c.only_right_columns, vec!["extra"]);
        assert_eq!(
            c.summary(),
            "1 same · 1 changed · 1 only left · 1 only right"
        );
    }

    #[test]
    fn positional() {
        let l = rows(&["a"], &[&[Value::Int(1)], &[Value::Int(2)]]);
        let r = rows(&["a"], &[&[Value::Int(1)]]);
        let c = compare(&l, &r, &CompareOptions::default());
        assert_eq!((c.same, c.diffs.clone()), (1, vec![RowDiff::OnlyLeft(1)]));
    }
}
