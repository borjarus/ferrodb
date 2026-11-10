//! MQL completion: what is expected at the cursor, from the brackets and
//! the method chain before it.
//!
//! | Context | Suggestions |
//! |---|---|
//! | start of a statement | `db`, `use`, `show` |
//! | `db.` | collections, database methods |
//! | `db.coll.` | collection methods |
//! | `db.coll.find(…).` | cursor methods |
//! | key of a filter (`find`, `count…`, `update…`/`delete…` argument 1, `$match`) | fields, `$and`/`$or`…, under a field: `$gt`, `$in`… |
//! | key of an update (argument 2) | `$set`, `$inc`…; inside them: fields |
//! | projection / sort / index keys / `$project`, `$sort` | fields |
//! | pipeline stage | `$match`, `$group`… |
//! | `"$` in a pipeline | `$field` references |
//! | value | `ObjectId(…)`, `ISODate(…)`… |

use std::ops::Range;

use ferrodb_api::CompletionKind;

use super::{
    COLLECTION_METHODS, CURSOR_METHODS, DB_METHODS,
    lexer::{Tok, lex},
    split,
};

/// Schema knowledge used for suggestions.
pub trait Schema {
    fn collections(&self) -> Vec<String>;
    /// Field paths of a collection (`address.city`), most frequent first.
    fn fields(&self, collection: &str) -> Vec<String>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: Option<&'static str>,
}

pub const QUERY_OPERATORS: &[&str] = &[
    "$and",
    "$or",
    "$nor",
    "$expr",
    "$text",
    "$where",
    "$jsonSchema",
    "$comment",
];
pub const FIELD_OPERATORS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$regex",
    "$options",
    "$not",
    "$all",
    "$elemMatch",
    "$size",
    "$mod",
    "$geoWithin",
    "$near",
];
pub const UPDATE_OPERATORS: &[&str] = &[
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$rename",
    "$setOnInsert",
    "$currentDate",
    "$push",
    "$addToSet",
    "$pull",
    "$pullAll",
    "$pop",
    "$each",
    "$position",
    "$slice",
    "$sort",
];
pub const STAGES: &[&str] = &[
    "$match",
    "$group",
    "$project",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$lookup",
    "$addFields",
    "$set",
    "$unset",
    "$count",
    "$facet",
    "$bucket",
    "$bucketAuto",
    "$replaceRoot",
    "$replaceWith",
    "$sample",
    "$sortByCount",
    "$graphLookup",
    "$out",
    "$merge",
    "$unionWith",
    "$densify",
    "$fill",
    "$setWindowFields",
    "$geoNear",
    "$documents",
    "$redact",
];
pub const EXPRESSION_OPERATORS: &[&str] = &[
    "$sum",
    "$avg",
    "$min",
    "$max",
    "$first",
    "$last",
    "$push",
    "$addToSet",
    "$count",
    "$concat",
    "$substr",
    "$toLower",
    "$toUpper",
    "$cond",
    "$ifNull",
    "$switch",
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$and",
    "$or",
    "$not",
    "$add",
    "$subtract",
    "$multiply",
    "$divide",
    "$size",
    "$arrayElemAt",
    "$filter",
    "$map",
    "$reduce",
    "$dateToString",
    "$year",
    "$month",
    "$dayOfMonth",
    "$toString",
    "$toInt",
    "$toDate",
    "$objectToArray",
    "$mergeObjects",
    "$literal",
    "$in",
    "$split",
    "$round",
];
const HELPERS: &[&str] = &[
    "ObjectId(\"\")",
    "ISODate(\"\")",
    "NumberLong(\"\")",
    "NumberDecimal(\"\")",
    "NumberInt()",
    "UUID(\"\")",
    "Timestamp()",
    "true",
    "false",
    "null",
];

#[derive(Clone, Debug)]
enum Frame {
    Call { method: String, arg: usize },
    Object { key: Option<String>, at_key: bool },
    Array,
}

