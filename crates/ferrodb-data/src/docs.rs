//! Rows ↔ documents, for copying between data models and for writing
//! documents to tabular formats.
//!
//! - a row becomes a document with one field per column; a dotted column
//!   name (`address.city`) becomes a nested field; NULLs are kept as
//!   `null` (so the column set survives the trip back);
//! - a document becomes a row by looking up each column path; nested
//!   documents and arrays not addressed by a path become JSON values.

use bson::{Bson, Document, RawDocumentBuf};

use crate::value::{Value, ValueType};

/// A column of documents seen as a table.
#[derive(Clone, Debug, PartialEq)]
pub struct DocColumn {
    /// Field path (`address.city`).
    pub path: String,
    pub ty: ValueType,
}

pub fn row_to_document(columns: &[String], values: &[Value]) -> Document {
    let mut doc = Document::new();
    for (name, v) in columns.iter().zip(values) {
        insert_path(&mut doc, name, v.to_bson());
    }
    doc
}

fn insert_path(doc: &mut Document, path: &str, value: Bson) {
    match path.split_once('.') {
        Some((head, rest)) if !head.is_empty() && !rest.is_empty() => {
            if !matches!(doc.get(head), Some(Bson::Document(_))) {
                doc.insert(head, Document::new());
            }
            if let Some(Bson::Document(inner)) = doc.get_mut(head) {
                insert_path(inner, rest, value);
            }
        }
        _ => {
            doc.insert(path, value);
        }
    }
}

/// The value at a dotted path (`None` when missing).
pub fn lookup<'a>(doc: &'a Document, path: &str) -> Option<&'a Bson> {
    if let Some(v) = doc.get(path) {
        return Some(v);
    }
    let (head, rest) = path.split_once('.')?;
    match doc.get(head)? {
        Bson::Document(inner) => lookup(inner, rest),
        _ => None,
    }
}

pub fn document_to_row(doc: &Document, columns: &[DocColumn]) -> Vec<Value> {
    columns
        .iter()
        .map(|c| lookup(doc, &c.path).map_or(Value::Null, Value::from_bson))
        .collect()
}

pub fn decode(raw: &RawDocumentBuf) -> Document {
    Document::try_from(raw.as_ref()).unwrap_or_default()
}

/// Top-level fields of `docs` in order of first appearance, typed by
/// their values (mixed types become `Any`).
pub fn infer_columns<'a>(docs: impl IntoIterator<Item = &'a Document>) -> Vec<DocColumn> {
    let mut cols: Vec<DocColumn> = Vec::new();
    for d in docs {
        for (k, v) in d {
            let ty = bson_type(v);
            match cols.iter_mut().find(|c| &c.path == k) {
                Some(c) => {
                    if c.ty != ty && ty != ValueType::Any {
                        c.ty = match (c.ty, ty) {
                            (ValueType::Any, t) => t,
                            (ValueType::Int, ValueType::Float)
                            | (ValueType::Float, ValueType::Int) => ValueType::Float,
                            _ => ValueType::Text,
                        };
                    }
                }
                None => cols.push(DocColumn {
                    path: k.clone(),
                    ty,
                }),
            }
        }
    }
    for c in &mut cols {
        if c.ty == ValueType::Any {
            c.ty = ValueType::Text;
        }
    }
    cols
}

/// Value type of a BSON value; `Any` for null.
pub fn bson_type(v: &Bson) -> ValueType {
    match v {
        Bson::Null | Bson::Undefined => ValueType::Any,
        Bson::Boolean(_) => ValueType::Bool,
        Bson::Int32(_) | Bson::Int64(_) => ValueType::Int,
        Bson::Double(_) => ValueType::Float,
        Bson::Decimal128(_) => ValueType::Decimal,
        Bson::String(_) | Bson::ObjectId(_) | Bson::Symbol(_) => ValueType::Text,
        Bson::Binary(_) => ValueType::Bytes,
        Bson::DateTime(_) => ValueType::Timestamp { tz: true },
        Bson::Document(_) | Bson::Array(_) => ValueType::Json,
        _ => ValueType::Text,
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn nested_paths() {
        let cols = vec!["id".to_owned(), "address.city".into(), "address.zip".into()];
        let d = row_to_document(
            &cols,
            &[Value::Int(1), Value::Text("Kraków".into()), Value::Null],
        );
        assert_eq!(
            d,
            doc! { "id": 1, "address": { "city": "Kraków", "zip": Bson::Null } }
        );
        let dc: Vec<DocColumn> = cols
            .iter()
            .map(|p| DocColumn {
                path: p.clone(),
                ty: ValueType::Any,
            })
            .collect();
        assert_eq!(
            document_to_row(&d, &dc),
            vec![Value::Int(1), Value::Text("Kraków".into()), Value::Null]
        );
    }

    #[test]
    fn inferred_columns() {
        let docs = [
            doc! { "_id": 1, "n": 1, "tags": ["a"] },
            doc! { "_id": 2, "n": 2.5, "x": Bson::Null },
        ];
        let cols = infer_columns(&docs);
        let names: Vec<_> = cols.iter().map(|c| (c.path.as_str(), c.ty)).collect();
        assert_eq!(
            names,
            vec![
                ("_id", ValueType::Int),
                ("n", ValueType::Float),
                ("tags", ValueType::Json),
                ("x", ValueType::Text)
            ]
        );
    }
}
