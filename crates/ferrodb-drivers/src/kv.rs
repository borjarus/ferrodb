//! The key-value protocol shared by key-value drivers (Redis), the core and
//! the TCK: native method names, the Arrow shapes of key listings and key
//! values, redis-cli style argument parsing and quoting, console script
//! splitting and command classification (read-only enforcement).
//!
//! Not behind a feature: the core and the TCK use it to read what a driver
//! streams, whatever drivers are compiled in.

use std::{ops::Range, sync::Arc, time::Duration};

use arrow_array::{
    Array, ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use ferrodb_api::{Error, KeyEntry, KeyType, KeyValue, Result, StreamEntry};

use crate::sink::Collected;

/// Native methods of key-value drivers ([`crate::Request::Native`]).
pub mod method {
    /// Keys matching `pattern` (default `*`), `count` per `SCAN` round
    /// (default 500), at most `limit` keys (default: all). Without `cursor`
    /// the whole keyspace is iterated (across every master of a cluster)
    /// and streamed with backpressure; with `cursor` one `SCAN` round runs
    /// and a second result (`cursor`) holds the next cursor (`0` = done; in
    /// a cluster `node/cursor`, walking the masters one after another).
    /// `meta=false` lists names only (type / TTL / size stay empty).
    /// Result: [`super::keys_schema`].
    pub const SCAN: &str = "scan";
    /// One key: [`super::key_info_schema`] (type, TTL, memory, encoding,
    /// length). No row when the key does not exist.
    pub const KEY_INFO: &str = "key_info";
    /// Value of `key`, at most `limit` elements (default 10 000):
    /// [`super::value_schema`].
    pub const VALUE: &str = "value";
    /// `SET key value` (string; `ttl` seconds optional, the TTL is kept
    /// otherwise) or `JSON.SET key $ value` with `kind=json`.
    pub const SET: &str = "set";
    /// `RENAME key new_key` (`RENAMENX` with `nx=true`).
    pub const RENAME: &str = "rename";
    /// `DEL key` (`UNLINK` with `async=true`).
    pub const DELETE: &str = "delete";
    /// `EXPIRE key ttl` (seconds); an empty / missing `ttl` = `PERSIST`.
    pub const EXPIRE: &str = "expire";
    /// Sets one member: hash field `member` = `value`, list index `member`
    /// = `value`, zset member `member` with score `value`, set member
    /// `member` replaced by `value`, stream entry added (`member` = id or
    /// `*`, `value` = `field=value` pairs separated by newlines).
    pub const SET_MEMBER: &str = "set_member";
    /// Adds a member: hash field / list element (`member` ignored, pushed
    /// at the tail) / set member / zset member with score `value`.
    pub const ADD_MEMBER: &str = "add_member";
    /// Removes a member: hash field, list element by value, set / zset
    /// member, stream entry id.
    pub const REMOVE_MEMBER: &str = "remove_member";
    /// Databases with their key counts: `database` (`db0`…), `keys`,
    /// `expires` (from `INFO keyspace` and `CONFIG GET databases`).
    pub const DATABASES: &str = "databases";
}

/// Key listing: `key`, `type`, `ttl_ms` (null = no expiry), `size` (memory
/// in bytes, null when unknown).
pub fn keys_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, true),
        Field::new("ttl_ms", DataType::Int64, true),
        Field::new("size", DataType::UInt64, true),
    ]))
}

/// One key in detail.
pub fn key_info_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("key", DataType::Utf8, false),
        Field::new("type", DataType::Utf8, false),
        Field::new("ttl_ms", DataType::Int64, true),
        Field::new("size", DataType::UInt64, true),
        Field::new("encoding", DataType::Utf8, true),
        Field::new("length", DataType::UInt64, true),
    ]))
}

/// A key value, one row per element: `kind` (Redis type name), `member`
/// (hash field, list index, set / zset member, stream id), `field` (stream
/// entry field), `value` (string, JSON text, hash / stream value), `score`
/// (zset).
pub fn value_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("kind", DataType::Utf8, false),
        Field::new("member", DataType::Utf8, true),
        Field::new("field", DataType::Utf8, true),
        Field::new("value", DataType::Utf8, true),
        Field::new("score", DataType::Float64, true),
    ]))
}

