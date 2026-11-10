//! Schema inference for document collections (plan, section 5.7): field
//! paths, BSON types with their share, presence and sample values, from a
//! sample of documents. A `$jsonSchema` validator takes precedence: its
//! properties come first with their declared types.

use std::collections::HashMap;

use bson::{Bson, Document};
use ferrodb_api::{FieldStats, InferredSchema};

/// Sample values kept per field.
const SAMPLES: usize = 3;
/// Characters of a sample value.
const SAMPLE_LEN: usize = 60;
/// Nesting depth followed into sub-documents and arrays.
const MAX_DEPTH: usize = 8;

/// mongosh / `$type` names.
pub fn type_name(v: &Bson) -> &'static str {
    match v {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Symbol(_) => "symbol",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
        Bson::DbPointer(_) => "dbPointer",
    }
}

#[derive(Default)]
struct Acc {
    order: usize,
    /// Documents containing the path.
    docs: usize,
    types: HashMap<&'static str, usize>,
    values: usize,
    samples: Vec<String>,
}

fn sample(v: &Bson) -> Option<String> {
    if matches!(v, Bson::Document(_) | Bson::Array(_)) {
        return None;
    }
    let s = ferrodb_lang_print(v);
    Some(if s.chars().count() > SAMPLE_LEN {
        let cut: String = s.chars().take(SAMPLE_LEN).collect();
        format!("{cut}…")
    } else {
        s
    })
}

fn ferrodb_lang_print(v: &Bson) -> String {
    match v {
        Bson::String(s) => s.clone(),
        other => ferrodb_lang::mql::print_inline(other),
    }
}

fn record(
    path: &str,
    v: &Bson,
    seen: &mut Vec<String>,
    acc: &mut HashMap<String, Acc>,
    next: &mut usize,
) {
    let a = acc.entry(path.to_owned()).or_insert_with(|| {
        *next += 1;
        Acc {
            order: *next,
            ..Default::default()
        }
    });
    if !seen.iter().any(|s| s == path) {
        a.docs += 1;
        seen.push(path.to_owned());
    }
    *a.types.entry(type_name(v)).or_default() += 1;
    a.values += 1;
    if a.samples.len() < SAMPLES
        && let Some(s) = sample(v)
        && !a.samples.contains(&s)
    {
        a.samples.push(s);
    }
}

fn walk(
    prefix: &str,
    v: &Bson,
    seen: &mut Vec<String>,
    acc: &mut HashMap<String, Acc>,
    next: &mut usize,
    depth: usize,
) {
    match v {
        Bson::Document(d) if depth < MAX_DEPTH => {
            for (k, x) in d {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                record(&path, x, seen, acc, next);
                walk(&path, x, seen, acc, next, depth + 1);
            }
        }
        // Array elements contribute to the same path (`items.sku`).
        Bson::Array(items) if depth < MAX_DEPTH => {
            for x in items.iter().take(100) {
                if let Bson::Document(_) = x {
                    walk(prefix, x, seen, acc, next, depth + 1);
                }
            }
        }
        _ => {}
    }
}

/// Field statistics of a sample.
pub fn infer(docs: &[Document]) -> InferredSchema {
    let mut acc: HashMap<String, Acc> = HashMap::new();
    let mut next = 0;
    for d in docs {
        let mut seen = Vec::new();
        walk(
            "",
            &Bson::Document(d.clone()),
            &mut seen,
            &mut acc,
            &mut next,
            0,
        );
    }
    let n = docs.len().max(1) as f32;
    let mut fields: Vec<(usize, FieldStats)> = acc
        .into_iter()
        .map(|(path, a)| {
            let mut types: Vec<(String, f32)> = a
                .types
                .into_iter()
                .map(|(t, c)| (t.to_owned(), c as f32 / a.values.max(1) as f32))
                .collect();
            types.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
            (
                a.order,
                FieldStats {
                    path,
                    types,
                    presence: (a.docs as f32 / n).min(1.0),
                    samples: a.samples,
                },
            )
        })
        .collect();
    // `_id` first, then by presence, then in order of appearance.
    fields.sort_by(|(oa, a), (ob, b)| {
        (b.path == "_id")
            .cmp(&(a.path == "_id"))
            .then(b.presence.total_cmp(&a.presence))
            .then(oa.cmp(ob))
    });
    InferredSchema {
        sampled: docs.len(),
        fields: fields.into_iter().map(|(_, f)| f).collect(),
    }
}