/// Where the cursor is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Context {
    Statement,
    DbMember,
    CollectionMember {
        collection: String,
    },
    CursorMember,
    Key {
        collection: Option<String>,
        role: Role,
    },
    Value {
        collection: Option<String>,
        field_ref: bool,
    },
    CollectionName,
    FieldName {
        collection: Option<String>,
    },
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Filter top level: fields and logical operators.
    Query,
    /// Below a field of a filter: comparison operators.
    FieldCondition,
    /// Update document top level.
    Update,
    /// Field names only (projection, sort, `$set` contents, index keys).
    Fields,
    /// Pipeline stage names.
    Stage,
    /// Expression keys (`$group` …): operators.
    Expression,
}

fn role_of(method: &str, arg: usize) -> Role {
    match (method, arg) {
        (
            "find" | "findOne" | "countDocuments" | "count" | "deleteOne" | "deleteMany" | "remove"
            | "updateOne" | "updateMany" | "update" | "replaceOne" | "findOneAndUpdate"
            | "findOneAndReplace" | "findOneAndDelete",
            0,
        ) => Role::Query,
        ("distinct", 1) => Role::Query,
        ("updateOne" | "updateMany" | "update" | "findOneAndUpdate", 1) => Role::Update,
        ("aggregate", 0) => Role::Stage,
        _ => Role::Fields,
    }
}

