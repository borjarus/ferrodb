//! Where a connection streams its results. The channel is bounded, so a
//! consumer that stops reading (a paused result page) stops the driver,
//! which in turn stops reading from the socket: backpressure all the way to
//! the server. Dropping the receiver ends the statement early
//! ([`crate::ExecSummary::truncated`]).

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use bson::RawDocumentBuf;
use ferrodb_api::{NoticeLevel, Result, ServerNotice};
use tokio::sync::mpsc;

use crate::{CancellationToken, Connection, ExecSummary, Request};

#[derive(Clone, Debug)]
pub enum SinkEvent {
    /// A result set starts (also for empty results with a known shape).
    Begin {
        schema: SchemaRef,
    },
    Batch(RecordBatch),
    /// A document result starts (MongoDB). `collection` names the
    /// container the documents come from, when there is one.
    BeginDocuments {
        database: Option<String>,
        collection: Option<String>,
    },
    /// Documents of the current document result.
    Documents(Vec<RawDocumentBuf>),
    /// The current result set is complete.
    End,
    Notice(ServerNotice),
}

/// The consumer went away; the driver should stop producing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SinkClosed;

#[derive(Clone, Debug)]
pub struct ResultSink {
    tx: mpsc::Sender<SinkEvent>,
    batch_rows: usize,
}

impl ResultSink {
    /// Default rows per batch (ADR-002: chunks of at most 1000 items).
    pub const BATCH_ROWS: usize = 1000;

    pub fn channel(capacity: usize, batch_rows: usize) -> (Self, mpsc::Receiver<SinkEvent>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (
            Self {
                tx,
                batch_rows: batch_rows.max(1),
            },
            rx,
        )
    }

    /// Rows per record batch the driver should produce.
    pub fn batch_rows(&self) -> usize {
        self.batch_rows
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    pub async fn send(&self, ev: SinkEvent) -> Result<(), SinkClosed> {
        self.tx.send(ev).await.map_err(|_| SinkClosed)
    }

    /// For drivers running on their own threads (SQLite). Must not be called
    /// from inside an async task.
    pub fn blocking_send(&self, ev: SinkEvent) -> Result<(), SinkClosed> {
        self.tx.blocking_send(ev).map_err(|_| SinkClosed)
    }

    pub async fn notice(&self, level: NoticeLevel, message: impl Into<String>) {
        let _ = self
            .send(SinkEvent::Notice(ServerNotice {
                level,
                message: message.into(),
            }))
            .await;
    }
}

/// A fully read result set.
#[derive(Clone, Debug)]
pub struct Collected {
    pub schema: SchemaRef,
    pub batches: Vec<RecordBatch>,
}

impl Collected {
    pub fn num_rows(&self) -> usize {
        self.batches.iter().map(RecordBatch::num_rows).sum()
    }
}

/// Everything a statement produced.
#[derive(Clone, Debug, Default)]
pub struct CollectedOutput {
    pub summary: ExecSummary,
    pub results: Vec<Collected>,
    /// Documents of every document result, in order.
    pub documents: Vec<RawDocumentBuf>,
    pub notices: Vec<ServerNotice>,
}

/// Runs one statement and reads all its results into memory (internal
/// queries, metadata listings, tests). Not for user queries: those stream.
pub async fn collect(
    conn: &mut dyn Connection,
    text: impl Into<String>,
    cancel: CancellationToken,
) -> Result<CollectedOutput> {
    collect_request(conn, Request::Text { text: text.into() }, cancel).await
}

/// [`collect`] for any request.
pub async fn collect_request(
    conn: &mut dyn Connection,
    req: Request,
    cancel: CancellationToken,
) -> Result<CollectedOutput> {
    let (sink, mut rx) = ResultSink::channel(16, ResultSink::BATCH_ROWS);
    let exec = conn.execute(req, sink, cancel);
    let drain = async move {
        let mut results: Vec<Collected> = Vec::new();
        let mut documents = Vec::new();
        let mut notices = Vec::new();
        while let Some(ev) = rx.recv().await {
            match ev {
                SinkEvent::Begin { schema } => results.push(Collected {
                    schema,
                    batches: Vec::new(),
                }),
                SinkEvent::Batch(b) => {
                    if let Some(r) = results.last_mut() {
                        r.batches.push(b);
                    }
                }
                SinkEvent::BeginDocuments { .. } | SinkEvent::End => {}
                SinkEvent::Documents(mut d) => documents.append(&mut d),
                SinkEvent::Notice(n) => notices.push(n),
            }
        }
        (results, documents, notices)
    };
    let (summary, (results, documents, notices)) = futures::join!(exec, drain);
    Ok(CollectedOutput {
        summary: summary?,
        results,
        documents,
        notices,
    })
}