fn json_schema_fields(prefix: &str, schema: &Document, out: &mut Vec<FieldStats>) {
    let required: Vec<&str> = schema
        .get_array("required")
        .map(|r| r.iter().filter_map(Bson::as_str).collect())
        .unwrap_or_default();
    let Ok(props) = schema.get_document("properties") else {
        return;
    };
    for (k, p) in props {
        let path = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        let Some(p) = p.as_document() else { continue };
        let types: Vec<String> = match p.get("bsonType").or_else(|| p.get("type")) {
            Some(Bson::String(t)) => vec![t.clone()],
            Some(Bson::Array(a)) => a
                .iter()
                .filter_map(Bson::as_str)
                .map(str::to_owned)
                .collect(),
            _ => Vec::new(),
        };
        let share = 1.0 / types.len().max(1) as f32;
        out.push(FieldStats {
            path: path.clone(),
            types: types.into_iter().map(|t| (t, share)).collect(),
            presence: if required.contains(&k.as_str()) {
                1.0
            } else {
                0.0
            },
            samples: Vec::new(),
        });
        json_schema_fields(&path, p, out);
        if let Ok(items) = p.get_document("items") {
            json_schema_fields(&path, items, out);
        }
    }
}

/// Fields declared by a validator's `$jsonSchema`, merged with a sample:
/// declared fields first (sampled presence when known), then the rest.
pub fn with_validator(validator: Option<&Document>, sampled: InferredSchema) -> InferredSchema {
    let Some(schema) = validator.and_then(|v| v.get_document("$jsonSchema").ok()) else {
        return sampled;
    };
    let mut declared = Vec::new();
    json_schema_fields("", schema, &mut declared);
    let mut rest = sampled.fields;
    for d in &mut declared {
        if let Some(i) = rest.iter().position(|f| f.path == d.path) {
            let s = rest.remove(i);
            d.presence = s.presence.max(d.presence);
            d.samples = s.samples;
            if d.types.is_empty() {
                d.types = s.types;
            }
        }
    }
    declared.extend(rest);
    InferredSchema {
        sampled: sampled.sampled,
        fields: declared,
    }
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn infers_paths_types_and_presence() {
        let docs = vec![
            doc! { "_id": 1, "name": "a", "address": { "city": "X" }, "items": [ { "sku": "s1" }, { "sku": 2 } ] },
            doc! { "_id": 2, "name": "b", "age": 30_i64 },
            doc! { "_id": 3, "name": Bson::Null, "address": { "city": "Y", "zip": "00-001" } },
        ];
        let s = infer(&docs);
        assert_eq!(s.sampled, 3);
        let f = |p: &str| s.fields.iter().find(|f| f.path == p).unwrap();
        assert_eq!(s.fields[0].path, "_id");
        assert_eq!(f("name").presence, 1.0);
        assert_eq!(f("name").types[0].0, "string");
        assert!((f("address.city").presence - 2.0 / 3.0).abs() < 1e-6);
        assert_eq!(f("items").types, vec![("array".to_owned(), 1.0)]);
        let sku = f("items.sku");
        assert_eq!(sku.types.len(), 2);
        assert!((sku.presence - 1.0 / 3.0).abs() < 1e-6);
        assert_eq!(f("age").types[0].0, "long");
        assert_eq!(f("address.city").samples, vec!["X", "Y"]);
    }

    #[test]
    fn validator_takes_precedence() {
        let v = doc! { "$jsonSchema": { "required": ["email"], "properties": {
            "email": { "bsonType": "string" },
            "tags": { "bsonType": "array", "items": { "properties": { "label": { "bsonType": "string" } } } },
        } } };
        let s = with_validator(
            Some(&v),
            infer(&[doc! { "_id": 1, "email": "e", "extra": true }]),
        );
        let paths: Vec<&str> = s.fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["email", "tags", "tags.label", "_id", "extra"]);
        assert_eq!(s.fields[0].presence, 1.0);
        assert_eq!(s.fields[0].samples, vec!["e"]);
    }
}