/// The context at the end of `before` (the statement text up to the cursor).
pub fn context(before: &str) -> (Context, String) {
    let (toks, _) = lex(before);
    let mut sig: Vec<&Tok> = toks
        .iter()
        .filter(|t| !matches!(t.tok, Tok::Comment | Tok::Newline))
        .map(|t| &t.tok)
        .collect();
    // An unterminated string at the cursor: complete inside it.
    let in_string = toks.last().is_some_and(|t| {
        matches!(t.tok, Tok::Str(_)) && t.range.end == before.len() && {
            let raw = &before[t.range.clone()];
            raw.len() < 2 || !raw.ends_with(&raw[..1])
        }
    });
    let ends_with_word = before
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$');
    let prefix = if in_string {
        match sig.pop() {
            Some(Tok::Str(s)) => s.clone(),
            _ => String::new(),
        }
    } else if ends_with_word {
        match sig.last() {
            Some(Tok::Ident(s) | Tok::Number(s)) => {
                let s = s.clone();
                sig.pop();
                s
            }
            _ => String::new(),
        }
    } else {
        String::new()
    };

    // Method chain and bracket frames.
    let mut frames: Vec<Frame> = Vec::new();
    let mut chain: Vec<String> = Vec::new();
    let mut collection: Option<String> = None;
    let mut first_method: Option<String> = None;
    let mut last_ident: Option<String> = None;
    for (i, tok) in sig.iter().enumerate() {
        match tok {
            Tok::Ident(s) => {
                if frames.is_empty() {
                    chain.push(s.clone());
                }
                last_ident = Some(s.clone());
            }
            Tok::Str(s) => last_ident = Some(s.clone()),
            Tok::Punct('(') => {
                let method = if frames.is_empty() {
                    chain.pop().unwrap_or_default()
                } else {
                    last_ident.clone().unwrap_or_default()
                };
                if frames.is_empty() {
                    if method == "getCollection" {
                        // collection set when the string argument closes.
                    } else if first_method.is_none() {
                        if chain.len() > 1 {
                            collection = Some(chain[1..].join("."));
                        }
                        first_method = Some(method.clone());
                    }
                }
                frames.push(Frame::Call { method, arg: 0 });
            }
            Tok::Punct(')') => {
                if let Some(Frame::Call { method, .. }) = frames.pop()
                    && frames.is_empty()
                    && method == "getCollection"
                    && let Some(Tok::Str(name)) = sig.get(i.wrapping_sub(1))
                {
                    chain.push(name.clone());
                }
            }
            Tok::Punct('{') => frames.push(Frame::Object {
                key: None,
                at_key: true,
            }),
            // Also `db["coll"]`: the name is taken when the bracket closes.
            Tok::Punct('[') => frames.push(Frame::Array),
            Tok::Punct('}') | Tok::Punct(']') => {
                let was_array = matches!(frames.pop(), Some(Frame::Array));
                if frames.is_empty()
                    && was_array
                    && let Some(Tok::Str(name)) = sig.get(i.wrapping_sub(1))
                {
                    chain.push(name.clone());
                }
            }
            Tok::Punct(',') => match frames.last_mut() {
                Some(Frame::Call { arg, .. }) => *arg += 1,
                Some(Frame::Object { at_key, key }) => {
                    *at_key = true;
                    *key = None;
                }
                _ => {}
            },
            Tok::Punct(':') => {
                if let Some(Frame::Object { at_key, key }) = frames.last_mut() {
                    *at_key = false;
                    *key = last_ident.clone();
                }
            }
            _ => {}
        }
    }
    if collection.is_none() && first_method.is_none() && chain.len() > 1 {
        collection = Some(chain[1..].join("."));
    }

    let ctx = match frames.last() {
        None => {
            let dot = matches!(sig.last(), Some(Tok::Punct('.')));
            let closed_call = matches!(sig.iter().rev().nth(1), Some(Tok::Punct(')')));
            if sig.is_empty() {
                Context::Statement
            } else if dot && closed_call && first_method.is_some() {
                Context::CursorMember
            } else if dot && chain.first().is_some_and(|c| c == "db") {
                if chain.len() == 1 {
                    Context::DbMember
                } else {
                    Context::CollectionMember {
                        collection: chain[1..].join("."),
                    }
                }
            } else {
                Context::None
            }
        }
        Some(Frame::Call { method, arg }) => {
            if in_string && (method == "getCollection" || method == "getSiblingDB") {
                if method == "getCollection" {
                    Context::CollectionName
                } else {
                    Context::None
                }
            } else if in_string && method == "distinct" && *arg == 0 {
                Context::FieldName { collection }
            } else {
                Context::None
            }
        }
        Some(Frame::Array) if frames.len() == 1 => {
            if in_string {
                Context::CollectionName
            } else {
                Context::None
            }
        }
        Some(top) => {
            // Role from the call argument and the enclosing objects.
            let mut role = Role::Fields;
            let mut call_seen = false;
            let n = frames.len();
            for (i, f) in frames.iter().enumerate() {
                match f {
                    Frame::Call { method, arg } => {
                        role = role_of(method, *arg);
                        call_seen = true;
                    }
                    Frame::Object { key, .. } if call_seen => {
                        if i + 1 < n {
                            let k = key.clone().unwrap_or_default();
                            role = match role {
                                Role::Stage => match k.as_str() {
                                    "$match" => Role::Query,
                                    "$project" | "$sort" | "$set" | "$addFields" | "$unset" => {
                                        Role::Fields
                                    }
                                    _ => Role::Expression,
                                },
                                Role::Query if k.starts_with('$') => Role::Query,
                                Role::Query => Role::FieldCondition,
                                Role::Update => Role::Fields,
                                Role::FieldCondition if k == "$elemMatch" => Role::Query,
                                r => r,
                            };
                        }
                    }
                    Frame::Array if call_seen => {
                        // Arrays keep the role ($and: [...], pipelines).
                    }
                    _ => {}
                }
            }
            match top {
                Frame::Object { at_key: true, .. } => Context::Key { collection, role },
                Frame::Object { .. } | Frame::Array => {
                    let agg = first_method.as_deref() == Some("aggregate");
                    Context::Value {
                        collection,
                        field_ref: agg && in_string && prefix.starts_with('$'),
                    }
                }
                Frame::Call { .. } => Context::None,
            }
        }
    };
    (ctx, prefix)
}

fn words(list: &[&str], kind: CompletionKind, detail: Option<&'static str>) -> Vec<Suggestion> {
    list.iter()
        .map(|w| Suggestion {
            label: (*w).to_owned(),
            kind,
            detail,
        })
        .collect()
}

fn fields(schema: &dyn Schema, collection: &Option<String>) -> Vec<Suggestion> {
    collection
        .as_deref()
        .map(|c| schema.fields(c))
        .unwrap_or_default()
        .into_iter()
        .map(|f| Suggestion {
            label: f,
            kind: CompletionKind::Field,
            detail: None,
        })
        .collect()
}

