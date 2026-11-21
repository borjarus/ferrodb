//! RESP2 / RESP3 replies as tables.
//!
//! A console command gives one table whose shape follows the reply:
//!
//! - a scalar (string, integer, double, boolean, nil, status) → one
//!   `value` cell;
//! - a map (RESP3) or a flat field/value array of a known command
//!   (`HGETALL`, `CONFIG GET`, `… WITHSCORES` in RESP2) → `field` / `value`
//!   (`member` / `score`) rows;
//! - a cursor reply (`SCAN`, `HSCAN`, `SSCAN`, `ZSCAN`) → its items, the
//!   next cursor as a notice;
//! - an array of equally long scalar arrays → one column per position;
//!   stream entries (`XRANGE`) → `id` / `fields`; an array of maps → one
//!   column per key;
//! - anything else → one `value` row per element, nested values as JSON.
//!
//! Columns whose values are all integers (doubles, booleans) become
//! `Int64` (`Float64`, `Boolean`); everything else is text. Binary strings
//! that are not UTF-8 are escaped like `redis-cli` does (`\xHH`).

use std::sync::Arc;

use arrow_array::{ArrayRef, BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use redis::Value;

/// One cell before column types are inferred.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

/// A rendered reply.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    /// Next cursor of a `*SCAN` reply.
    pub cursor: Option<String>,
}

/// Bytes as text: UTF-8 as is, other bytes escaped (`\xHH`).
pub fn bytes_text(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => {
            let mut out = String::with_capacity(b.len() * 2);
            for chunk in b.utf8_chunks() {
                out.push_str(chunk.valid());
                for x in chunk.invalid() {
                    out.push_str(&format!("\\x{x:02x}"));
                }
            }
            out
        }
    }
}

/// A scalar reply as text (`None` for nil and aggregates).
pub fn text(v: &Value) -> Option<String> {
    Some(match v {
        Value::BulkString(b) => bytes_text(b),
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".into(),
        Value::Int(i) => i.to_string(),
        Value::Double(d) => float_text(*d),
        Value::Boolean(b) => b.to_string(),
        Value::VerbatimString { text, .. } => text.clone(),
        Value::BigNumber(n) => bytes_text(n),
        Value::ServerError(e) => format!("(error) {e}"),
        Value::Attribute { data, .. } => return text(data),
        _ => return None,
    })
}

/// An integer reply (also integers sent as strings).
pub fn int(v: &Value) -> Option<i64> {
    match v {
        Value::Int(i) => Some(*i),
        Value::Double(d) if d.fract() == 0.0 => Some(*d as i64),
        other => text(other)?.trim().parse().ok(),
    }
}

/// A double reply (RESP3 doubles, RESP2 strings).
pub fn float(v: &Value) -> Option<f64> {
    match v {
        Value::Double(d) => Some(*d),
        Value::Int(i) => Some(*i as f64),
        other => {
            let t = text(other)?;
            match t.as_str() {
                "inf" | "+inf" => Some(f64::INFINITY),
                "-inf" => Some(f64::NEG_INFINITY),
                t => t.parse().ok(),
            }
        }
    }
}

fn float_text(d: f64) -> String {
    if d.is_infinite() {
        if d > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        d.to_string()
    }
}

/// Elements of an aggregate (array, set, push data); `None` for others.
pub fn items(v: &Value) -> Option<&[Value]> {
    match v {
        Value::Array(a) | Value::Set(a) => Some(a),
        Value::Push { data, .. } => Some(data),
        Value::Attribute { data, .. } => items(data),
        _ => None,
    }
}

/// Field/value pairs of a map, or of a flat array with an even length.
pub fn pairs(v: &Value) -> Option<Vec<(&Value, &Value)>> {
    match v {
        Value::Map(m) => Some(m.iter().map(|(k, v)| (k, v)).collect()),
        Value::Attribute { data, .. } => pairs(data),
        other => {
            let a = items(other)?;
            if !a.len().is_multiple_of(2) {
                return None;
            }
            Some(a.chunks(2).map(|c| (&c[0], &c[1])).collect())
        }
    }
}