/// The Redis type name (`TYPE` reply) of a key type.
pub fn type_name(t: KeyType) -> &'static str {
    match t {
        KeyType::String => "string",
        KeyType::Hash => "hash",
        KeyType::List => "list",
        KeyType::Set => "set",
        KeyType::ZSet => "zset",
        KeyType::Stream => "stream",
        KeyType::Json => "ReJSON-RL",
        KeyType::Other => "other",
    }
}

/// A `TYPE` reply as a key type (`ReJSON-RL` = RedisJSON).
pub fn key_type(name: &str) -> KeyType {
    match name.to_ascii_lowercase().as_str() {
        "string" => KeyType::String,
        "hash" => KeyType::Hash,
        "list" => KeyType::List,
        "set" => KeyType::Set,
        "zset" => KeyType::ZSet,
        "stream" => KeyType::Stream,
        "rejson-rl" | "json" => KeyType::Json,
        _ => KeyType::Other,
    }
}

/// One row of a key listing, as built by drivers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyRow {
    pub key: String,
    /// `TYPE` reply; `None` = not read (or the key vanished).
    pub kind: Option<String>,
    /// `PTTL` in milliseconds; `None` = no expiry.
    pub ttl_ms: Option<i64>,
    pub size: Option<u64>,
}

/// Key rows as a [`keys_schema`] batch.
pub fn keys_batch(rows: &[KeyRow]) -> RecordBatch {
    let key: StringArray = rows.iter().map(|r| Some(r.key.as_str())).collect();
    let kind: StringArray = rows.iter().map(|r| r.kind.as_deref()).collect();
    let ttl: Int64Array = rows.iter().map(|r| r.ttl_ms).collect();
    let size: UInt64Array = rows.iter().map(|r| r.size).collect();
    RecordBatch::try_new(
        keys_schema(),
        vec![
            Arc::new(key) as ArrayRef,
            Arc::new(kind),
            Arc::new(ttl),
            Arc::new(size),
        ],
    )
    .expect("columns match the schema")
}

fn str_col<'a>(b: &'a RecordBatch, name: &str) -> Option<&'a StringArray> {
    b.column_by_name(name)?.as_any().downcast_ref()
}

fn opt_str(a: Option<&StringArray>, i: usize) -> Option<String> {
    a.filter(|a| !a.is_null(i)).map(|a| a.value(i).to_owned())
}

/// Key entries of a [`keys_schema`] batch (the key browser's model).
pub fn keys_from_batch(b: &RecordBatch) -> Vec<KeyEntry> {
    let Some(key) = str_col(b, "key") else {
        return Vec::new();
    };
    let kind = str_col(b, "type");
    let ttl = b
        .column_by_name("ttl_ms")
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
    let size = b
        .column_by_name("size")
        .and_then(|c| c.as_any().downcast_ref::<UInt64Array>());
    (0..b.num_rows())
        .map(|i| KeyEntry {
            key: key.value(i).to_owned(),
            kind: opt_str(kind, i).map_or(KeyType::Other, |k| key_type(&k)),
            ttl: ttl
                .filter(|a| !a.is_null(i) && a.value(i) >= 0)
                .map(|a| Duration::from_millis(a.value(i) as u64)),
            size: size.filter(|a| !a.is_null(i)).map(|a| a.value(i)),
        })
        .collect()
}

/// One row of a key value, as built by drivers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ValueRow {
    pub member: Option<String>,
    pub field: Option<String>,
    pub value: Option<String>,
    pub score: Option<f64>,
}

/// Value rows of a key of type `kind` as a [`value_schema`] batch.
pub fn value_batch(kind: &str, rows: &[ValueRow]) -> RecordBatch {
    let k: StringArray = rows.iter().map(|_| Some(kind)).collect();
    let member: StringArray = rows.iter().map(|r| r.member.as_deref()).collect();
    let field: StringArray = rows.iter().map(|r| r.field.as_deref()).collect();
    let value: StringArray = rows.iter().map(|r| r.value.as_deref()).collect();
    let score: Float64Array = rows.iter().map(|r| r.score).collect();
    RecordBatch::try_new(
        value_schema(),
        vec![
            Arc::new(k) as ArrayRef,
            Arc::new(member),
            Arc::new(field),
            Arc::new(value),
            Arc::new(score),
        ],
    )
    .expect("columns match the schema")
}

