//! Chart data from a result (plan 2.6: bar, line, pie): a category
//! column, one or more value columns, an aggregate per category.

use crate::{compare::Rows, value::Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChartKind {
    Bar,
    Line,
    Pie,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aggregate {
    Sum,
    Count,
    Avg,
    Min,
    Max,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartSpec {
    pub kind: ChartKind,
    pub category: usize,
    /// Value columns (pie: the first one only). Empty with `Count`.
    pub values: Vec<usize>,
    pub aggregate: Aggregate,
    /// Most categories shown; the rest are summed into "other".
    pub limit: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub name: String,
    pub values: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChartData {
    pub kind: ChartKind,
    pub categories: Vec<String>,
    pub series: Vec<Series>,
}

#[derive(Clone, Copy, Default)]
struct Acc {
    sum: f64,
    n: u64,
    min: Option<f64>,
    max: Option<f64>,
}

impl Acc {
    fn add(&mut self, v: f64) {
        self.sum += v;
        self.n += 1;
        self.min = Some(self.min.map_or(v, |m| m.min(v)));
        self.max = Some(self.max.map_or(v, |m| m.max(v)));
    }
    fn get(&self, a: Aggregate) -> f64 {
        match a {
            Aggregate::Sum => self.sum,
            Aggregate::Count => self.n as f64,
            Aggregate::Avg if self.n > 0 => self.sum / self.n as f64,
            Aggregate::Avg => 0.0,
            Aggregate::Min => self.min.unwrap_or(0.0),
            Aggregate::Max => self.max.unwrap_or(0.0),
        }
    }
}

pub fn chart(rows: &Rows, spec: &ChartSpec) -> Result<ChartData, String> {
    if spec.category >= rows.columns.len() {
        return Err("no category column".into());
    }
    let mut values = spec.values.clone();
    if spec.kind == ChartKind::Pie {
        values.truncate(1);
    }
    if values.is_empty() && spec.aggregate != Aggregate::Count {
        return Err("choose at least one value column".into());
    }
    if let Some(&bad) = values.iter().find(|&&v| v >= rows.columns.len()) {
        return Err(format!("no column {}", bad + 1));
    }
    let width = values.len().max(1);
    let mut cats: Vec<String> = Vec::new();
    let mut accs: Vec<Vec<Acc>> = Vec::new();
    let mut index = std::collections::HashMap::new();
    for r in &rows.rows {
        let cat = r
            .get(spec.category)
            .and_then(Value::to_text)
            .unwrap_or_else(|| "(null)".into());
        let i = *index.entry(cat.clone()).or_insert_with(|| {
            cats.push(cat);
            accs.push(vec![Acc::default(); width]);
            cats.len() - 1
        });
        if values.is_empty() {
            accs[i][0].add(1.0);
        }
        for (s, &c) in values.iter().enumerate() {
            match r.get(c).and_then(Value::as_f64) {
                Some(v) => accs[i][s].add(v),
                None if spec.aggregate == Aggregate::Count => {}
                None => {}
            }
        }
    }
    let limit = spec.limit.max(1);
    if cats.len() > limit {
        // Keep the largest categories (by the first series).
        let mut order: Vec<usize> = (0..cats.len()).collect();
        order.sort_by(|a, b| {
            accs[*b][0]
                .get(spec.aggregate)
                .total_cmp(&accs[*a][0].get(spec.aggregate))
        });
        let keep: std::collections::HashSet<usize> = order[..limit - 1].iter().copied().collect();
        let mut other = vec![Acc::default(); width];
        let mut nc = Vec::new();
        let mut na = Vec::new();
        for (i, (c, a)) in cats.into_iter().zip(accs).enumerate() {
            if keep.contains(&i) {
                nc.push(c);
                na.push(a);
            } else {
                for (o, x) in other.iter_mut().zip(a) {
                    o.sum += x.sum;
                    o.n += x.n;
                    o.min = match (o.min, x.min) {
                        (Some(p), Some(q)) => Some(p.min(q)),
                        (p, q) => p.or(q),
                    };
                    o.max = match (o.max, x.max) {
                        (Some(p), Some(q)) => Some(p.max(q)),
                        (p, q) => p.or(q),
                    };
                }
            }
        }
        nc.push("other".into());
        na.push(other);
        cats = nc;
        accs = na;
    }
    let names: Vec<String> = if values.is_empty() {
        vec!["count".into()]
    } else {
        values.iter().map(|&c| rows.columns[c].clone()).collect()
    };
    let series = names
        .into_iter()
        .enumerate()
        .map(|(s, name)| Series {
            name,
            values: accs.iter().map(|a| a[s].get(spec.aggregate)).collect(),
        })
        .collect();
    Ok(ChartData {
        kind: spec.kind,
        categories: cats,
        series,
    })
}

/// Selection aggregates over any values: numbers get sum / avg / min /
/// max; other values min / max as text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Aggregates {
    pub count: usize,
    pub nulls: usize,
    pub distinct: usize,
    pub sum: Option<f64>,
    pub avg: Option<f64>,
    pub min: Option<String>,
    pub max: Option<String>,
}

impl Aggregates {
    pub fn of<'a>(values: impl IntoIterator<Item = &'a Value>) -> Self {
        let mut a = Aggregates::default();
        let mut distinct = std::collections::HashSet::new();
        let mut acc = Acc::default();
        let mut numeric = true;
        let mut tmin: Option<String> = None;
        let mut tmax: Option<String> = None;
        for v in values {
            if v.is_null() {
                a.nulls += 1;
                continue;
            }
            a.count += 1;
            let text = v.to_text().unwrap_or_default();
            match v.as_f64().filter(|_| !matches!(v, Value::Bool(_))) {
                Some(f) => acc.add(f),
                None => numeric = false,
            }
            if tmin.as_ref().is_none_or(|m| &text < m) {
                tmin = Some(text.clone());
            }
            if tmax.as_ref().is_none_or(|m| &text > m) {
                tmax = Some(text.clone());
            }
            distinct.insert(text);
        }
        a.distinct = distinct.len();
        if numeric && acc.n > 0 {
            a.sum = Some(acc.sum);
            a.avg = Some(acc.sum / acc.n as f64);
            a.min = acc.min.map(crate::value::float_text);
            a.max = acc.max.map(crate::value::float_text);
        } else {
            a.min = tmin;
            a.max = tmax;
        }
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouped_series() {
        let rows = Rows {
            columns: vec!["city".into(), "amount".into()],
            rows: vec![
                vec![Value::Text("A".into()), Value::Int(10)],
                vec![Value::Text("B".into()), Value::Int(5)],
                vec![Value::Text("A".into()), Value::Float(2.5)],
                vec![Value::Text("C".into()), Value::Int(1)],
                vec![Value::Null, Value::Int(1)],
            ],
        };
        let d = chart(
            &rows,
            &ChartSpec {
                kind: ChartKind::Bar,
                category: 0,
                values: vec![1],
                aggregate: Aggregate::Sum,
                limit: 3,
            },
        )
        .unwrap();
        assert_eq!(d.categories, vec!["A", "B", "other"]);
        assert_eq!(d.series[0].values, vec![12.5, 5.0, 2.0]);
        let count = chart(
            &rows,
            &ChartSpec {
                kind: ChartKind::Pie,
                category: 0,
                values: vec![],
                aggregate: Aggregate::Count,
                limit: 10,
            },
        )
        .unwrap();
        assert_eq!(count.series[0].values, vec![2.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn aggregates() {
        let v = [Value::Int(3), Value::Null, Value::Float(1.5), Value::Int(3)];
        let a = Aggregates::of(&v);
        assert_eq!((a.count, a.nulls, a.distinct), (3, 1, 2));
        assert_eq!(a.sum, Some(7.5));
        assert_eq!(a.min.as_deref(), Some("1.5"));
        let t = [Value::Text("b".into()), Value::Text("a".into())];
        let a = Aggregates::of(&t);
        assert_eq!(
            (a.sum, a.min.as_deref(), a.max.as_deref()),
            (None, Some("a"), Some("b"))
        );
    }
}
