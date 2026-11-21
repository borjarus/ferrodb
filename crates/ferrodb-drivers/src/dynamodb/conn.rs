//! Connecting (AWS configuration chain, rustls + ring) and running PartiQL
//! statements; the [`Connection`] implementation.

use std::{collections::HashMap, sync::Arc, time::Duration};

use aws_config::{BehaviorVersion, Region, timeout::TimeoutConfig};
use aws_sdk_dynamodb::{
    Client,
    config::Credentials,
    error::{DisplayErrorContext, ProvideErrorMetadata, SdkError},
    types::{AttributeValue, ConsumedCapacity, ReturnConsumedCapacity, TableDescription},
};
use aws_smithy_http_client::tls::{Provider, rustls_provider::CryptoMode};
use bson::{Document, RawDocumentBuf};
use ferrodb_api::{Error, NoticeLevel, QueryError, Result};

use super::{
    ops,
    partiql::{self, Kind},
    value,
};
use crate::{
    BoxFuture, CancellationToken, Connection, ExecSummary, RemoteCancel, Request, ResolvedConfig,
    ResultSink, SinkEvent,
};

/// Key attributes of a table and of its indexes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableKeys {
    pub partition: String,
    pub sort: Option<String>,
    /// (index, partition key, sort key)
    pub indexes: Vec<(String, String, Option<String>)>,
}

impl TableKeys {
    pub fn from_description(t: &TableDescription) -> Self {
        let keys = |schema: &[aws_sdk_dynamodb::types::KeySchemaElement]| {
            let mut p = String::new();
            let mut s = None;
            for k in schema {
                match k.key_type().as_str() {
                    "HASH" => p = k.attribute_name().to_owned(),
                    _ => s = Some(k.attribute_name().to_owned()),
                }
            }
            (p, s)
        };
        let (partition, sort) = keys(t.key_schema());
        let mut indexes = Vec::new();
        for g in t.global_secondary_indexes() {
            let (p, s) = keys(g.key_schema());
            indexes.push((g.index_name().unwrap_or_default().to_owned(), p, s));
        }
        for l in t.local_secondary_indexes() {
            let (p, s) = keys(l.key_schema());
            indexes.push((l.index_name().unwrap_or_default().to_owned(), p, s));
        }
        Self {
            partition,
            sort,
            indexes,
        }
    }

    /// Key attributes first in results.
    pub fn order(&self) -> Vec<String> {
        let mut v = vec![self.partition.clone()];
        v.extend(self.sort.clone());
        v
    }

    /// Partition key of the table or of one of its indexes.
    pub fn partition_of(&self, index: Option<&str>) -> &str {
        index
            .and_then(|i| self.indexes.iter().find(|(n, ..)| n == i))
            .map_or(self.partition.as_str(), |(_, p, _)| p.as_str())
    }
}

pub struct DynamoConnection {
    pub(crate) client: Client,
    /// Name of the single container (the region).
    pub(crate) container: String,
    label: String,
    keys: HashMap<String, Arc<TableKeys>>,
}

/// SDK error → contract error: service errors keep their code
/// (`ResourceNotFoundException`, `ValidationException`…).
pub(crate) fn map_err<E, R>(e: SdkError<E, R>) -> Error
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
    R: std::fmt::Debug,
{
    match &e {
        SdkError::ServiceError(s) => {
            let err = s.err();
            Error::Query(QueryError {
                message: err
                    .message()
                    .map(str::to_owned)
                    .unwrap_or_else(|| DisplayErrorContext(&e).to_string()),
                code: err.code().map(str::to_owned),
                range: None,
            })
        }
        SdkError::TimeoutError(_) => Error::Timeout,
        SdkError::ConstructionFailure(_) => Error::Query(QueryError {
            message: DisplayErrorContext(&e).to_string(),
            code: None,
            range: None,
        }),
        _ => Error::Connection(DisplayErrorContext(&e).to_string()),
    }
}