/// Reads a key value from the result of [`method::VALUE`]. `kind` is the
/// key type when the result is empty (an empty collection).
pub fn value_from_results(results: &[Collected], kind: Option<KeyType>) -> Result<KeyValue> {
    let mut rows: Vec<(String, ValueRow)> = Vec::new();
    for r in results {
        for b in &r.batches {
            let (Some(k), m, f, v) = (
                str_col(b, "kind"),
                str_col(b, "member"),
                str_col(b, "field"),
                str_col(b, "value"),
            ) else {
                return Err(Error::Other("not a key value result".into()));
            };
            let s = b
                .column_by_name("score")
                .and_then(|c| c.as_any().downcast_ref::<Float64Array>());
            for i in 0..b.num_rows() {
                rows.push((
                    k.value(i).to_owned(),
                    ValueRow {
                        member: opt_str(m, i),
                        field: opt_str(f, i),
                        value: opt_str(v, i),
                        score: s.filter(|a| !a.is_null(i)).map(|a| a.value(i)),
                    },
                ));
            }
        }
    }
    let kind = match rows.first() {
        Some((k, _)) => key_type(k),
        None => kind.ok_or_else(|| Error::NotFound("key".into()))?,
    };
    let rows = rows.into_iter().map(|(_, r)| r);
    Ok(match kind {
        KeyType::String | KeyType::Other => {
            KeyValue::String(rows.filter_map(|r| r.value).next().unwrap_or_default())
        }
        KeyType::Json => KeyValue::Json(rows.filter_map(|r| r.value).next().unwrap_or_default()),
        KeyType::Hash => KeyValue::Hash(
            rows.map(|r| (r.member.unwrap_or_default(), r.value.unwrap_or_default()))
                .collect(),
        ),
        KeyType::List => KeyValue::List(rows.map(|r| r.value.unwrap_or_default()).collect()),
        KeyType::Set => KeyValue::Set(rows.map(|r| r.member.unwrap_or_default()).collect()),
        KeyType::ZSet => KeyValue::ZSet(
            rows.map(|r| (r.member.unwrap_or_default(), r.score.unwrap_or_default()))
                .collect(),
        ),
        KeyType::Stream => {
            let mut out: Vec<StreamEntry> = Vec::new();
            for r in rows {
                let id = r.member.unwrap_or_default();
                if out.last().is_none_or(|e| e.id != id) {
                    out.push(StreamEntry {
                        id: id.clone(),
                        fields: Vec::new(),
                    });
                }
                if let (Some(f), Some(e)) = (r.field, out.last_mut()) {
                    e.fields.push((f, r.value.unwrap_or_default()));
                }
            }
            KeyValue::Stream(out)
        }
    })
}

// ------------------------------------------------------------ command lines

/// Splits a command line into arguments like `redis-cli`: whitespace
/// separates, `"…"` supports `\n \r \t \b \a \\ \" \xHH`, `'…'` only `\'`.
/// A quoted argument must be followed by whitespace or the end.
pub fn split_args(line: &str) -> std::result::Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let Some(&first) = chars.peek() else {
            return Ok(out);
        };
        let mut arg = String::new();
        match first {
            '"' => {
                chars.next();
                let mut bytes: Vec<u8> = Vec::new();
                loop {
                    match chars.next() {
                        None => return Err("unbalanced quotes in the command".into()),
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => bytes.push(b'\n'),
                            Some('r') => bytes.push(b'\r'),
                            Some('t') => bytes.push(b'\t'),
                            Some('b') => bytes.push(8),
                            Some('a') => bytes.push(7),
                            Some('x') => {
                                let h: String = chars.by_ref().take(2).collect();
                                match u8::from_str_radix(&h, 16) {
                                    Ok(b) if h.len() == 2 => bytes.push(b),
                                    _ => {
                                        bytes.extend_from_slice(b"x");
                                        bytes.extend_from_slice(h.as_bytes());
                                    }
                                }
                            }
                            Some(c) => {
                                let mut buf = [0; 4];
                                bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                            }
                            None => return Err("unbalanced quotes in the command".into()),
                        },
                        Some(c) => {
                            let mut buf = [0; 4];
                            bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
                arg = String::from_utf8_lossy(&bytes).into_owned();
                if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                    return Err("closing quote must be followed by a space".into());
                }
            }
            '\'' => {
                chars.next();
                loop {
                    match chars.next() {
                        None => return Err("unbalanced quotes in the command".into()),
                        Some('\'') => break,
                        Some('\\') if chars.peek() == Some(&'\'') => {
                            chars.next();
                            arg.push('\'');
                        }
                        Some(c) => arg.push(c),
                    }
                }
                if chars.peek().is_some_and(|c| !c.is_whitespace()) {
                    return Err("closing quote must be followed by a space".into());
                }
            }
            _ => {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        break;
                    }
                    arg.push(c);
                    chars.next();
                }
            }
        }
        out.push(arg);
    }
}

