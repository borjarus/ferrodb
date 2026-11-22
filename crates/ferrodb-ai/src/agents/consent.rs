//! Consent requests: a call waits until the user decides, the request times
//! out or the client cancels it. "Allow for session" grants are remembered
//! per (MCP session, tool, data source) until the session ends.

use std::{
    collections::HashSet,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use ferrodb_api::{AgentEvent, ConsentDecision, ConsentRequest, DataSourceId};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use super::Events;

/// How a consent request ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsentOutcome {
    Allowed,
    /// Allowed by an earlier "allow for session" (nobody was asked).
    AllowedForSession,
    Denied,
    TimedOut,
    /// The client cancelled the call (or disconnected).
    Cancelled,
}

type GrantKey = (u64, String, Option<DataSourceId>);

struct Pending {
    request: ConsentRequest,
    reply: oneshot::Sender<ConsentDecision>,
}

#[derive(Default)]
pub struct ConsentBroker {
    next_id: AtomicU64,
    pending: Mutex<Vec<Pending>>,
    grants: Mutex<HashSet<GrantKey>>,
}

impl ConsentBroker {
    /// Asks the user about `request` on behalf of MCP session `session`.
    /// The request's `id` is assigned here.
    pub async fn ask(
        &self,
        events: &Events,
        session: u64,
        mut request: ConsentRequest,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> ConsentOutcome {
        let key: GrantKey = (session, request.tool.clone(), request.data_source.clone());
        if !request.modifies && self.grants.lock().unwrap().contains(&key) {
            return ConsentOutcome::AllowedForSession;
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        request.id = id;
        let modifies = request.modifies;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().push(Pending {
            request: request.clone(),
            reply: tx,
        });
        events.emit(AgentEvent::ConsentRequested(request));
        let outcome = tokio::select! {
            decision = rx => match decision {
                Ok(ConsentDecision::AllowOnce) => ConsentOutcome::Allowed,
                Ok(ConsentDecision::AllowForSession) => {
                    // Never remembered for calls that change data or schema.
                    if !modifies {
                        self.grants.lock().unwrap().insert(key);
                    }
                    ConsentOutcome::Allowed
                }
                Ok(ConsentDecision::Deny) => ConsentOutcome::Denied,
                Err(_) => ConsentOutcome::Cancelled,
            },
            _ = tokio::time::sleep(timeout) => ConsentOutcome::TimedOut,
            _ = cancel.cancelled() => ConsentOutcome::Cancelled,
        };
        self.pending.lock().unwrap().retain(|p| p.request.id != id);
        events.emit(AgentEvent::ConsentClosed { id });
        outcome
    }

    /// Answers a waiting request; unknown ids (already closed) are ignored.
    pub fn decide(&self, id: u64, decision: ConsentDecision) {
        let pending = {
            let mut list = self.pending.lock().unwrap();
            list.iter()
                .position(|p| p.request.id == id)
                .map(|i| list.remove(i))
        };
        if let Some(p) = pending {
            let _ = p.reply.send(decision);
        }
    }

    /// Requests still waiting, oldest first.
    pub fn pending(&self) -> Vec<ConsentRequest> {
        self.pending
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.request.clone())
            .collect()
    }

    /// Forgets the "allow for session" grants of an MCP session that ended.
    pub fn end_session(&self, session: u64) {
        self.grants
            .lock()
            .unwrap()
            .retain(|(s, _, _)| *s != session);
    }
}