/// Runs a request future unless the statement is cancelled first.
pub(crate) async fn call<T, E, R, F>(cancel: &CancellationToken, fut: F) -> Result<T>
where
    F: std::future::Future<Output = std::result::Result<T, SdkError<E, R>>>,
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
    R: std::fmt::Debug,
{
    tokio::select! {
        r = fut => r.map_err(map_err),
        _ = cancel.cancelled() => Err(Error::Cancelled),
    }
}

/// Sum of the consumed capacity of every page of a call.
#[derive(Default)]
pub(crate) struct Capacity {
    units: f64,
    tables: Vec<String>,
}

impl Capacity {
    pub(crate) fn add(&mut self, c: Option<&ConsumedCapacity>) {
        if let Some(c) = c {
            self.units += c.capacity_units().unwrap_or(0.0);
            if let Some(t) = c.table_name()
                && !self.tables.iter().any(|x| x == t)
            {
                self.tables.push(t.to_owned());
            }
        }
    }

    pub(crate) fn add_all(&mut self, cs: &[ConsumedCapacity]) {
        for c in cs {
            self.add(Some(c));
        }
    }

    pub(crate) async fn report(&self, sink: &ResultSink) {
        if self.tables.is_empty() && self.units == 0.0 {
            return;
        }
        let on = if self.tables.is_empty() {
            String::new()
        } else {
            format!(" on {}", self.tables.join(", "))
        };
        sink.notice(
            NoticeLevel::Info,
            format!("Consumed capacity: {} units{on}", trim_units(self.units)),
        )
        .await;
    }
}

fn trim_units(u: f64) -> String {
    let s = format!("{u:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A document result being streamed.
pub(crate) struct DocOut<'a> {
    sink: &'a ResultSink,
    buf: Vec<RawDocumentBuf>,
    pub truncated: bool,
    pub count: usize,
    began: bool,
}

impl<'a> DocOut<'a> {
    pub(crate) fn new(sink: &'a ResultSink) -> Self {
        Self {
            sink,
            buf: Vec::new(),
            truncated: false,
            count: 0,
            began: false,
        }
    }

    pub(crate) async fn begin(&mut self, database: &str, collection: Option<&str>) {
        self.began = true;
        if self
            .sink
            .send(SinkEvent::BeginDocuments {
                database: Some(database.to_owned()),
                collection: collection.map(str::to_owned),
            })
            .await
            .is_err()
        {
            self.truncated = true;
        }
    }

    /// Queues a document; `false` once the consumer went away.
    pub(crate) async fn push(&mut self, d: &Document) -> Result<bool> {
        if self.truncated {
            return Ok(false);
        }
        let raw = RawDocumentBuf::try_from(d).map_err(|e| Error::Other(format!("BSON: {e}")))?;
        self.buf.push(raw);
        self.count += 1;
        if self.buf.len() >= self.sink.batch_rows() {
            let batch = std::mem::take(&mut self.buf);
            if self.sink.send(SinkEvent::Documents(batch)).await.is_err() {
                self.truncated = true;
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) async fn finish(&mut self) {
        if !self.began {
            return;
        }
        if !self.truncated && !self.buf.is_empty() {
            let batch = std::mem::take(&mut self.buf);
            if self.sink.send(SinkEvent::Documents(batch)).await.is_err() {
                self.truncated = true;
            }
        }
        let _ = self.sink.send(SinkEvent::End).await;
    }
}

fn local(endpoint: &str) -> bool {
    let host = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, r)| r)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]") || host.ends_with(".local")
}