/// A reply as JSON (nested values in one cell).
pub fn json(v: &Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        Value::Nil => J::Null,
        Value::Int(i) => J::from(*i),
        Value::Double(d) => serde_json::Number::from_f64(*d)
            .map(J::Number)
            .unwrap_or_else(|| J::String(float_text(*d))),
        Value::Boolean(b) => J::Bool(*b),
        Value::Map(m) => J::Object(
            m.iter()
                .map(|(k, v)| (text(k).unwrap_or_else(|| json(k).to_string()), json(v)))
                .collect(),
        ),
        Value::Array(a) | Value::Set(a) => J::Array(a.iter().map(json).collect()),
        Value::Push { data, .. } => J::Array(data.iter().map(json).collect()),
        Value::Attribute { data, .. } => json(data),
        other => J::String(text(other).unwrap_or_default()),
    }
}

/// A value as one cell: scalars typed, aggregates as JSON text.
pub fn cell(v: &Value) -> Cell {
    match v {
        Value::Nil => Cell::Null,
        Value::Int(i) => Cell::Int(*i),
        Value::Double(d) => Cell::Float(*d),
        Value::Boolean(b) => Cell::Bool(*b),
        Value::Attribute { data, .. } => cell(data),
        Value::Array(_) | Value::Set(_) | Value::Map(_) | Value::Push { .. } => {
            Cell::Text(json(v).to_string())
        }
        other => Cell::Text(text(other).unwrap_or_default()),
    }
}

fn is_aggregate(v: &Value) -> bool {
    match v {
        Value::Array(_) | Value::Set(_) | Value::Map(_) | Value::Push { .. } => true,
        Value::Attribute { data, .. } => is_aggregate(data),
        _ => false,
    }
}

fn single(name: &str, v: &Value) -> Table {
    Table {
        columns: vec![name.into()],
        rows: vec![vec![cell(v)]],
        cursor: None,
    }
}

fn two(a: &str, b: &str, rows: Vec<Vec<Cell>>) -> Table {
    Table {
        columns: vec![a.into(), b.into()],
        rows,
        cursor: None,
    }
}

/// Commands whose RESP2 flat arrays are field/value pairs.
fn pair_names(cmd: &str, args: &[String]) -> Option<(&'static str, &'static str)> {
    let has = |w: &str| args.iter().any(|a| a.eq_ignore_ascii_case(w));
    match cmd {
        "HGETALL" | "CONFIG" | "HSCAN" => Some(("field", "value")),
        "HRANDFIELD" if has("WITHVALUES") => Some(("field", "value")),
        "ZSCAN" => Some(("member", "score")),
        "ZRANGE" | "ZRANGEBYSCORE" | "ZREVRANGE" | "ZREVRANGEBYSCORE" | "ZRANDMEMBER"
        | "ZINTER" | "ZUNION" | "ZDIFF" | "ZPOPMIN" | "ZPOPMAX"
            if has("WITHSCORES") || cmd.starts_with("ZPOP") =>
        {
            Some(("member", "score"))
        }
        _ => None,
    }
}

fn pair_rows(v: &Value, (_, b): (&str, &str)) -> Option<Vec<Vec<Cell>>> {
    // RESP3 `… WITHSCORES` replies are arrays of [member, score] arrays.
    if let Some(a) = items(v)
        && !a.is_empty()
        && a.iter().all(|x| items(x).is_some_and(|i| i.len() == 2))
    {
        return Some(
            a.iter()
                .map(|x| {
                    let i = items(x).expect("checked");
                    vec![cell(&i[0]), score_cell(&i[1], b)]
                })
                .collect(),
        );
    }
    let p = pairs(v)?;
    if p.iter().any(|(k, _)| is_aggregate(k)) {
        return None;
    }
    Some(
        p.into_iter()
            .map(|(k, v)| vec![cell(k), score_cell(v, b)])
            .collect(),
    )
}

fn score_cell(v: &Value, column: &str) -> Cell {
    if column == "score" {
        float(v).map(Cell::Float).unwrap_or_else(|| cell(v))
    } else {
        cell(v)
    }
}

