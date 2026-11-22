//! Prompts. The system prompt (instructions naming the product and the
//! language, then the schema) depends only on the data source and the
//! settings, so providers that cache prompts reuse it across questions; the
//! task and the user's words go in the last user turn.

use ferrodb_api::{AssistantRequest, AssistantTask, ChatRole, DataModel, QueryLanguage};

use super::{context::SourceContext, providers::Chat};

/// How a language is named and fenced.
pub(crate) struct LanguageProfile {
    /// E.g. `PostgreSQL SQL`, `MongoDB shell (mongosh) syntax`.
    pub name: String,
    /// Info string of the code fence the model is asked to use.
    pub fence: &'static str,
    /// Fence tags accepted when extracting queries (besides an empty one).
    pub aliases: &'static [&'static str],
    /// Language-specific rules.
    pub notes: &'static str,
}

const SQL_TAGS: &[&str] = &["sql", "plsql", "tsql", "pgsql", "postgresql", "mysql"];

fn sql_dialect(dialect: &str) -> (&'static str, &'static str) {
    match dialect {
        "postgresql" => (
            "PostgreSQL",
            "Use PostgreSQL syntax (LIMIT/OFFSET, double-quoted identifiers, ILIKE, date_trunc, FILTER, window functions).",
        ),
        "greenplum" => (
            "Greenplum (PostgreSQL-based)",
            "Use PostgreSQL syntax as supported by Greenplum.",
        ),
        "cockroachdb" => (
            "CockroachDB (PostgreSQL-compatible)",
            "Use PostgreSQL syntax as supported by CockroachDB.",
        ),
        "redshift" => (
            "Amazon Redshift",
            "Use Redshift SQL (PostgreSQL 8-based; no PostgreSQL features Redshift lacks).",
        ),
        "mysql" => (
            "MySQL",
            "Use MySQL 8 syntax (LIMIT, backtick-quoted identifiers).",
        ),
        "mariadb" => (
            "MariaDB",
            "Use MariaDB syntax (LIMIT, backtick-quoted identifiers).",
        ),
        "sqlite" => (
            "SQLite",
            "Use SQLite syntax (LIMIT, few built-in functions, dates stored as text or numbers).",
        ),
        "tsql" => (
            "T-SQL (Microsoft SQL Server)",
            "Use T-SQL: TOP or OFFSET … FETCH instead of LIMIT, square-bracket identifiers, GETDATE().",
        ),
        "oracle" => (
            "Oracle SQL and PL/SQL",
            "Use Oracle syntax: FETCH FIRST n ROWS ONLY instead of LIMIT, double-quoted identifiers, SYSDATE, no AS before table aliases.",
        ),
        "db2" => ("IBM Db2 SQL", "Use Db2 syntax (FETCH FIRST n ROWS ONLY)."),
        "sybase" => (
            "Sybase ASE Transact-SQL",
            "Use Sybase ASE Transact-SQL (TOP, no LIMIT).",
        ),
        "h2" => ("H2 SQL", "Use H2 syntax."),
        "derby" => (
            "Apache Derby SQL",
            "Use Derby syntax (FETCH FIRST n ROWS ONLY).",
        ),
        "hsqldb" => ("HyperSQL (HSQLDB) SQL", "Use HyperSQL syntax."),
        "exasol" => ("Exasol SQL", "Use Exasol syntax."),
        "vertica" => ("Vertica SQL", "Use Vertica syntax."),
        "hiveql" => (
            "Apache Hive HiveQL",
            "Use HiveQL; avoid features Hive lacks (no UPDATE on non-ACID tables).",
        ),
        "snowflake" => (
            "Snowflake SQL",
            "Use Snowflake syntax (QUALIFY, FLATTEN for semi-structured data, ILIKE).",
        ),
        "googlesql" => (
            "Google BigQuery GoogleSQL",
            "Use GoogleSQL: backtick-quoted `dataset.table` names, SAFE_ functions, no ILIKE; avoid SELECT * on large tables (cost).",
        ),
        "clickhouse" => (
            "ClickHouse SQL",
            "Use ClickHouse syntax and functions (toStartOfDay, countIf, arrayJoin; LIMIT BY).",
        ),
        _ => ("SQL", "Use standard SQL."),
    }
}