/// Quotes an argument for a command line when needed ([`split_args`]
/// reads it back unchanged).
pub fn quote_arg(s: &str) -> String {
    let plain = !s.is_empty()
        && s.chars()
            .all(|c| !c.is_whitespace() && !c.is_control() && c != '"' && c != '\'');
    if plain {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A command line from arguments, quoted where needed.
pub fn command_line(args: &[&str]) -> String {
    args.iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Commands of a console script: one per non-empty line; lines starting
/// with `#` or `//` are comments. Ranges cover the command text.
pub fn split_commands(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with("//") {
            continue;
        }
        let lead = line.len() - line.trim_start().len();
        out.push(start + lead..start + lead + trimmed.len());
    }
    out
}

/// What a command does, for read-only enforcement and the console.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandClass {
    /// Reads only (allowed on read-only sources).
    Read,
    /// Connection state only (`SELECT`, `PING`, `CLIENT SETNAME`…).
    Session,
    /// `MULTI` / `EXEC` / `DISCARD` / `WATCH` / `UNWATCH`.
    Transaction,
    /// Changes data.
    Write,
    /// Destroys data or affects the whole server (`FLUSHALL`, `FLUSHDB`,
    /// `SHUTDOWN`, `DEBUG`, `CONFIG SET`…): refused on read-only sources and
    /// confirmed by the user elsewhere.
    Dangerous,
}

impl CommandClass {
    pub fn allowed_read_only(self) -> bool {
        matches!(self, Self::Read | Self::Session | Self::Transaction)
    }
}

/// Read-only commands (the `readonly` flag of `COMMAND INFO`), uppercase.
const READ: &[&str] = &[
    "GET",
    "MGET",
    "STRLEN",
    "GETRANGE",
    "SUBSTR",
    "LCS",
    "EXISTS",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "DUMP",
    "RANDOMKEY",
    "DBSIZE",
    "SCAN",
    "KEYS",
    "TOUCH",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSTRLEN",
    "HSCAN",
    "HRANDFIELD",
    "HTTL",
    "HPTTL",
    "HEXPIRETIME",
    "HPEXPIRETIME",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "LPOS",
    "SMEMBERS",
    "SISMEMBER",
    "SMISMEMBER",
    "SCARD",
    "SSCAN",
    "SRANDMEMBER",
    "SINTER",
    "SINTERCARD",
    "SUNION",
    "SDIFF",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZREVRANGEBYLEX",
    "ZSCORE",
    "ZMSCORE",
    "ZCARD",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZRANK",
    "ZREVRANK",
    "ZSCAN",
    "ZRANDMEMBER",
    "ZINTER",
    "ZUNION",
    "ZDIFF",
    "ZINTERCARD",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XREAD",
    "XINFO",
    "XPENDING",
    "BITCOUNT",
    "BITPOS",
    "GETBIT",
    "BITFIELD_RO",
    "PFCOUNT",
    "GEOPOS",
    "GEODIST",
    "GEOHASH",
    "GEOSEARCH",
    "GEORADIUS_RO",
    "GEORADIUSBYMEMBER_RO",
    "SORT_RO",
    "EVAL_RO",
    "EVALSHA_RO",
    "FCALL_RO",
    "OBJECT",
    "MEMORY",
    "INFO",
    "TIME",
    "LASTSAVE",
    "ROLE",
    "LOLWUT",
    "COMMAND",
    "SLOWLOG",
    "LATENCY",
    "CLUSTER",
    "JSON.GET",
    "JSON.MGET",
    "JSON.TYPE",
    "JSON.STRLEN",
    "JSON.ARRLEN",
    "JSON.ARRINDEX",
    "JSON.OBJKEYS",
    "JSON.OBJLEN",
    "JSON.RESP",
    "JSON.DEBUG",
    "FT.SEARCH",
    "FT.AGGREGATE",
    "FT.INFO",
    "FT._LIST",
    "FT.EXPLAIN",
    "FT.PROFILE",
    "FT.TAGVALS",
    "FT.SPELLCHECK",
    "FT.SYNDUMP",
    "TS.GET",
    "TS.MGET",
    "TS.RANGE",
    "TS.REVRANGE",
    "TS.MRANGE",
    "TS.MREVRANGE",
    "TS.INFO",
    "TS.QUERYINDEX",
    "BF.EXISTS",
    "BF.MEXISTS",
    "BF.INFO",
    "CF.EXISTS",
    "CF.COUNT",
    "CF.INFO",
    "CMS.QUERY",
    "CMS.INFO",
    "TOPK.QUERY",
    "TOPK.LIST",
    "TOPK.INFO",
    "TDIGEST.INFO",
    "TDIGEST.QUANTILE",
    "TDIGEST.CDF",
    "TDIGEST.MIN",
    "TDIGEST.MAX",
    "SENTINEL",
    "PUBSUB",
    "MODULE",
    "ACL",
];

const SESSION: &[&str] = &[
    "PING",
    "ECHO",
    "SELECT",
    "AUTH",
    "HELLO",
    "QUIT",
    "RESET",
    "READONLY",
    "READWRITE",
    "CLIENT",
];

const TRANSACTION: &[&str] = &["MULTI", "EXEC", "DISCARD", "WATCH", "UNWATCH"];

const DANGEROUS: &[&str] = &[
    "FLUSHALL",
    "FLUSHDB",
    "SHUTDOWN",
    "DEBUG",
    "SWAPDB",
    "MIGRATE",
    "REPLICAOF",
    "SLAVEOF",
    "FAILOVER",
    "BGREWRITEAOF",
    "BGSAVE",
    "SAVE",
    "SYNC",
    "PSYNC",
    "MONITOR",
];

/// Subcommands of read-mostly containers that change the server.
fn dangerous_subcommand(cmd: &str, sub: &str) -> bool {
    match cmd {
        "CONFIG" => matches!(sub, "SET" | "RESETSTAT" | "REWRITE"),
        "CLUSTER" => !matches!(
            sub,
            "INFO"
                | "NODES"
                | "SLOTS"
                | "SHARDS"
                | "KEYSLOT"
                | "COUNTKEYSINSLOT"
                | "GETKEYSINSLOT"
                | "MYID"
                | "MYSHARDID"
                | "LINKS"
                | "REPLICAS"
                | "SLAVES"
                | "HELP"
        ),
        "ACL" => !matches!(
            sub,
            "WHOAMI" | "LIST" | "USERS" | "GETUSER" | "CAT" | "LOG" | "HELP" | "DRYRUN"
        ),
        "MODULE" => !matches!(sub, "LIST" | "HELP"),
        "SCRIPT" | "FUNCTION" => matches!(sub, "FLUSH" | "KILL" | "DELETE" | "RESTORE"),
        "CLIENT" => matches!(sub, "KILL" | "PAUSE"),
        "SLOWLOG" | "LATENCY" => matches!(sub, "RESET"),
        "SENTINEL" => !matches!(
            sub,
            "GET-MASTER-ADDR-BY-NAME"
                | "MASTERS"
                | "MASTER"
                | "PRIMARY"
                | "PRIMARIES"
                | "REPLICAS"
                | "SLAVES"
                | "SENTINELS"
                | "CKQUORUM"
                | "INFO-CACHE"
                | "MYID"
                | "HELP"
        ),
        "MEMORY" => matches!(sub, "PURGE"),
        _ => false,
    }
}

/// Classifies a command by its name (and subcommand).
pub fn classify(args: &[String]) -> CommandClass {
    let Some(cmd) = args.first().map(|c| c.to_ascii_uppercase()) else {
        return CommandClass::Session;
    };
    let sub = args
        .get(1)
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    if DANGEROUS.contains(&cmd.as_str()) || dangerous_subcommand(&cmd, &sub) {
        return CommandClass::Dangerous;
    }
    if cmd == "CONFIG" {
        return CommandClass::Read;
    }
    if TRANSACTION.contains(&cmd.as_str()) {
        return CommandClass::Transaction;
    }
    if SESSION.contains(&cmd.as_str()) {
        return CommandClass::Session;
    }
    if READ.contains(&cmd.as_str()) {
        // `XREAD … GROUP` / `XREADGROUP` acknowledge entries: writes.
        return CommandClass::Read;
    }
    CommandClass::Write
}

/// Commands that block the connection until data arrives (cancelled with
/// `CLIENT UNBLOCK`).
pub fn is_blocking(args: &[String]) -> bool {
    let Some(cmd) = args.first().map(|c| c.to_ascii_uppercase()) else {
        return false;
    };
    matches!(
        cmd.as_str(),
        "BLPOP"
            | "BRPOP"
            | "BLMOVE"
            | "BRPOPLPUSH"
            | "BLMPOP"
            | "BZPOPMIN"
            | "BZPOPMAX"
            | "BZMPOP"
            | "WAIT"
            | "WAITAOF"
    ) || (matches!(cmd.as_str(), "XREAD" | "XREADGROUP")
        && args.iter().any(|a| a.eq_ignore_ascii_case("BLOCK")))
}

/// The key browser pattern of a console command that lists keys (`SCAN 0
/// MATCH p COUNT n`, `KEYS p`), with the `COUNT` hint. Other commands give
/// `None`.
pub fn key_listing(args: &[String]) -> Option<(String, Option<u64>, Option<String>)> {
    let cmd = args.first()?.to_ascii_uppercase();
    match cmd.as_str() {
        "KEYS" if args.len() == 2 => Some((args[1].clone(), None, None)),
        "SCAN" if args.len() >= 2 && args[1] == "0" => {
            let mut pattern = "*".to_owned();
            let mut count = None;
            let mut kind = None;
            let mut i = 2;
            while i + 1 < args.len() {
                match args[i].to_ascii_uppercase().as_str() {
                    "MATCH" => pattern = args[i + 1].clone(),
                    "COUNT" => count = args[i + 1].parse().ok(),
                    "TYPE" => kind = Some(args[i + 1].clone()),
                    _ => return None,
                }
                i += 2;
            }
            (i == args.len()).then_some((pattern, count, kind))
        }
        _ => None,
    }
}

/// Database index of a container name: `db3`, `3`.
pub fn database_index(name: &str) -> Option<i64> {
    name.strip_prefix("db").unwrap_or(name).parse().ok()
}

/// Redis-style glob (`*`, `?`, `[abc]`, `[a-z]`, `[^a]`, `\x`).
pub fn glob_match(pattern: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some('*') => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            Some('?') => !s.is_empty() && go(&p[1..], &s[1..]),
            Some('[') => {
                let Some(&c) = s.first() else { return false };
                let mut i = 1;
                let negate = p.get(1) == Some(&'^');
                if negate {
                    i += 1;
                }
                let mut matched = false;
                while i < p.len() && p[i] != ']' {
                    if p[i] == '\\' && i + 1 < p.len() {
                        matched |= p[i + 1] == c;
                        i += 2;
                    } else if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
                        matched |= p[i] <= c && c <= p[i + 2];
                        i += 3;
                    } else {
                        matched |= p[i] == c;
                        i += 1;
                    }
                }
                matched != negate && go(&p[(i + 1).min(p.len())..], &s[1..])
            }
            Some('\\') if p.len() > 1 => s.first() == Some(&p[1]) && go(&p[2..], &s[1..]),
            Some(c) => s.first() == Some(c) && go(&p[1..], &s[1..]),
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = s.chars().collect();
    go(&p, &s)
}