impl DynamoConnection {
    pub async fn connect(cfg: &ResolvedConfig) -> Result<Self> {
        ferrodb_net::install_crypto_provider();
        let region = cfg
            .prop("region")
            .or_else(|| cfg.prop("aws.region"))
            .unwrap_or("us-east-1")
            .to_owned();
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(Provider::Rustls(CryptoMode::Ring))
            .build_https();
        let mut loader = aws_config::defaults(BehaviorVersion::latest())
            .region(Region::new(region.clone()))
            .http_client(http)
            .timeout_config(
                TimeoutConfig::builder()
                    .connect_timeout(cfg.connect_timeout)
                    .build(),
            );
        if let Some(p) = cfg.prop("aws.profile") {
            loader = loader.profile_name(p);
        }
        let endpoint = cfg.prop("endpoint");
        let key = cfg
            .secret("aws.access_key_id")
            .or_else(|| cfg.prop("aws.access_key_id"));
        match (key, cfg.secret("aws.secret_access_key")) {
            (Some(k), Some(s)) => {
                loader = loader.credentials_provider(Credentials::new(
                    k,
                    s,
                    cfg.secret("aws.session_token").map(str::to_owned),
                    None,
                    "ferrodb",
                ));
            }
            // DynamoDB Local accepts any signature.
            _ if endpoint.is_some_and(local) && cfg.prop("aws.profile").is_none() => {
                loader = loader.credentials_provider(Credentials::new(
                    "local", "local", None, None, "ferrodb",
                ));
            }
            _ => {}
        }
        let sdk = loader.load().await;
        let mut b = aws_sdk_dynamodb::config::Builder::from(&sdk);
        if let Some(e) = endpoint {
            b = b.endpoint_url(e);
        }
        let client = Client::from_conf(b.build());
        let probe = tokio::time::timeout(
            cfg.connect_timeout + Duration::from_secs(5),
            client.list_tables().limit(1).send(),
        )
        .await
        .map_err(|_| Error::Connection("timed out connecting to DynamoDB".into()))?;
        probe.map_err(|e| match map_err(e) {
            Error::Query(q) => Error::Connection(match q.code {
                Some(c) => format!("{c}: {}", q.message),
                None => q.message,
            }),
            other => other,
        })?;
        let label = match endpoint {
            Some(e) if local(e) => format!("DynamoDB Local ({e})"),
            Some(e) => format!("Amazon DynamoDB ({region}, {e})"),
            None => format!("Amazon DynamoDB ({region})"),
        };
        Ok(Self {
            client,
            container: region,
            label,
            keys: HashMap::new(),
        })
    }

    /// Key schema of a table (`DescribeTable`, cached per connection).
    pub(crate) async fn table_keys(
        &mut self,
        table: &str,
        cancel: &CancellationToken,
    ) -> Result<Arc<TableKeys>> {
        if let Some(k) = self.keys.get(table) {
            return Ok(k.clone());
        }
        let r = call(
            cancel,
            self.client.describe_table().table_name(table).send(),
        )
        .await?;
        let keys = Arc::new(
            r.table()
                .map(TableKeys::from_description)
                .unwrap_or_default(),
        );
        self.keys.insert(table.to_owned(), keys.clone());
        Ok(keys)
    }

    pub(crate) fn forget(&mut self, table: &str) {
        self.keys.remove(table);
    }