pub(crate) fn language_profile(lang: &QueryLanguage) -> LanguageProfile {
    match lang {
        QueryLanguage::Sql(d) => {
            let (name, notes) = sql_dialect(d.as_str());
            let name = if name == "SQL" && d.as_str() != "generic" {
                format!("SQL ({d})")
            } else {
                name.to_owned()
            };
            LanguageProfile {
                name,
                fence: "sql",
                aliases: SQL_TAGS,
                notes,
            }
        }
        QueryLanguage::Mql => LanguageProfile {
            name: "MongoDB shell (mongosh) syntax".into(),
            fence: "javascript",
            aliases: &["javascript", "js", "mongodb", "mongo", "mongosh"],
            notes: "Write shell statements such as db.orders.find({...}, {...}).sort({...}).limit(n) or db.orders.aggregate([...]); use db.getCollection(\"name\") for names that are not valid identifiers. No SQL.",
        },
        QueryLanguage::Cql => LanguageProfile {
            name: "Cassandra Query Language (CQL)".into(),
            fence: "cql",
            aliases: &["cql", "sql"],
            notes: "CQL has no joins and no subqueries. WHERE must restrict the full partition key (equality or IN), then clustering columns in order; filtering on other columns needs a secondary index or ALLOW FILTERING, which you must point out as slow. Prefer a query table modelled for the access pattern.",
        },
        QueryLanguage::SqlPlusPlus => LanguageProfile {
            name: "Couchbase SQL++ (N1QL)".into(),
            fence: "sql",
            aliases: &["sql", "sqlpp", "n1ql", "sql++"],
            notes: "Use SQL++: keyspaces as `bucket`.`scope`.`collection` in backticks, UNNEST for arrays, META().id for document keys; queries need a suitable index.",
        },
        QueryLanguage::PartiQl => LanguageProfile {
            name: "Amazon DynamoDB PartiQL".into(),
            fence: "sql",
            aliases: &["sql", "partiql"],
            notes: "DynamoDB PartiQL has no joins, aggregates or ORDER BY across partitions; efficient queries restrict the partition key with = or IN, otherwise they scan the whole table (say so). Quote table names with double quotes and strings with single quotes.",
        },
        QueryLanguage::RedisCommands => LanguageProfile {
            name: "Redis commands".into(),
            fence: "redis",
            aliases: &["redis", "bash", "shell", "text"],
            notes: "Write Redis commands, one per line (e.g. HGETALL user:1, ZRANGE board 0 9 REV WITHSCORES). Never use KEYS on large databases: use SCAN with MATCH and COUNT.",
        },
    }
}

fn model_name(m: DataModel) -> &'static str {
    match m {
        DataModel::Tabular => "tables and columns",
        DataModel::Document => "documents in collections",
        DataModel::KeyValue => "keys with typed values",
    }
}

/// The stable system blocks: instructions, then the schema.
pub(crate) fn system_blocks(ctx: &SourceContext) -> Vec<String> {
    let profile = language_profile(&ctx.language);
    let mut s = String::from(
        "You are the query assistant of FerroDb, a database IDE. You help the user write, explain, fix and optimize queries for one data source. Your answers appear next to a query console; nothing you write is run automatically, the user decides.\n\n",
    );
    match &ctx.product {
        Some(product) => {
            s.push_str(&format!("Database: {product}"));
            if let Some(name) = &ctx.source_name {
                s.push_str(&format!(" (data source \"{name}\")"));
            }
            s.push_str(".\n");
            if let Some(m) = ctx.data_model {
                s.push_str(&format!("Data model: {}.\n", model_name(m)));
            }
        }
        None => s.push_str(
            "No data source is selected; assume standard SQL unless the user names a database.\n",
        ),
    }
    s.push_str(&format!(
        "Query language: {}. {}\n",
        profile.name, profile.notes
    ));
    if ctx.product.is_some() && !ctx.joins && ctx.language != QueryLanguage::Cql {
        s.push_str("This database does not support joins.\n");
    }
    s.push_str(&format!(
        "\nRules:\n\
         - Put the query you propose in one fenced code block tagged {fence} (```{fence}), followed by a short explanation. Use several blocks only when several statements must run separately.\n\
         - Use only objects (tables, collections, columns, fields, keys) from the schema below. If something the user asks for is not there, say so and do not invent names.\n\
         - Prefer read-only queries. When a query changes data or schema, say so explicitly before the block and warn about what it affects; never hide a change inside a read-only answer.\n\
         - Limit result sizes when the user does not ask for everything.\n\
         - Keep explanations short and concrete.\n",
        fence = profile.fence
    ));

    let mut schema = String::new();
    match (&ctx.schema_text, ctx.shared) {
        (_, false) => schema.push_str(
            "The user does not share the schema (privacy setting). Use the names the user gives; when you need table, collection or column names, ask for them instead of guessing.",
        ),
        (Some(text), true) => {
            schema.push_str("Schema (most relevant objects first");
            if let Some(cs) = &ctx.console_schema {
                schema.push_str(&format!("; unqualified names resolve to `{cs}`"));
            }
            schema.push_str("):\n");
            schema.push_str(text);
        }
        (None, true) => schema.push_str(
            "The schema of this data source is not available (not loaded yet). Ask for the names you need instead of guessing.",
        ),
    }
    vec![s, schema]
}