/// Escapes glob characters so that a literal prefix can start a pattern.
pub fn glob_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_like_redis_cli() {
        let a = split_args(r#"SET "my key" 'it\'s' "a\x41\n" plain"#).unwrap();
        assert_eq!(a, vec!["SET", "my key", "it's", "aA\n", "plain"]);
        assert!(split_args(r#"SET "x"y"#).is_err());
        assert!(split_args(r#"SET "x"#).is_err());
        assert_eq!(split_args("  ").unwrap(), Vec::<String>::new());
        for s in [
            "a b",
            "",
            "q\"uote",
            "back\\slash",
            "tab\there",
            "żółć",
            "'",
        ] {
            let line = command_line(&["SET", s]);
            assert_eq!(split_args(&line).unwrap()[1], s, "{line}");
        }
    }

    #[test]
    fn script_lines() {
        let t = "GET a\n\n  # comment\n  HGETALL h  \r\n// x\nPING";
        let r: Vec<&str> = split_commands(t).into_iter().map(|r| &t[r]).collect();
        assert_eq!(r, vec!["GET a", "HGETALL h", "PING"]);
    }

    #[test]
    fn classification() {
        let c = |s: &str| classify(&split_args(s).unwrap());
        assert_eq!(c("get a"), CommandClass::Read);
        assert_eq!(c("SET a 1"), CommandClass::Write);
        assert_eq!(c("FLUSHALL"), CommandClass::Dangerous);
        assert_eq!(c("config set maxmemory 1"), CommandClass::Dangerous);
        assert_eq!(c("CONFIG GET *"), CommandClass::Read);
        assert_eq!(c("MULTI"), CommandClass::Transaction);
        assert_eq!(c("select 2"), CommandClass::Session);
        assert_eq!(c("CLIENT KILL ID 3"), CommandClass::Dangerous);
        assert_eq!(c("cluster nodes"), CommandClass::Read);
        assert_eq!(c("cluster reset"), CommandClass::Dangerous);
        assert_eq!(c("JSON.GET k $"), CommandClass::Read);
        assert_eq!(c("JSON.SET k $ 1"), CommandClass::Write);
        assert!(!c("DEBUG SLEEP 1").allowed_read_only());
        assert!(is_blocking(&split_args("BLPOP q 0").unwrap()));
        assert!(is_blocking(
            &split_args("XREAD BLOCK 0 STREAMS s $").unwrap()
        ));
        assert!(!is_blocking(&split_args("XREAD STREAMS s 0").unwrap()));
    }

    #[test]
    fn key_listings() {
        let k = |s: &str| key_listing(&split_args(s).unwrap());
        assert_eq!(
            k("SCAN 0 MATCH app:* COUNT 500"),
            Some(("app:*".into(), Some(500), None))
        );
        assert_eq!(k("scan 0"), Some(("*".into(), None, None)));
        assert_eq!(k("KEYS user:*"), Some(("user:*".into(), None, None)));
        assert_eq!(k("SCAN 17 MATCH x"), None);
        assert_eq!(k("SCAN 0 MATCH"), None);
        assert_eq!(k("GET x"), None);
    }

    #[test]
    fn globs() {
        assert!(glob_match("app:*", "app:user:1"));
        assert!(glob_match("h?llo", "hello"));
        assert!(glob_match("h[ae]llo", "hallo"));
        assert!(!glob_match("h[^e]llo", "hello"));
        assert!(glob_match("h[a-c]llo", "hbllo"));
        assert!(glob_match(&format!("{}*", glob_escape("a*b")), "a*bc"));
        assert!(!glob_match(&format!("{}*", glob_escape("a*b")), "axbc"));
        assert_eq!(database_index("db3"), Some(3));
        assert_eq!(database_index("12"), Some(12));
    }

    #[test]
    fn values_round_trip() {
        let rows = vec![
            ValueRow {
                member: Some("1-0".into()),
                field: Some("a".into()),
                value: Some("1".into()),
                score: None,
            },
            ValueRow {
                member: Some("1-0".into()),
                field: Some("b".into()),
                value: Some("2".into()),
                score: None,
            },
            ValueRow {
                member: Some("2-0".into()),
                field: Some("a".into()),
                value: Some("3".into()),
                score: None,
            },
        ];
        let b = value_batch("stream", &rows);
        let c = Collected {
            schema: b.schema(),
            batches: vec![b],
        };
        let KeyValue::Stream(s) = value_from_results(&[c], None).unwrap() else {
            panic!("stream expected");
        };
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0].fields,
            vec![("a".into(), "1".into()), ("b".into(), "2".into())]
        );
        let empty = value_from_results(&[], Some(KeyType::Set)).unwrap();
        assert_eq!(empty, KeyValue::Set(vec![]));
        let keys = keys_batch(&[KeyRow {
            key: "a".into(),
            kind: Some("hash".into()),
            ttl_ms: Some(1500),
            size: Some(10),
        }]);
        let e = keys_from_batch(&keys);
        assert_eq!(e[0].kind, KeyType::Hash);
        assert_eq!(e[0].ttl, Some(Duration::from_millis(1500)));
    }
}
