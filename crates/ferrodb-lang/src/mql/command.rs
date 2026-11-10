//! Shell statements → [`Command`]s.
//!
//! Grammar (a subset of mongosh):
//!
//! ```text
//! statement := "use" name | "show" ("dbs" | "databases" | "collections" | "tables")
//!            | "db" access* call ("." call)*
//! access    := "." name | "[" string "]" | ".getCollection(" string ")"
//!            | ".getSiblingDB(" string ")"
//! ```
//!
//! `db.a.b.find()` addresses the collection `a.b`, as in mongosh.

use std::ops::Range;

use bson::{Bson, Document};

use super::{
    lexer::{Tok, Token, lex},
    value::{ParseError, Tokens, parse_args},
};

/// Which call of a `findOneAnd…` family.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindAndModify {
    Update,
    Replace,
    Delete,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Use(String),
    ShowDatabases,
    ShowCollections,
    Find {
        collection: String,
        filter: Document,
        projection: Option<Document>,
        sort: Option<Document>,
        skip: Option<u64>,
        limit: Option<i64>,
        /// Remaining cursor options (`hint`, `collation`, `maxTimeMS`,
        /// `batchSize`, `comment`).
        options: Document,
        /// `findOne`.
        one: bool,
        /// `.count()` on the cursor.
        count: bool,
    },
    Aggregate {
        /// `None` = `db.aggregate([...])` (database-level stages).
        collection: Option<String>,
        pipeline: Vec<Document>,
        options: Document,
    },
    CountDocuments {
        collection: String,
        filter: Document,
        options: Document,
    },
    EstimatedCount {
        collection: String,
    },
    Distinct {
        collection: String,
        field: String,
        filter: Document,
    },
    Insert {
        collection: String,
        documents: Vec<Document>,
        /// `insertMany` (result lists every id).
        many: bool,
        ordered: bool,
    },
    Update {
        collection: String,
        filter: Document,
        /// Update document or pipeline (array).
        update: Bson,
        many: bool,
        options: Document,
    },
    Replace {
        collection: String,
        filter: Document,
        replacement: Document,
        options: Document,
    },
    Delete {
        collection: String,
        filter: Document,
        many: bool,
    },
    FindAndModify {
        collection: String,
        kind: FindAndModify,
        filter: Document,
        /// Update / replacement (`Null` for delete).
        argument: Bson,
        options: Document,
    },
    CreateIndex {
        collection: String,
        /// One or several key specifications.
        keys: Vec<Document>,
        options: Document,
    },
    DropIndex {
        collection: String,
        /// Name or key specification; `None` = every index (`dropIndexes()`).
        index: Option<Bson>,
    },
    GetIndexes {
        collection: String,
    },
    DropCollection {
        collection: String,
    },
    RenameCollection {
        collection: String,
        to: String,
        drop_target: bool,
    },
    CreateCollection {
        name: String,
        options: Document,
    },
    CreateView {
        name: String,
        source: String,
        pipeline: Vec<Document>,
        options: Document,
    },
    DropDatabase,
    /// `db.getCollectionInfos(filter)` / `db.getCollectionNames()`.
    ListCollections {
        filter: Document,
        names_only: bool,
    },
    RunCommand {
        command: Document,
        admin: bool,
    },
}

/// One parsed statement.
#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    /// Database other than the session's (`db.getSiblingDB("x")`, or set
    /// by the IDE for introspection).
    pub database: Option<String>,
    pub op: Op,
}

impl Command {
    pub fn new(op: Op) -> Self {
        Self { database: None, op }
    }

    pub fn on(database: impl Into<String>, op: Op) -> Self {
        Self {
            database: Some(database.into()),
            op,
        }
    }

