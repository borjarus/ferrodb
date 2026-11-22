//! Query results as compact JSON for agents: rows of Arrow batches as arrays
//! of JSON values, BSON documents as relaxed Extended JSON, keys with their
//! type, TTL and size. Long values are cut.

use arrow_cast::display::{ArrayFormatter, FormatOptions};
use ferrodb_api::{
    ExecutionPlan, KeyEntry,
    arrow_array::{Array, RecordBatch, cast::AsArray},
    arrow_schema::{DataType, Schema},
    bson::{Bson, Document, RawDocumentBuf},
};
use serde_json::{Number, Value, json};

/// Characters kept of one string value.
pub const MAX_CELL_CHARS: usize = 2_000;
/// Characters kept of one document (serialized).
pub const MAX_DOCUMENT_CHARS: usize = 20_000;

/// Column names and types of a tabular result.
pub fn columns(schema: &Schema) -> Value {
    Value::Array(
        schema
            .fields()
            .iter()
            .map(|f| json!({ "name": f.name(), "type": f.data_type().to_string() }))
            .collect(),
    )
}

/// Appends up to `limit` rows of `batch` (from its first row) to `out`;
/// returns the number of rows that did not fit.
pub fn batch_rows(batch: &RecordBatch, limit: usize, out: &mut Vec<Value>) -> usize {
    let options = FormatOptions::default();
    let formatters: Vec<Option<ArrayFormatter<'_>>> = batch
        .columns()
        .iter()
        .map(|c| ArrayFormatter::try_new(c.as_ref(), &options).ok())
        .collect();
    let n = batch.num_rows().min(limit);
    for row in 0..n {
        let cells = batch
            .columns()
            .iter()
            .zip(&formatters)
            .map(|(col, fmt)| cell(col.as_ref(), row, fmt.as_ref()))
            .collect();
        out.push(Value::Array(cells));
    }
    batch.num_rows() - n
}

fn cell(array: &dyn Array, row: usize, fmt: Option<&ArrayFormatter<'_>>) -> Value {
    if array.is_null(row) {
        return Value::Null;
    }
    if let DataType::Boolean = array.data_type() {
        return Value::Bool(array.as_boolean().value(row));
    }
    let Some(fmt) = fmt else {
        return Value::String(format!("<{}>", array.data_type()));
    };
    let text = fmt.value(row).to_string();
    let numeric = matches!(
        array.data_type(),
        DataType::Int8
            | DataType::Int16
            | DataType::Int32
            | DataType::Int64
            | DataType::UInt8
            | DataType::UInt16
            | DataType::UInt32
            | DataType::UInt64
            | DataType::Float16
            | DataType::Float32
            | DataType::Float64
    );
    if numeric {
        // NaN and infinities are not JSON numbers: they stay strings.
        if let Ok(n) = text.parse::<Number>() {
            return Value::Number(n);
        }
    }
    Value::String(cut(text, MAX_CELL_CHARS))
}

/// A BSON document as relaxed Extended JSON (cut when very large).
pub fn document(raw: &RawDocumentBuf) -> Value {
    let value = match Document::try_from(raw) {
        Ok(doc) => Bson::Document(doc).into_relaxed_extjson(),
        Err(e) => return json!({ "$error": format!("unreadable document: {e}") }),
    };
    let text = value.to_string();
    if text.chars().count() > MAX_DOCUMENT_CHARS {
        json!({ "$truncated": cut(text, MAX_DOCUMENT_CHARS) })
    } else {
        value
    }
}

pub fn key(entry: &KeyEntry) -> Value {
    json!({
        "key": cut(entry.key.clone(), MAX_CELL_CHARS),
        "type": format!("{:?}", entry.kind).to_lowercase(),
        "ttl_secs": entry.ttl.map(|t| t.as_secs()),
        "size": entry.size,
    })
}

