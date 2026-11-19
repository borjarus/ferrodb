//! Schema tools on the live catalog (plan, phase 7): ER model, schema
//! comparison with a migration script, the table editor (design → `ALTER`)
//! and running DDL scripts. The logic is in `ferrodb_meta::diff`; this
//! module loads the catalogs and runs the statements.

use std::time::{Instant, SystemTime};

use ferrodb_api::{
    CompareOptions, DesignScript, ErModel, Error, NodeRef, Result, SchemaComparison, TableDesign,
};
use ferrodb_drivers::{CancellationToken, sink::collect};
use ferrodb_meta::{
    Content, Dialect,
    ddl::Object,
    diff::{self, Side},
    tree::Found,
};
use ferrodb_storage::{LogEntry, LogSource};

use crate::Inner;

/// The schema / database node of `node` (itself, or the container of a
/// table).
fn container_of(node: &NodeRef) -> Result<NodeRef> {
    if node.path.is_empty() {
        return Err(Error::Unsupported(
            "choose a schema or database, not the data source".into(),
        ));
    }
    Ok(NodeRef {
        data_source: node.data_source.clone(),
        path: node.path[..1].to_vec(),
    })
}

impl Inner {
    /// Dialect, container name and loaded content of a container node.
    pub(crate) async fn container_content(
        &self,
        node: &NodeRef,
    ) -> Result<(Dialect, String, Content)> {
        let c = container_of(node)?;
        let (_, d) = self.source_dialect(&c.data_source)?;
        let d = d.ok_or_else(|| Error::Unsupported("metadata of this data source".into()))?;
        let name = c.path[0].1.clone();
        let cat = self.ensure_content(&c.data_source, d, &name).await?;
        let content = cat
            .container(&name)
            .and_then(|x| x.content.clone())
            .ok_or_else(|| Error::NotFound(name.clone()))?;
        Ok((d, name, content))
    }

    pub(crate) async fn er_model_of(&self, node: NodeRef) -> Result<ErModel> {
        let (d, name, content) = self.container_content(&node).await?;
        if d == Dialect::Mongo {
            return Err(Error::Unsupported(
                "ER diagrams of document databases".into(),
            ));
        }
        Ok(diff::er_model(&name, &content))
    }

    pub(crate) async fn compare(
        &self,
        source: NodeRef,
        target: NodeRef,
        opts: CompareOptions,
    ) -> Result<SchemaComparison> {
        let (sd, sname, scontent) = self.container_content(&source).await?;
        let (td, tname, tcontent) = self.container_content(&target).await?;
        if sd == Dialect::Mongo || td == Dialect::Mongo {
            return Err(Error::Unsupported(
                "schema comparison of document databases".into(),
            ));
        }
        let r = diff::compare(
            Side {
                dialect: sd,
                container: &sname,
                content: &scontent,
            },
            Side {
                dialect: td,
                container: &tname,
                content: &tcontent,
            },
            &opts,
        );
        Ok(SchemaComparison {
            source: container_of(&source)?,
            target: container_of(&target)?,
            items: r.items,
            script: r.script,
            warnings: r.warnings,
        })
    }

    pub(crate) async fn design_of(&self, table: NodeRef) -> Result<TableDesign> {
        let (_, d) = self.source_dialect(&table.data_source)?;
        if d == Some(Dialect::Mongo) {
            return Err(Error::Unsupported("the table editor on collections".into()));
        }
        self.with_found(&table, |_, _, found| match found {
            Found::Object(Object::Table(t)) if !t.is_view() => Ok(diff::table_to_design(t)),
            _ => Err(Error::Unsupported("only tables can be edited".into())),
        })
        .await
    }

    pub(crate) async fn script_of(
        &self,
        container: NodeRef,
        design: TableDesign,
    ) -> Result<DesignScript> {
        let (d, name, content) = self.container_content(&container).await?;
        if d == Dialect::Mongo {
            return Err(Error::Unsupported("the table editor on collections".into()));
        }
        diff::design_script(d, &name, &content, &design).map_err(Error::Other)
    }

    /// Runs DDL in the container: one transaction where DDL is
    /// transactional (PostgreSQL, SQLite, DuckDB), statement by statement
    /// otherwise; then refreshes the container's metadata.
    pub(crate) async fn run_ddl(
        &self,
        container: NodeRef,
        statements: Vec<String>,
    ) -> Result<usize> {
        let c = container_of(&container)?;
        let ds = c.data_source.clone();
        let cfg = self.source_config(&ds)?;
        if cfg.read_only {
            return Err(Error::ReadOnly);
        }
        let driver = self.driver_for(&cfg)?;
        let transactional = driver.variant().quirks().transactional_ddl;
        let (_, d) = self.source_dialect(&ds)?;
        if d == Some(Dialect::Mongo) {
            return Err(Error::Unsupported("DDL scripts on MongoDB".into()));
        }
        let statements: Vec<String> = statements
            .into_iter()
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .collect();
        let mut conn = self.checkout(&ds).await?;
        // Unqualified names in the script mean the chosen schema.
        if let Some(name) = c.name()
            && d != Some(Dialect::Sqlite)
        {
            let _ = conn.set_schema(name).await;
        }
        if transactional {
            conn.begin().await?;
        }
        let mut done = 0;
        let mut failure = None;
        for st in &statements {
            let started = (SystemTime::now(), Instant::now());
            let r = collect(conn.as_mut(), st.clone(), CancellationToken::new()).await;
            let entry = LogEntry {
                data_source: ds.clone(),
                session: 0,
                source: LogSource::User,
                text: st.clone(),
                started_at: started.0,
                elapsed: started.1.elapsed(),
                rows: None,
                error: r.as_ref().err().map(|e| e.to_string()),
            };
            let storage = self.storage.clone();
            self.handle().spawn_blocking(move || {
                let _ = storage.log(&entry);
            });
            match r {
                Ok(_) => done += 1,
                Err(e) => {
                    failure = Some(match e {
                        Error::Query(mut q) => {
                            q.message = format!("statement {} failed: {}", done + 1, q.message);
                            Error::Query(q)
                        }
                        other => other,
                    });
                    break;
                }
            }
        }
        let result = match failure {
            Some(e) => {
                if transactional {
                    let _ = conn.rollback().await;
                }
                Err(e)
            }
            None => {
                if transactional {
                    conn.commit().await?;
                }
                Ok(done)
            }
        };
        self.checkin(&ds, conn);
        // Partly applied (non-transactional DDL) changes are real too.
        if done > 0 {
            let _ = self
                .refresh_scope(&ds, ferrodb_api::RefreshScope::Node(c.clone()))
                .await;
        }
        result
    }
}