    /// The collection a command targets.
    pub fn collection(&self) -> Option<&str> {
        match &self.op {
            Op::Find { collection, .. }
            | Op::CountDocuments { collection, .. }
            | Op::EstimatedCount { collection }
            | Op::Distinct { collection, .. }
            | Op::Insert { collection, .. }
            | Op::Update { collection, .. }
            | Op::Replace { collection, .. }
            | Op::Delete { collection, .. }
            | Op::FindAndModify { collection, .. }
            | Op::CreateIndex { collection, .. }
            | Op::DropIndex { collection, .. }
            | Op::GetIndexes { collection }
            | Op::DropCollection { collection }
            | Op::RenameCollection { collection, .. } => Some(collection),
            Op::Aggregate { collection, .. } => collection.as_deref(),
            Op::CreateCollection { name, .. } | Op::CreateView { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Whether the command can change data or schema.
    pub fn is_write(&self) -> bool {
        match &self.op {
            Op::Use(_)
            | Op::ShowDatabases
            | Op::ShowCollections
            | Op::Find { .. }
            | Op::CountDocuments { .. }
            | Op::EstimatedCount { .. }
            | Op::Distinct { .. }
            | Op::GetIndexes { .. }
            | Op::ListCollections { .. } => false,
            Op::Aggregate { pipeline, .. } => pipeline
                .iter()
                .any(|s| s.contains_key("$out") || s.contains_key("$merge")),
            Op::RunCommand { command, .. } => !read_command(command),
            _ => true,
        }
    }
}

/// Commands known not to modify anything.
fn read_command(cmd: &Document) -> bool {
    let Some(name) = cmd.keys().next() else {
        return true;
    };
    let reads = [
        "find",
        "count",
        "distinct",
        "listCollections",
        "listIndexes",
        "listDatabases",
        "dbStats",
        "collStats",
        "serverStatus",
        "ping",
        "buildInfo",
        "buildinfo",
        "hello",
        "isMaster",
        "ismaster",
        "explain",
        "connectionStatus",
        "hostInfo",
        "getParameter",
        "getCmdLineOpts",
        "currentOp",
        "replSetGetStatus",
        "replSetGetConfig",
        "top",
        "usersInfo",
        "rolesInfo",
        "validate",
        "dataSize",
        "getLog",
        "listCommands",
        "whatsmyuri",
        "features",
        "lockInfo",
        "profile",
    ];
    if name == "aggregate" {
        return !cmd
            .get_array("pipeline")
            .map(|p| {
                p.iter().any(|s| {
                    s.as_document()
                        .is_some_and(|d| d.contains_key("$out") || d.contains_key("$merge"))
                })
            })
            .unwrap_or(false);
    }
    reads.iter().any(|r| r.eq_ignore_ascii_case(name))
        && !(name == "profile" && cmd.get("profile").is_some_and(|v| v != &Bson::Int32(-1)))
}

/// Database methods (`db.<method>(…)`); anything else is a collection.
pub const DB_METHODS: &[&str] = &[
    "getCollection",
    "getSiblingDB",
    "getCollectionNames",
    "getCollectionInfos",
    "createCollection",
    "createView",
    "dropDatabase",
    "runCommand",
    "adminCommand",
    "aggregate",
    "getName",
    "stats",
    "version",
    "serverStatus",
    "hostInfo",
    "currentOp",
    "killOp",
    "listCommands",
];

/// Collection methods.
pub const COLLECTION_METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "countDocuments",
    "estimatedDocumentCount",
    "count",
    "distinct",
    "insertOne",
    "insertMany",
    "insert",
    "updateOne",
    "updateMany",
    "update",
    "replaceOne",
    "deleteOne",
    "deleteMany",
    "remove",
    "findOneAndUpdate",
    "findOneAndReplace",
    "findOneAndDelete",
    "createIndex",
    "createIndexes",
    "ensureIndex",
    "dropIndex",
    "dropIndexes",
    "getIndexes",
    "drop",
    "renameCollection",
    "stats",
    "bulkWrite",
];

/// Cursor methods after `find(…)` / `aggregate(…)`.
pub const CURSOR_METHODS: &[&str] = &[
    "sort",
    "limit",
    "skip",
    "projection",
    "hint",
    "maxTimeMS",
    "batchSize",
    "collation",
    "comment",
    "count",
    "toArray",
    "pretty",
    "allowDiskUse",
];

struct Call {
    name: String,
    args: Vec<Bson>,
    range: Range<usize>,
}

fn doc_arg(c: &Call, i: usize) -> Result<Document, ParseError> {
    match c.args.get(i) {
        None => Ok(Document::new()),
        Some(Bson::Document(d)) => Ok(d.clone()),
        Some(_) => Err(ParseError::new(
            format!("{}: argument {} must be a document", c.name, i + 1),
            c.range.clone(),
        )),
    }
}

fn opt_doc(c: &Call, i: usize) -> Result<Option<Document>, ParseError> {
    match c.args.get(i) {
        None | Some(Bson::Null | Bson::Undefined) => Ok(None),
        _ => doc_arg(c, i).map(Some),
    }
}

fn need_doc(c: &Call, i: usize) -> Result<Document, ParseError> {
    if c.args.len() <= i {
        return Err(ParseError::new(
            format!("{}: argument {} is required", c.name, i + 1),
            c.range.clone(),
        ));
    }
    doc_arg(c, i)
}

fn str_arg(c: &Call, i: usize) -> Result<String, ParseError> {
    match c.args.get(i) {
        Some(Bson::String(s)) => Ok(s.clone()),
        _ => Err(ParseError::new(
            format!("{}: argument {} must be a string", c.name, i + 1),
            c.range.clone(),
        )),
    }
}

fn int_value(v: &Bson) -> Option<i64> {
    match v {
        Bson::Int32(i) => Some(*i as i64),
        Bson::Int64(i) => Some(*i),
        Bson::Double(d) if d.fract() == 0.0 => Some(*d as i64),
        _ => None,
    }
}

fn pipeline(c: &Call, i: usize) -> Result<Vec<Document>, ParseError> {
    match c.args.get(i) {
        None => Ok(Vec::new()),
        Some(Bson::Array(a)) => a
            .iter()
            .map(|s| match s {
                Bson::Document(d) => Ok(d.clone()),
                _ => Err(ParseError::new(
                    format!("{}: every pipeline stage must be a document", c.name),
                    c.range.clone(),
                )),
            })
            .collect(),
        // `aggregate({$match…}, {$group…})` (legacy varargs form).
        Some(Bson::Document(_)) => c
            .args
            .iter()
            .map(|s| match s {
                Bson::Document(d) => Ok(d.clone()),
                _ => Err(ParseError::new("expected a pipeline", c.range.clone())),
            })
            .collect(),
        Some(_) => Err(ParseError::new(
            format!("{}: expected a pipeline (array)", c.name),
            c.range.clone(),
        )),
    }
}

fn docs_arg(c: &Call, i: usize) -> Result<Vec<Document>, ParseError> {
    match c.args.get(i) {
        Some(Bson::Array(a)) => a
            .iter()
            .map(|d| match d {
                Bson::Document(d) => Ok(d.clone()),
                _ => Err(ParseError::new("expected documents", c.range.clone())),
            })
            .collect(),
        Some(Bson::Document(d)) => Ok(vec![d.clone()]),
        _ => Err(ParseError::new(
            format!("{}: expected an array of documents", c.name),
            c.range.clone(),
        )),
    }
}

/// Parses one statement (as found by [`super::split`]).
pub fn parse(src: &str) -> Result<Command, ParseError> {
    let (toks, errs) = lex(src);
    if let Some(e) = errs.into_iter().next() {
        return Err(ParseError::new(e.message, e.range));
    }
    let mut t = Tokens::new(&toks, src.trim_end().len());
    let cmd = parse_tokens(&mut t)?;
    t.eat(';');
    if !t.at_end() {
        return Err(ParseError::new(
            "unexpected input after the statement",
            t.here(),
        ));
    }
    Ok(cmd)
}

fn ident(t: &mut Tokens<'_>) -> Option<(String, Range<usize>)> {
    match t.peek() {
        Some(Token {
            tok: Tok::Ident(s),
            range,
        }) => {
            t.next();
            Some((s.clone(), range.clone()))
        }
        _ => None,
    }
}

fn parse_tokens(t: &mut Tokens<'_>) -> Result<Command, ParseError> {
    let Some((first, r)) = ident(t) else {
        return Err(ParseError::new("expected `db`, `use` or `show`", t.here()));
    };
    match first.as_str() {
        "use" => {
            let name = match t.next() {
                Some(Token {
                    tok: Tok::Ident(n) | Tok::Str(n),
                    ..
                }) => n.clone(),
                _ => return Err(ParseError::new("use: expected a database name", t.here())),
            };
            Ok(Command::new(Op::Use(name)))
        }
        "show" => match ident(t).map(|(s, _)| s).as_deref() {
            Some("dbs" | "databases") => Ok(Command::new(Op::ShowDatabases)),
            Some("collections" | "tables") => Ok(Command::new(Op::ShowCollections)),
            _ => Err(ParseError::new("show: expected `dbs` or `collections`", r)),
        },
        "db" => parse_db(t),
        other => Err(ParseError::new(
            format!("unknown statement `{other}`: expected `db`, `use` or `show`"),
            r,
        )),
    }
}

fn parse_db(t: &mut Tokens<'_>) -> Result<Command, ParseError> {
    let mut database = None;
    let mut path: Vec<String> = Vec::new();
    // Accessors and the first call.
    let first = loop {
        if t.eat('[') {
            match t.next() {
                Some(Token {
                    tok: Tok::Str(s), ..
                }) => path.push(s.clone()),
                _ => return Err(ParseError::new("expected a collection name", t.here())),
            }
            t.expect(']')?;
            continue;
        }
        if !t.eat('.') {
            return Err(ParseError::new("expected `.` and a method", t.here()));
        }
        let Some((name, range)) = ident(t) else {
            return Err(ParseError::new("expected a name", t.here()));
        };
        if !t.is_punct('(') {
            path.push(name);
            continue;
        }
        t.next();
        let args = parse_args(t)?;
        let call = Call { name, args, range };
        match call.name.as_str() {
            "getCollection" if path.is_empty() => path.push(str_arg(&call, 0)?),
            "getSiblingDB" if path.is_empty() => database = Some(str_arg(&call, 0)?),
            _ => break call,
        }
    };
    let mut calls = vec![first];
    while t.eat('.') {
        let Some((name, range)) = ident(t) else {
            return Err(ParseError::new("expected a method", t.here()));
        };
        t.expect('(')?;
        let args = parse_args(t)?;
        calls.push(Call { name, args, range });
    }
    let op = if path.is_empty() {
        db_method(&calls)?
    } else {
        collection_method(path.join("."), &calls)?
    };
    Ok(Command { database, op })
}

fn no_modifiers(calls: &[Call]) -> Result<(), ParseError> {
    match calls.get(1) {
        Some(c) if !matches!(c.name.as_str(), "toArray" | "pretty") => Err(ParseError::new(
            format!("`{}` cannot follow `{}`", c.name, calls[0].name),
            c.range.clone(),
        )),
        _ => Ok(()),
    }
}

fn db_method(calls: &[Call]) -> Result<Op, ParseError> {
    let c = &calls[0];
    let op = match c.name.as_str() {
        "getCollectionNames" => Op::ListCollections {
            filter: doc_arg(c, 0)?,
            names_only: true,
        },
        "getCollectionInfos" => Op::ListCollections {
            filter: doc_arg(c, 0)?,
            names_only: false,
        },
        "createCollection" => Op::CreateCollection {
            name: str_arg(c, 0)?,
            options: doc_arg(c, 1)?,
        },
        "createView" => Op::CreateView {
            name: str_arg(c, 0)?,
            source: str_arg(c, 1)?,
            pipeline: pipeline(c, 2)?,
            options: doc_arg(c, 3)?,
        },
        "dropDatabase" => Op::DropDatabase,
        "runCommand" | "adminCommand" => {
            let command = match c.args.first() {
                Some(Bson::String(s)) => bson::doc! { s.clone(): 1 },
                _ => need_doc(c, 0)?,
            };
            Op::RunCommand {
                command,
                admin: c.name == "adminCommand",
            }
        }
        "aggregate" => Op::Aggregate {
            collection: None,
            pipeline: pipeline(c, 0)?,
            options: doc_arg(c, 1)?,
        },
        "stats" => Op::RunCommand {
            command: bson::doc! { "dbStats": 1 },
            admin: false,
        },
        "serverStatus" => Op::RunCommand {
            command: bson::doc! { "serverStatus": 1 },
            admin: true,
        },
        "hostInfo" => Op::RunCommand {
            command: bson::doc! { "hostInfo": 1 },
            admin: true,
        },
        "version" => Op::RunCommand {
            command: bson::doc! { "buildInfo": 1 },
            admin: true,
        },
        "currentOp" => Op::RunCommand {
            command: {
                let mut d = bson::doc! { "currentOp": 1 };
                d.extend(doc_arg(c, 0)?);
                d
            },
            admin: true,
        },
        "killOp" => Op::RunCommand {
            command: bson::doc! { "killOp": 1, "op": c.args.first().cloned().unwrap_or(Bson::Null) },
            admin: true,
        },
        "listCommands" => Op::RunCommand {
            command: bson::doc! { "listCommands": 1 },
            admin: false,
        },
        other => {
            return Err(ParseError::new(
                format!("unknown database method `{other}`"),
                c.range.clone(),
            ));
        }
    };
    no_modifiers(calls)?;
    Ok(op)
}

fn collection_method(collection: String, calls: &[Call]) -> Result<Op, ParseError> {
    let c = &calls[0];
    let name = c.name.as_str();
    let op = match name {
        "find" | "findOne" => {
            let mut op = Op::Find {
                collection,
                filter: doc_arg(c, 0)?,
                projection: opt_doc(c, 1)?,
                sort: None,
                skip: None,
                limit: None,
                options: doc_arg(c, 2)?,
                one: name == "findOne",
                count: false,
            };
            for m in &calls[1..] {
                cursor_modifier(&mut op, m)?;
            }
            return Ok(op);
        }
        "aggregate" => {
            let mut op = Op::Aggregate {
                collection: Some(collection),
                pipeline: pipeline(c, 0)?,
                options: if matches!(c.args.first(), Some(Bson::Array(_))) {
                    doc_arg(c, 1)?
                } else {
                    Document::new()
                },
            };
            for m in &calls[1..] {
                cursor_modifier(&mut op, m)?;
            }
            return Ok(op);
        }
        "countDocuments" | "count" => Op::CountDocuments {
            collection,
            filter: doc_arg(c, 0)?,
            options: doc_arg(c, 1)?,
        },
        "estimatedDocumentCount" => Op::EstimatedCount { collection },
        "distinct" => Op::Distinct {
            collection,
            field: str_arg(c, 0)?,
            filter: doc_arg(c, 1)?,
        },
        "insertOne" => Op::Insert {
            collection,
            documents: vec![need_doc(c, 0)?],
            many: false,
            ordered: true,
        },
        "insertMany" | "insert" => {
            let opts = doc_arg(c, 1)?;
            Op::Insert {
                collection,
                documents: docs_arg(c, 0)?,
                many: name == "insertMany" || matches!(c.args.first(), Some(Bson::Array(_))),
                ordered: opts.get_bool("ordered").unwrap_or(true),
            }
        }
        "updateOne" | "updateMany" | "update" => {
            let mut options = doc_arg(c, 2)?;
            let many = name == "updateMany"
                || (name == "update" && options.get_bool("multi").unwrap_or(false));
            options.remove("multi");
            let update = match c.args.get(1) {
                Some(v @ (Bson::Document(_) | Bson::Array(_))) => v.clone(),
                _ => {
                    return Err(ParseError::new(
                        format!("{name}: argument 2 must be an update document or pipeline"),
                        c.range.clone(),
                    ));
                }
            };
            Op::Update {
                collection,
                filter: need_doc(c, 0)?,
                update,
                many,
                options,
            }
        }
        "replaceOne" => Op::Replace {
            collection,
            filter: need_doc(c, 0)?,
            replacement: need_doc(c, 1)?,
            options: doc_arg(c, 2)?,
        },
        "deleteOne" | "deleteMany" | "remove" => {
            let just_one = matches!(c.args.get(1), Some(Bson::Boolean(true)))
                || matches!(c.args.get(1), Some(Bson::Document(d)) if d.get_bool("justOne").unwrap_or(false));
            Op::Delete {
                collection,
                filter: need_doc(c, 0)?,
                many: name == "deleteMany" || (name == "remove" && !just_one),
            }
        }
        "findOneAndUpdate" | "findOneAndReplace" | "findOneAndDelete" => {
            let kind = match name {
                "findOneAndUpdate" => FindAndModify::Update,
                "findOneAndReplace" => FindAndModify::Replace,
                _ => FindAndModify::Delete,
            };
            let (argument, opts_at) = if kind == FindAndModify::Delete {
                (Bson::Null, 1)
            } else {
                (c.args.get(1).cloned().unwrap_or(Bson::Null), 2)
            };
            if kind != FindAndModify::Delete && argument == Bson::Null {
                return Err(ParseError::new(
                    format!("{name}: argument 2 is required"),
                    c.range.clone(),
                ));
            }
            Op::FindAndModify {
                collection,
                kind,
                filter: need_doc(c, 0)?,
                argument,
                options: doc_arg(c, opts_at)?,
            }
        }
        "createIndex" | "ensureIndex" => Op::CreateIndex {
            collection,
            keys: vec![need_doc(c, 0)?],
            options: doc_arg(c, 1)?,
        },
        "createIndexes" => Op::CreateIndex {
            collection,
            keys: docs_arg(c, 0)?,
            options: doc_arg(c, 1)?,
        },
        "dropIndex" => Op::DropIndex {
            collection,
            index: Some(c.args.first().cloned().ok_or_else(|| {
                ParseError::new("dropIndex: expected an index name or key", c.range.clone())
            })?),
        },
        "dropIndexes" => Op::DropIndex {
            collection,
            index: c
                .args
                .first()
                .cloned()
                .filter(|v| *v != Bson::String("*".into())),
        },
        "getIndexes" => Op::GetIndexes { collection },
        "drop" => Op::DropCollection { collection },
        "renameCollection" => Op::RenameCollection {
            collection,
            to: str_arg(c, 0)?,
            drop_target: matches!(c.args.get(1), Some(Bson::Boolean(true))),
        },
        "stats" => Op::RunCommand {
            command: bson::doc! { "collStats": collection },
            admin: false,
        },
        "bulkWrite" => {
            return Err(ParseError::new(
                "bulkWrite is not supported yet: run the operations one by one",
                c.range.clone(),
            ));
        }
        other => {
            return Err(ParseError::new(
                format!("unknown collection method `{other}`"),
                c.range.clone(),
            ));
        }
    };
    no_modifiers(calls)?;
    Ok(op)
}

fn cursor_modifier(op: &mut Op, m: &Call) -> Result<(), ParseError> {
    let arg_doc = || doc_arg(m, 0);
    let arg_int = || {
        m.args.first().and_then(int_value).ok_or_else(|| {
            ParseError::new(format!("{}: expected a number", m.name), m.range.clone())
        })
    };
    match op {
        Op::Find {
            sort,
            skip,
            limit,
            projection,
            options,
            count,
            ..
        } => match m.name.as_str() {
            "sort" => *sort = Some(arg_doc()?),
            "limit" => *limit = Some(arg_int()?),
            "skip" => *skip = Some(arg_int()?.max(0) as u64),
            "projection" => *projection = Some(arg_doc()?),
            "count" => *count = true,
            "hint" | "maxTimeMS" | "batchSize" | "collation" | "comment" | "allowDiskUse" => {
                options.insert(
                    m.name.clone(),
                    m.args.first().cloned().unwrap_or(Bson::Boolean(true)),
                );
            }
            "toArray" | "pretty" => {}
            other => {
                return Err(ParseError::new(
                    format!("unknown cursor method `{other}`"),
                    m.range.clone(),
                ));
            }
        },
        Op::Aggregate { options, .. } => match m.name.as_str() {
            "maxTimeMS" | "batchSize" | "collation" | "comment" | "hint" | "allowDiskUse" => {
                options.insert(
                    m.name.clone(),
                    m.args.first().cloned().unwrap_or(Bson::Boolean(true)),
                );
            }
            "toArray" | "pretty" => {}
            other => {
                return Err(ParseError::new(
                    format!("`{other}` cannot follow `aggregate`"),
                    m.range.clone(),
                ));
            }
        },
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use bson::doc;

    use super::*;

    #[test]
    fn find_with_modifiers() {
        let c = parse("db.orders.find({status: 'A'}, {_id: 0}).sort({ts: -1}).limit(10).skip(5)")
            .unwrap();
        assert_eq!(
            c.op,
            Op::Find {
                collection: "orders".into(),
                filter: doc! {"status": "A"},
                projection: Some(doc! {"_id": 0}),
                sort: Some(doc! {"ts": -1}),
                skip: Some(5),
                limit: Some(10),
                options: Document::new(),
                one: false,
                count: false,
            }
        );
        assert!(!c.is_write());
        let c = parse("db.getCollection('a b').findOne()").unwrap();
        assert_eq!(c.collection(), Some("a b"));
        let c = parse("db.system.views.find()").unwrap();
        assert_eq!(c.collection(), Some("system.views"));
        let c = parse("db['x'].countDocuments({})").unwrap();
        assert_eq!(c.collection(), Some("x"));
        let c = parse("db.getSiblingDB('admin').runCommand({ping: 1})").unwrap();
        assert_eq!(c.database.as_deref(), Some("admin"));
        assert!(!c.is_write());
    }

    #[test]
    fn writes_and_classification() {
        let c = parse("db.c.updateMany({}, {$set: {a: 1}}, {upsert: true});").unwrap();
        assert!(c.is_write());
        assert!(matches!(c.op, Op::Update { many: true, .. }));
        assert!(
            parse("db.c.aggregate([{$match: {}}, {$out: 'x'}])")
                .unwrap()
                .is_write()
        );
        assert!(!parse("db.c.aggregate([{$match: {}}])").unwrap().is_write());
        assert!(parse("db.runCommand({drop: 'x'})").unwrap().is_write());
        assert!(!parse("db.runCommand('ping')").unwrap().is_write());
        assert!(matches!(
            parse("db.c.remove({a: 1}, true)").unwrap().op,
            Op::Delete { many: false, .. }
        ));
        assert!(matches!(
            parse("db.c.insert([{a: 1}, {a: 2}])").unwrap().op,
            Op::Insert { many: true, .. }
        ));
        assert!(matches!(parse("use shop").unwrap().op, Op::Use(ref d) if d == "shop"));
        assert_eq!(parse("show dbs").unwrap().op, Op::ShowDatabases);
    }

    #[test]
    fn errors_point_at_the_problem() {
        let e = parse("db.c.fnd({})").unwrap_err();
        assert!(e.message.contains("fnd"));
        assert_eq!(e.range, 5..8);
        let e = parse("db.c.find({a: 1}").unwrap_err();
        assert_eq!(e.message, "expected `)`");
        let e = parse("db.c.insertOne()").unwrap_err();
        assert!(e.message.contains("required"));
        assert!(parse("select 1").is_err());
        assert!(parse("db.c.find().foo()").is_err());
    }
}