/// Renders the reply of `args` (`args[0]` is the command).
pub fn render(args: &[String], v: &Value) -> Table {
    let cmd = args
        .first()
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or_default();
    let cmd = cmd.as_str();
    // Cursor replies: [cursor, items].
    if matches!(cmd, "SCAN" | "HSCAN" | "SSCAN" | "ZSCAN")
        && let Some([cursor, rest]) = items(v)
        && !is_aggregate(cursor)
    {
        let mut t = match pair_names(cmd, args) {
            Some(names) => pair_rows(rest, names)
                .map(|rows| two(names.0, names.1, rows))
                .unwrap_or_else(|| list("value", rest)),
            None => list(if cmd == "SCAN" { "key" } else { "member" }, rest),
        };
        t.cursor = text(cursor);
        return t;
    }
    if !is_aggregate(v) {
        return single("value", v);
    }
    if let Value::Map(m) = v {
        return two(
            "field",
            "value",
            m.iter().map(|(k, v)| vec![cell(k), cell(v)]).collect(),
        );
    }
    if let Some(names) = pair_names(cmd, args)
        && let Some(rows) = pair_rows(v, names)
    {
        return two(names.0, names.1, rows);
    }
    let a = items(v).unwrap_or_default();
    if a.is_empty() {
        return Table {
            columns: vec!["value".into()],
            rows: Vec::new(),
            cursor: None,
        };
    }
    // Stream entries: [id, [field, value, …]].
    if matches!(cmd, "XRANGE" | "XREVRANGE" | "XCLAIM" | "XAUTOCLAIM")
        || a.iter().all(|e| {
            items(e).is_some_and(|i| {
                i.len() == 2
                    && !is_aggregate(&i[0])
                    && items(&i[1]).is_some_and(|f| f.len() % 2 == 0)
                    && text(&i[0]).is_some_and(|id| is_stream_id(&id))
            })
        })
    {
        let rows: Option<Vec<Vec<Cell>>> = a
            .iter()
            .map(|e| {
                let i = items(e)?;
                let (id, fields) = (i.first()?, i.get(1)?);
                let obj: serde_json::Map<String, serde_json::Value> = pairs(fields)?
                    .into_iter()
                    .map(|(k, v)| (text(k).unwrap_or_default(), json(v)))
                    .collect();
                Some(vec![
                    cell(id),
                    Cell::Text(serde_json::Value::Object(obj).to_string()),
                ])
            })
            .collect();
        if let Some(rows) = rows {
            return two("id", "fields", rows);
        }
    }
    // Arrays of maps (RESP3 CLUSTER SHARDS, COMMAND DOCS…): a column per key.
    if a.iter().all(|e| matches!(e, Value::Map(_))) {
        let mut columns: Vec<String> = Vec::new();
        for e in a {
            if let Value::Map(m) = e {
                for (k, _) in m {
                    let k = text(k).unwrap_or_default();
                    if !columns.contains(&k) {
                        columns.push(k);
                    }
                }
            }
        }
        let rows = a
            .iter()
            .map(|e| {
                let Value::Map(m) = e else { unreachable!() };
                columns
                    .iter()
                    .map(|c| {
                        m.iter()
                            .find(|(k, _)| text(k).as_deref() == Some(c))
                            .map(|(_, v)| cell(v))
                            .unwrap_or(Cell::Null)
                    })
                    .collect()
            })
            .collect();
        return Table {
            columns,
            rows,
            cursor: None,
        };
    }
    // Arrays of equally long scalar arrays: a column per position.
    if let Some(n) = items(&a[0]).map(<[Value]>::len)
        && (1..=16).contains(&n)
        && a.iter()
            .all(|e| items(e).is_some_and(|i| i.len() == n && i.iter().all(|x| !is_aggregate(x))))
    {
        return Table {
            columns: (1..=n).map(|i| i.to_string()).collect(),
            rows: a
                .iter()
                .map(|e| items(e).expect("checked").iter().map(cell).collect())
                .collect(),
            cursor: None,
        };
    }
    list("value", v)
}

fn is_stream_id(s: &str) -> bool {
    s.split_once('-').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
    })
}