/// Candidates for a context (unfiltered).
pub fn candidates(ctx: &Context, prefix: &str, schema: &dyn Schema) -> Vec<Suggestion> {
    use CompletionKind as K;
    match ctx {
        Context::Statement => words(&["db", "use", "show"], K::Keyword, None),
        Context::DbMember => {
            let mut v: Vec<Suggestion> = schema
                .collections()
                .into_iter()
                .map(|c| Suggestion {
                    label: c,
                    kind: K::Collection,
                    detail: None,
                })
                .collect();
            v.extend(words(DB_METHODS, K::Method, Some("database")));
            v
        }
        Context::CollectionMember { .. } => words(COLLECTION_METHODS, K::Method, None),
        Context::CursorMember => words(CURSOR_METHODS, K::Method, Some("cursor")),
        Context::CollectionName => schema
            .collections()
            .into_iter()
            .map(|c| Suggestion {
                label: c,
                kind: K::Collection,
                detail: None,
            })
            .collect(),
        Context::FieldName { collection } => fields(schema, collection),
        Context::Key { collection, role } => {
            let ops: &[&str] = match role {
                Role::Query => QUERY_OPERATORS,
                Role::FieldCondition => FIELD_OPERATORS,
                Role::Update => UPDATE_OPERATORS,
                Role::Stage => STAGES,
                Role::Expression => EXPRESSION_OPERATORS,
                Role::Fields => &[],
            };
            let mut v = words(ops, K::Operator, None);
            let with_fields = matches!(role, Role::Query | Role::Fields)
                || (!prefix.starts_with('$') && *role == Role::Expression);
            if with_fields && !prefix.starts_with('$') {
                let mut f = fields(schema, collection);
                f.append(&mut v);
                v = f;
            }
            v
        }
        Context::Value {
            collection,
            field_ref,
        } => {
            if *field_ref {
                fields(schema, collection)
                    .into_iter()
                    .map(|mut s| {
                        s.label = format!("${}", s.label);
                        s
                    })
                    .chain(words(EXPRESSION_OPERATORS, K::Operator, None))
                    .collect()
            } else if prefix.starts_with('$') {
                words(EXPRESSION_OPERATORS, K::Operator, None)
            } else {
                words(HELPERS, K::Function, None)
            }
        }
        Context::None => Vec::new(),
    }
}

/// Suggestions at `offset`, ranked: prefix matches first (shorter first),
/// then other substring matches. Returns the range the prefix occupies.
pub fn complete(text: &str, offset: usize, schema: &dyn Schema) -> (Range<usize>, Vec<Suggestion>) {
    let offset = offset.min(text.len());
    let start = split(&text[..offset])
        .last()
        .filter(|s| {
            // The cursor is still inside / right after the last statement.
            let tail = &text[s.range.end..offset];
            !tail.contains(';') && !(tail.contains('\n') && !tail.trim_start().starts_with('.'))
        })
        .map_or(offset, |s| s.range.start);
    let start = if start > offset { offset } else { start };
    let before = &text[start..offset];
    let (ctx, prefix) = context(before);
    let lower = prefix.to_lowercase();
    let mut scored: Vec<(u8, usize, usize, Suggestion)> = candidates(&ctx, &prefix, schema)
        .into_iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let l = s.label.to_lowercase();
            let rank = if lower.is_empty() || l.starts_with(&lower) {
                0
            } else if l
                .split(['.', '_'])
                .any(|part| part.starts_with(&lower.trim_start_matches('$').to_owned()))
                && !lower.trim_start_matches('$').is_empty()
            {
                1
            } else if l.contains(&lower) {
                2
            } else {
                return None;
            };
            Some((rank, if lower.is_empty() { 0 } else { s.label.len() }, i, s))
        })
        .collect();
    scored.sort_by_key(|(r, len, i, _)| (*r, *len, *i));
    scored.dedup_by(|a, b| a.3.label == b.3.label);
    let replace = offset - prefix.len().min(offset)..offset;
    (replace, scored.into_iter().map(|x| x.3).collect())
}

#[cfg(test)]
mod tests;