    /// Runs one PartiQL statement, paging with `NextToken`.
    pub(crate) async fn statement(
        &mut self,
        text: &str,
        params: Vec<AttributeValue>,
        sink: &ResultSink,
        cancel: &CancellationToken,
    ) -> Result<ExecSummary> {
        let st = partiql::analyze(text);
        let reads = matches!(st.kind, Kind::Select | Kind::Exists | Kind::Other);
        let keys = match (&st.table, reads) {
            (Some(t), true) => self.table_keys(t, cancel).await.ok(),
            _ => None,
        };
        if let (Some(k), Some(t)) = (&keys, &st.table)
            && st.scans(k.partition_of(st.index.as_deref()))
        {
            sink.notice(
                NoticeLevel::Warning,
                format!(
                    "This SELECT has no `=` / `IN` condition on the partition key `{}`: DynamoDB \
                     runs it as a Scan of `{t}`, reading (and billing) every item it passes",
                    k.partition_of(st.index.as_deref())
                ),
            )
            .await;
        }
        let order = keys.map(|k| k.order()).unwrap_or_default();
        let mut out = DocOut::new(sink);
        if reads {
            let editable = (st.star && st.table.is_some())
                .then(|| st.table.clone())
                .flatten();
            out.begin(&self.container, editable.as_deref()).await;
        }
        let mut capacity = Capacity::default();
        let mut next: Option<String> = None;
        let params = (!params.is_empty()).then_some(params);
        loop {
            let r = call(
                cancel,
                self.client
                    .execute_statement()
                    .statement(text)
                    .set_parameters(params.clone())
                    .set_next_token(next.take())
                    .return_consumed_capacity(ReturnConsumedCapacity::Total)
                    .send(),
            )
            .await?;
            capacity.add(r.consumed_capacity());
            if reads {
                for item in r.items() {
                    if !out.push(&value::map_to_doc(item, &order)).await? {
                        break;
                    }
                }
            }
            match r.next_token() {
                Some(t) if !out.truncated => next = Some(t.to_owned()),
                _ => break,
            }
        }
        out.finish().await;
        capacity.report(sink).await;
        Ok(ExecSummary {
            affected: (!reads).then_some(1),
            results: usize::from(reads),
            truncated: out.truncated,
        })
    }
}

struct NoCancel;

impl RemoteCancel for NoCancel {
    fn cancel(&self) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

impl Connection for DynamoConnection {
    fn server_version(&self) -> String {
        self.label.clone()
    }

    fn execute<'a>(
        &'a mut self,
        req: Request,
        sink: ResultSink,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, Result<ExecSummary>> {
        Box::pin(async move {
            match req {
                Request::Text { text } => match partiql::api_call(&text) {
                    Some((op, json)) => {
                        let offset = text.find(json).unwrap_or(0);
                        let v = ops::parse_request(json, offset)?;
                        ops::run(self, op, &v, &sink, &cancel).await
                    }
                    None => self.statement(&text, Vec::new(), &sink, &cancel).await,
                },
                Request::Native { method, args } => {
                    let op = partiql::OPERATIONS
                        .iter()
                        .find(|o| o.eq_ignore_ascii_case(&method))
                        .ok_or_else(|| Error::Unsupported(format!("native call `{method}`")))?;
                    let v = ops::args_to_json(&args)?;
                    ops::run(self, op, &v, &sink, &cancel).await
                }
                Request::Mql(_) => Err(Error::Unsupported(
                    "MongoDB commands on a DynamoDB connection".into(),
                )),
            }
        })
    }

    fn remote_cancel(&self) -> Option<Arc<dyn RemoteCancel>> {
        // Dropping the request future is the cancellation.
        Some(Arc::new(NoCancel))
    }

    fn set_read_only(&mut self, _read_only: bool) -> BoxFuture<'_, Result<()>> {
        // No read-only sessions: the core refuses writes (and IAM policies
        // can enforce it at the server).
        Box::pin(async { Ok(()) })
    }

    fn set_schema<'a>(&'a mut self, _schema: &'a str) -> BoxFuture<'a, Result<()>> {
        // One container (the region): nothing to switch.
        Box::pin(async { Ok(()) })
    }

    fn current_schema(&mut self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move { Ok(Some(self.container.clone())) })
    }

    fn begin(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async {
            Err(Error::Unsupported(
                "DynamoDB runs a transaction as one call (ExecuteTransaction or \
                 TransactWriteItems)"
                    .into(),
            ))
        })
    }

    fn commit(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn rollback(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async { Ok(()) })
    }

    fn ping(&mut self) -> BoxFuture<'_, Result<()>> {
        Box::pin(async move {
            self.client
                .list_tables()
                .limit(1)
                .send()
                .await
                .map(|_| ())
                .map_err(map_err)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_endpoints() {
        assert!(local("http://localhost:8000"));
        assert!(local("http://127.0.0.1:56520/"));
        assert!(!local("https://dynamodb.eu-central-1.amazonaws.com"));
        assert_eq!(trim_units(0.5), "0.5");
        assert_eq!(trim_units(12.0), "12");
    }
}