fn list(name: &str, v: &Value) -> Table {
    Table {
        columns: vec![name.into()],
        rows: items(v)
            .unwrap_or_default()
            .iter()
            .map(|x| vec![cell(x)])
            .collect(),
        cursor: None,
    }
}

impl Table {
    /// The Arrow schema: integer / double / boolean columns where every
    /// value has that type, text otherwise.
    pub fn schema(&self) -> SchemaRef {
        let fields: Vec<Field> = (0..self.columns.len())
            .map(|i| Field::new(&self.columns[i], self.column_type(i), true))
            .collect();
        Arc::new(Schema::new(fields))
    }

    fn column_type(&self, i: usize) -> DataType {
        let cells = self
            .rows
            .iter()
            .map(|r| &r[i])
            .filter(|c| **c != Cell::Null);
        let mut ty: Option<DataType> = None;
        for c in cells {
            let t = match c {
                Cell::Int(_) => DataType::Int64,
                Cell::Float(_) => DataType::Float64,
                Cell::Bool(_) => DataType::Boolean,
                _ => return DataType::Utf8,
            };
            ty = Some(match (ty, t) {
                (None, t) => t,
                (Some(a), b) if a == b => a,
                (Some(DataType::Int64), DataType::Float64)
                | (Some(DataType::Float64), DataType::Int64) => DataType::Float64,
                _ => return DataType::Utf8,
            });
        }
        ty.unwrap_or(DataType::Utf8)
    }

    /// Rows `[from, to)` as a record batch of `schema`.
    pub fn batch(&self, schema: &SchemaRef, from: usize, to: usize) -> RecordBatch {
        let rows = &self.rows[from..to];
        let columns: Vec<ArrayRef> = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(i, f)| -> ArrayRef {
                match f.data_type() {
                    DataType::Int64 => Arc::new(
                        rows.iter()
                            .map(|r| match r[i] {
                                Cell::Int(v) => Some(v),
                                _ => None,
                            })
                            .collect::<Int64Array>(),
                    ),
                    DataType::Float64 => Arc::new(
                        rows.iter()
                            .map(|r| match r[i] {
                                Cell::Float(v) => Some(v),
                                Cell::Int(v) => Some(v as f64),
                                _ => None,
                            })
                            .collect::<Float64Array>(),
                    ),
                    DataType::Boolean => Arc::new(
                        rows.iter()
                            .map(|r| match r[i] {
                                Cell::Bool(v) => Some(v),
                                _ => None,
                            })
                            .collect::<BooleanArray>(),
                    ),
                    _ => Arc::new(
                        rows.iter()
                            .map(|r| match &r[i] {
                                Cell::Null => None,
                                Cell::Int(v) => Some(v.to_string()),
                                Cell::Float(v) => Some(float_text(*v)),
                                Cell::Bool(v) => Some(v.to_string()),
                                Cell::Text(s) => Some(s.clone()),
                            })
                            .collect::<StringArray>(),
                    ),
                }
            })
            .collect();
        RecordBatch::try_new(schema.clone(), columns).expect("columns match the schema")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    fn args(s: &str) -> Vec<String> {
        crate::kv::split_args(s).unwrap()
    }

    #[test]
    fn scalars() {
        let t = render(&args("GET a"), &bulk("hello"));
        assert_eq!(t.columns, vec!["value"]);
        assert_eq!(t.rows, vec![vec![Cell::Text("hello".into())]]);
        assert_eq!(t.schema().field(0).data_type(), &DataType::Utf8);
        let t = render(&args("INCR a"), &Value::Int(5));
        assert_eq!(t.schema().field(0).data_type(), &DataType::Int64);
        assert_eq!(
            render(&args("GET x"), &Value::Nil).rows,
            vec![vec![Cell::Null]]
        );
        assert_eq!(
            render(&args("SET a 1"), &Value::Okay).rows,
            vec![vec![Cell::Text("OK".into())]]
        );
        assert_eq!(bytes_text(&[0x61, 0xff, 0x62]), "a\\xffb");
    }