fn fenced(tag: &str, text: &str) -> String {
    let text = text.trim_end();
    let fence = if text.contains("```") { "````" } else { "```" };
    format!("{fence}{tag}\n{text}\n{fence}")
}

/// The new user turn: the task with its query and the user's words.
pub(crate) fn user_turn(req: &AssistantRequest, ctx: &SourceContext) -> String {
    let tag = language_profile(&ctx.language).fence;
    let prompt = req.prompt.trim();
    let query = req
        .context
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let mut s = String::new();
    let with_note = |s: &mut String, lead: &str| {
        if !prompt.is_empty() {
            s.push_str(&format!("\n\n{lead}{prompt}"));
        }
    };
    match &req.task {
        AssistantTask::Generate => {
            s.push_str("Write a query for this request:\n");
            s.push_str(if prompt.is_empty() {
                "(no description)"
            } else {
                prompt
            });
            if let Some(q) = query {
                s.push_str(&format!(
                    "\n\nThe console currently contains (for reference, may be unrelated):\n{}",
                    fenced(tag, q)
                ));
            }
        }
        AssistantTask::Explain => {
            s.push_str("Explain what this query does: what it returns, how the parts work together, and anything surprising or risky.\n");
            s.push_str(&fenced(tag, query.unwrap_or("")));
            with_note(&mut s, "Question: ");
        }
        AssistantTask::Fix { error } => {
            s.push_str("This query failed. Find the cause and give the corrected query.\n");
            s.push_str(&fenced(tag, query.unwrap_or("")));
            s.push_str(&format!("\n\nError:\n{}", fenced("", error.trim())));
            with_note(&mut s, "Note: ");
        }
        AssistantTask::Optimize => {
            s.push_str("Suggest a faster equivalent of this query with the same results, and indexes that would help (as separate statements, marked as schema changes). Explain why it is faster.\n");
            s.push_str(&fenced(tag, query.unwrap_or("")));
            with_note(&mut s, "Note: ");
        }
        AssistantTask::Chat => {
            s.push_str(if prompt.is_empty() {
                "(no question)"
            } else {
                prompt
            });
            if let Some(q) = query {
                s.push_str(&format!("\n\nQuery in the console:\n{}", fenced(tag, q)));
            }
        }
    }
    s
}

/// The whole conversation for a provider. History is normalised: empty
/// turns dropped, consecutive turns of one role merged, a leading assistant
/// turn dropped (providers want user first, alternating).
pub(crate) fn chat(
    req: &AssistantRequest,
    ctx: &SourceContext,
    model: String,
    max_tokens: u32,
) -> Chat {
    let mut messages: Vec<(ChatRole, String)> = Vec::new();
    let turns = req
        .history
        .iter()
        .map(|t| (t.role, t.text.clone()))
        .chain(std::iter::once((ChatRole::User, user_turn(req, ctx))));
    for (role, text) in turns {
        if text.trim().is_empty() || (messages.is_empty() && role == ChatRole::Assistant) {
            continue;
        }
        match messages.last_mut() {
            Some((r, t)) if *r == role => {
                t.push_str("\n\n");
                t.push_str(&text);
            }
            _ => messages.push((role, text)),
        }
    }
    Chat {
        model,
        max_tokens: max_tokens.max(1),
        system: system_blocks(ctx),
        messages,
    }
}