/// `text` cut to `max` characters, with an ellipsis.
pub fn cut(mut text: String, max: usize) -> String {
    if let Some((i, _)) = text.char_indices().nth(max) {
        text.truncate(i);
        text.push('…');
    }
    text
}

/// An execution plan as an indented tree, one node per line:
///
/// ```text
/// Hash Join on o.customer_id = c.id (cost=120.5 rows=1000)
///   -> Seq Scan on orders o (cost=50 rows=1000)
///        Filter: total > 100
///        ! full scan
/// ```
pub fn plan_text(plan: &ExecutionPlan) -> String {
    let mut out = String::new();
    for (depth, node) in plan.root.flatten() {
        let indent = "  ".repeat(depth);
        out.push_str(&indent);
        if depth > 0 {
            out.push_str("-> ");
        }
        out.push_str(&node.operation);
        if let Some(detail) = &node.detail {
            out.push_str(" on ");
            out.push_str(detail);
        } else if let Some(object) = &node.object {
            out.push_str(" on ");
            out.push_str(object);
        }
        let mut figures = Vec::new();
        if node.cost > 0.0 {
            figures.push(format!("cost={}", node.cost));
        }
        if node.rows > 0 {
            figures.push(format!("rows={}", node.rows));
        }
        if !figures.is_empty() {
            out.push_str(&format!(" ({})", figures.join(" ")));
        }
        out.push('\n');
        let pad = format!("{indent}     ");
        for (k, v) in &node.properties {
            out.push_str(&format!("{pad}{k}: {v}\n"));
        }
        for w in &node.warnings {
            out.push_str(&format!("{pad}! {w}\n"));
        }
    }
    if let Some(ms) = plan.planning_ms {
        out.push_str(&format!("Planning time: {ms} ms\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ferrodb_api::{
        PlanNode,
        arrow_array::{BooleanArray, Float64Array, Int64Array, StringArray},
        arrow_schema::Field,
        bson::{doc, oid::ObjectId},
    };

    use super::*;

    #[test]
    fn rows_keep_json_types() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("score", DataType::Float64, true),
            Field::new("ok", DataType::Boolean, true),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])),
                Arc::new(Float64Array::from(vec![1.5, f64::NAN, 3.0])),
                Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
            ],
        )
        .unwrap();
        let mut rows = Vec::new();
        assert_eq!(batch_rows(&batch, 2, &mut rows), 1);
        assert_eq!(
            Value::Array(rows),
            json!([[1, "a", 1.5, true], [2, null, "NaN", false]])
        );
        assert_eq!(columns(&schema)[0], json!({"name": "id", "type": "Int64"}));
    }

    #[test]
    fn documents_are_relaxed_extended_json() {
        let id = ObjectId::new();
        let raw = RawDocumentBuf::try_from(&doc! { "_id": id, "n": 5_i32, "s": "x" }).unwrap();
        assert_eq!(
            document(&raw),
            json!({ "_id": { "$oid": id.to_hex() }, "n": 5, "s": "x" })
        );
    }

    #[test]
    fn cut_respects_char_boundaries() {
        assert_eq!(cut("żółw".into(), 2), "żó…");
        assert_eq!(cut("abc".into(), 3), "abc");
    }

    #[test]
    fn plan_tree() {
        let mut root = PlanNode::new("Hash Join");
        root.detail = Some("o.customer_id = c.id".into());
        root.cost = 120.5;
        root.rows = 1000;
        let mut scan = PlanNode::new("Seq Scan");
        scan.object = Some("orders".into());
        scan.properties = vec![("Filter".into(), "total > 100".into())];
        scan.warnings = vec!["full scan".into()];
        root.children.push(scan);
        let plan = ExecutionPlan {
            root,
            ..Default::default()
        };
        assert_eq!(
            plan_text(&plan),
            "Hash Join on o.customer_id = c.id (cost=120.5 rows=1000)\n  -> Seq Scan on orders\n       Filter: total > 100\n       ! full scan\n"
        );
    }
}