    #[test]
    fn hashes_resp2_and_resp3() {
        let resp2 = Value::Array(vec![bulk("name"), bulk("Ann"), bulk("age"), bulk("30")]);
        let t = render(&args("HGETALL u"), &resp2);
        assert_eq!(t.columns, vec!["field", "value"]);
        assert_eq!(t.rows.len(), 2);
        let resp3 = Value::Map(vec![(bulk("name"), bulk("Ann"))]);
        let t = render(&args("HGETALL u"), &resp3);
        assert_eq!(t.columns, vec!["field", "value"]);
        assert_eq!(t.rows[0][1], Cell::Text("Ann".into()));
    }

    #[test]
    fn zsets_with_scores() {
        let resp2 = Value::Array(vec![bulk("a"), bulk("1.5"), bulk("b"), bulk("2")]);
        let t = render(&args("ZRANGE z 0 -1 WITHSCORES"), &resp2);
        assert_eq!(t.columns, vec!["member", "score"]);
        assert_eq!(t.schema().field(1).data_type(), &DataType::Float64);
        let resp3 = Value::Array(vec![
            Value::Array(vec![bulk("a"), Value::Double(1.5)]),
            Value::Array(vec![bulk("b"), Value::Double(2.0)]),
        ]);
        let t = render(&args("zrange z 0 -1 withscores"), &resp3);
        assert_eq!(t.rows[1], vec![Cell::Text("b".into()), Cell::Float(2.0)]);
    }

    #[test]
    fn cursors() {
        let v = Value::Array(vec![bulk("17"), Value::Array(vec![bulk("k1"), bulk("k2")])]);
        let t = render(&args("SCAN 0"), &v);
        assert_eq!(t.cursor.as_deref(), Some("17"));
        assert_eq!(t.columns, vec!["key"]);
        assert_eq!(t.rows.len(), 2);
        let v = Value::Array(vec![bulk("0"), Value::Array(vec![bulk("f"), bulk("v")])]);
        let t = render(&args("HSCAN h 0"), &v);
        assert_eq!(t.columns, vec!["field", "value"]);
    }

    #[test]
    fn streams_and_nested() {
        let v = Value::Array(vec![Value::Array(vec![
            bulk("1700000000000-0"),
            Value::Array(vec![bulk("type"), bulk("click")]),
        ])]);
        let t = render(&args("XRANGE s - +"), &v);
        assert_eq!(t.columns, vec!["id", "fields"]);
        assert_eq!(t.rows[0][1], Cell::Text("{\"type\":\"click\"}".into()));
        // EXEC with an error inside.
        let v = Value::Array(vec![Value::Okay, Value::Int(2)]);
        let t = render(&args("EXEC"), &v);
        assert_eq!(t.columns, vec!["value"]);
        assert_eq!(t.schema().field(0).data_type(), &DataType::Utf8);
        // Array of maps.
        let v = Value::Array(vec![
            Value::Map(vec![(bulk("id"), Value::Int(1)), (bulk("x"), bulk("a"))]),
            Value::Map(vec![(bulk("id"), Value::Int(2))]),
        ]);
        let t = render(&args("CLUSTER SHARDS"), &v);
        assert_eq!(t.columns, vec!["id", "x"]);
        assert_eq!(t.rows[1][1], Cell::Null);
        // Nested arrays fall back to JSON.
        let v = Value::Array(vec![Value::Array(vec![Value::Array(vec![Value::Int(1)])])]);
        let t = render(&args("COMMAND INFO get"), &v);
        assert_eq!(t.rows[0][0], Cell::Text("[[1]]".into()));
        // Fixed-width tuples.
        let v = Value::Array(vec![
            Value::Array(vec![bulk("a"), Value::Int(1)]),
            Value::Array(vec![bulk("b"), Value::Int(2)]),
        ]);
        let t = render(&args("SLOWLOG GET"), &v);
        assert_eq!(t.columns, vec!["1", "2"]);
        let schema = t.schema();
        let b = t.batch(&schema, 0, 2);
        assert_eq!(b.num_rows(), 2);
        assert_eq!(schema.field(1).data_type(), &DataType::Int64);
    }
}
