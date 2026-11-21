//! Redis "introspection": databases are the containers (`INFO keyspace`,
//! `CONFIG GET databases`, through the driver's
//! [`method::DATABASES`](ferrodb_drivers::kv::method::DATABASES) call); keys
//! are not cached (there may be millions and they change all the time), so a
//! container's content is empty and the explorer lists key prefixes live
//! with [`prefix_children`], a capped `SCAN`.

use std::collections::BTreeMap;

use ferrodb_api::{KeyType, Result};
use ferrodb_drivers::{
    BoxFuture, CancellationToken, Connection, Request,
    kv::{self, method},
    sink::collect_request,
};

use crate::{Introspector, model::*, now, rows::rows};

pub struct RedisIntrospector;

/// Keys examined per explorer expansion.
pub const SCAN_LIMIT: usize = 20_000;
/// Keys listed directly under one node (prefixes are always listed).
pub const KEY_LIMIT: usize = 500;

/// A child of a database or key prefix node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyNode {
    /// A prefix segment with the number of keys seen under it (`more` =
    /// the scan stopped at its limit, there may be more).
    Prefix {
        name: String,
        keys: usize,
        more: bool,
    },
    /// A key (last segment) with its type.
    Key { name: String, kind: KeyType },
}

/// Groups `keys` below `prefix` (segments joined by `sep`, without the
/// trailing separator; empty = database level) by their next segment.
pub fn group(
    keys: &[(String, KeyType)],
    prefix: &str,
    sep: &str,
    truncated: bool,
    key_limit: usize,
) -> Vec<KeyNode> {
    let start = if prefix.is_empty() {
        String::new()
    } else {
        format!("{prefix}{sep}")
    };
    let mut prefixes: BTreeMap<String, usize> = BTreeMap::new();
    let mut leaves: Vec<KeyNode> = Vec::new();
    for (k, kind) in keys {
        let Some(rest) = k.strip_prefix(&start) else {
            continue;
        };
        match rest.split_once(sep) {
            Some((seg, _)) => *prefixes.entry(seg.to_owned()).or_default() += 1,
            None if leaves.len() < key_limit => leaves.push(KeyNode::Key {
                name: rest.to_owned(),
                kind: *kind,
            }),
            None => {}
        }
    }
    leaves.sort_by(|a, b| match (a, b) {
        (KeyNode::Key { name: x, .. }, KeyNode::Key { name: y, .. }) => x.cmp(y),
        _ => std::cmp::Ordering::Equal,
    });
    prefixes
        .into_iter()
        .map(|(name, keys)| KeyNode::Prefix {
            name,
            keys,
            more: truncated,
        })
        .chain(leaves)
        .collect()
}

/// Children of a database (`prefix` empty) or of a key prefix: one capped
/// `SCAN MATCH prefix:*` on `database`.
pub async fn prefix_children(
    conn: &mut dyn Connection,
    database: &str,
    prefix: &str,
    sep: &str,
) -> Result<Vec<KeyNode>> {
    conn.set_schema(database).await?;
    let pattern = if prefix.is_empty() {
        "*".to_owned()
    } else {
        format!("{}*", kv::glob_escape(&format!("{prefix}{sep}")))
    };
    let limit = SCAN_LIMIT.to_string();
    let out = collect_request(
        conn,
        Request::native(
            method::SCAN,
            &[("pattern", &pattern), ("count", "1000"), ("limit", &limit)],
        ),
        CancellationToken::new(),
    )
    .await?;
    let keys: Vec<(String, KeyType)> = out
        .results
        .iter()
        .flat_map(|r| r.batches.iter())
        .flat_map(kv::keys_from_batch)
        .map(|e| (e.key, e.kind))
        .collect();
    Ok(group(&keys, prefix, sep, out.summary.truncated, KEY_LIMIT))
}

impl Introspector for RedisIntrospector {
    fn dialect(&self) -> Dialect {
        Dialect::Redis
    }

    fn containers<'a>(
        &'a self,
        conn: &'a mut dyn Connection,
    ) -> BoxFuture<'a, Result<Vec<Container>>> {
        Box::pin(async move {
            let out = collect_request(
                conn,
                Request::native(method::DATABASES, &[]),
                CancellationToken::new(),
            )
            .await?;
            Ok(rows(&out.results)
                .iter()
                .filter_map(|r| {
                    let keys = r.int("keys").unwrap_or(0);
                    Some(Container {
                        name: r.text("database")?,
                        comment: Some(format!("{keys} keys")),
                        content: None,
                    })
                })
                .collect())
        })
    }

    fn content<'a>(
        &'a self,
        _conn: &'a mut dyn Connection,
        _container: &'a str,
        _only: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Content>> {
        Box::pin(async move {
            Ok(Content {
                refreshed_at: now(),
                ..Default::default()
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_by_segment() {
        let keys: Vec<(String, KeyType)> = [
            ("app:user:1", KeyType::Hash),
            ("app:user:2", KeyType::Hash),
            ("app:session:9", KeyType::String),
            ("app:flag", KeyType::String),
            ("other", KeyType::Set),
        ]
        .iter()
        .map(|(k, t)| ((*k).to_owned(), *t))
        .collect();
        let top = group(&keys, "", ":", false, 500);
        assert_eq!(
            top,
            vec![
                KeyNode::Prefix {
                    name: "app".into(),
                    keys: 4,
                    more: false
                },
                KeyNode::Key {
                    name: "other".into(),
                    kind: KeyType::Set
                }
            ]
        );
        let app = group(&keys, "app", ":", true, 500);
        assert_eq!(app.len(), 3);
        assert_eq!(
            app[0],
            KeyNode::Prefix {
                name: "session".into(),
                keys: 1,
                more: true
            }
        );
        assert_eq!(
            app[2],
            KeyNode::Key {
                name: "flag".into(),
                kind: KeyType::String
            }
        );
        assert_eq!(group(&keys, "app", ":", false, 0).len(), 2);
    }
}
