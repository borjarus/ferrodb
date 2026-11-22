//! Consent flow, settings and token of the permission layer (on the mock).

use std::{sync::Arc, time::Duration};

use ferrodb_api::{
    AgentAccess, AgentEvent, AgentPolicy, AgentService, AuditOutcome, Backend, ConsentDecision,
    ConsentMode, DataSourceId, McpSettings,
};
use ferrodb_mock::MockBackend;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::AiOptions;

fn agents() -> Arc<Agents> {
    let backend: Arc<dyn Backend> = Arc::new(MockBackend::new());
    Agents::new(backend, AiOptions::in_memory())
}

/// A call on `shop-dev` (a writable PostgreSQL-like source).
fn call(agents: &Agents, session: u64, tool: &'static str, class: CallClass) -> Call {
    Call {
        session,
        client: "test-agent".into(),
        tool,
        data_source: agents.visible_source(&DataSourceId::new("shop-dev")),
        text: Some("select 1".into()),
        class,
        risks: Vec::new(),
        summary: "Run a query on shop (dev)".into(),
    }
}

/// Answers every consent request with `decision`; returns the requests seen.
fn answer(agents: &Arc<Agents>, decision: ConsentDecision) -> async_channel::Receiver<u64> {
    let rx = agents.subscribe();
    let (seen_tx, seen_rx) = async_channel::unbounded();
    let a = agents.clone();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let AgentEvent::ConsentRequested(r) = ev {
                let _ = seen_tx.send(r.id).await;
                a.decide(r.id, decision);
            }
        }
    });
    seen_rx
}

async fn next_request(rx: &async_channel::Receiver<AgentEvent>) -> ConsentRequest {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("an event")
            .expect("open channel");
        if let AgentEvent::ConsentRequested(r) = ev {
            return r;
        }
    }
}

async fn next_closed(rx: &async_channel::Receiver<AgentEvent>) -> u64 {
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("an event")
            .expect("open channel");
        if let AgentEvent::ConsentClosed { id } = ev {
            return id;
        }
    }
}

const ALLOWED: Verdict = Verdict::Allowed(AuditOutcome::AllowedByUser);

#[tokio::test]
async fn allow_once_asks_every_time() {
    let agents = agents();
    let events = agents.subscribe();
    let seen = answer(&agents, ConsentDecision::AllowOnce);
    let ct = CancellationToken::new();
    let c = call(&agents, 1, "run_query", CallClass::Read);
    assert_eq!(agents.authorize(&c, &ct).await, ALLOWED);
    assert_eq!(agents.authorize(&c, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 2);
    let r = next_request(&events).await;
    assert_eq!(r.tool, "run_query");
    assert_eq!(r.data_source_name.as_deref(), Some("shop (dev)"));
    assert_eq!(r.text.as_deref(), Some("select 1"));
    assert!(!r.modifies);
    assert_eq!(next_closed(&events).await, r.id);
    assert!(agents.pending().is_empty());
}

#[tokio::test]
async fn pending_lists_waiting_requests() {
    let agents = agents();
    let events = agents.subscribe();
    let a = agents.clone();
    let waiting = tokio::spawn(async move {
        let c = call(&a, 1, "list_objects", CallClass::Metadata);
        a.authorize(&c, &CancellationToken::new()).await
    });
    let r = next_request(&events).await;
    assert_eq!(agents.pending(), vec![r.clone()]);
    agents.decide(r.id, ConsentDecision::AllowOnce);
    assert_eq!(waiting.await.unwrap(), ALLOWED);
    assert!(agents.pending().is_empty());
    // A late or unknown decision is ignored.
    agents.decide(r.id, ConsentDecision::Deny);
    agents.decide(999, ConsentDecision::Deny);
}

#[tokio::test]
async fn allow_for_session_is_reused_but_not_for_writes() {
    let agents = agents();
    let mut policy = agents.policy();
    policy.sources = vec![(DataSourceId::new("shop-dev"), AgentAccess::ReadWrite)];
    agents.save_policy(policy).await.unwrap();
    let seen = answer(&agents, ConsentDecision::AllowForSession);
    let ct = CancellationToken::new();

    let read = call(&agents, 1, "run_query", CallClass::Read);
    assert_eq!(agents.authorize(&read, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 1);
    // Same session, tool and source: not asked again.
    assert_eq!(agents.authorize(&read, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 1);
    // Another tool, another session: asked.
    let other_tool = call(&agents, 1, "explain_query", CallClass::Read);
    assert_eq!(agents.authorize(&other_tool, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 2);
    let other_session = call(&agents, 2, "run_query", CallClass::Read);
    assert_eq!(agents.authorize(&other_session, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 3);
    // Writes are asked every time, even after "allow for session".
    let write = call(&agents, 1, "run_query", CallClass::Modify);
    assert_eq!(agents.authorize(&write, &ct).await, ALLOWED);
    assert_eq!(agents.authorize(&write, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 5);
    // The grant ends with the session.
    agents.session_ended(1);
    assert_eq!(agents.authorize(&read, &ct).await, ALLOWED);
    assert_eq!(seen.len(), 6);
}

#[tokio::test]
async fn deny() {
    let agents = agents();
    let _seen = answer(&agents, ConsentDecision::Deny);
    let c = call(&agents, 1, "run_query", CallClass::Read);
    assert_eq!(
        agents.authorize(&c, &CancellationToken::new()).await,
        Verdict::Refused {
            outcome: AuditOutcome::DeniedByUser,
            message: "denied by the user".into()
        }
    );
}

#[tokio::test(start_paused = true)]
async fn unanswered_requests_time_out() {
    let agents = agents();
    let mut policy = agents.policy();
    policy.consent_timeout_secs = 30;
    agents.save_policy(policy).await.unwrap();
    let events = agents.subscribe();
    let c = call(&agents, 1, "run_query", CallClass::Read);
    let verdict = agents.authorize(&c, &CancellationToken::new()).await;
    let Verdict::Refused { outcome, message } = verdict else {
        panic!("expected a refusal, got {verdict:?}");
    };
    assert_eq!(outcome, AuditOutcome::TimedOut);
    assert!(message.contains("30 s"), "{message}");
    let r = next_request(&events).await;
    assert_eq!(next_closed(&events).await, r.id);
    assert!(agents.pending().is_empty());
}

#[tokio::test]
async fn every_subscriber_sees_the_request() {
    let agents = agents();
    let first = agents.subscribe();
    let second = agents.subscribe();
    let dropped = agents.subscribe();
    drop(dropped);
    let a = agents.clone();
    let waiting = tokio::spawn(async move {
        let c = call(&a, 1, "run_query", CallClass::Read);
        a.authorize(&c, &CancellationToken::new()).await
    });
    let r1 = next_request(&first).await;
    let r2 = next_request(&second).await;
    assert_eq!(r1, r2);
    agents.decide(r1.id, ConsentDecision::AllowOnce);
    assert_eq!(waiting.await.unwrap(), ALLOWED);
    assert_eq!(next_closed(&first).await, r1.id);
    assert_eq!(next_closed(&second).await, r1.id);
    // The closed receiver was dropped from the list.
    assert_eq!(agents.events.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn client_cancellation_closes_the_request() {
    let agents = agents();
    let events = agents.subscribe();
    let ct = CancellationToken::new();
    let a = agents.clone();
    let token = ct.clone();
    let waiting = tokio::spawn(async move {
        let c = call(&a, 1, "run_query", CallClass::Read);
        a.authorize(&c, &token).await
    });
    let r = next_request(&events).await;
    ct.cancel();
    assert_eq!(
        waiting.await.unwrap(),
        Verdict::Refused {
            outcome: AuditOutcome::DeniedByUser,
            message: "cancelled by the client".into()
        }
    );
    assert_eq!(next_closed(&events).await, r.id);
    assert!(agents.pending().is_empty());
}

#[tokio::test]
async fn policy_refusals_do_not_ask() {
    let agents = agents();
    let events = agents.subscribe();
    let ct = CancellationToken::new();
    // Read-only by default: a write is refused at once.
    let write = call(&agents, 1, "run_query", CallClass::Modify);
    assert_eq!(
        agents.authorize(&write, &ct).await,
        Verdict::Refused {
            outcome: AuditOutcome::DeniedByPolicy,
            message: policy::READ_ONLY_FOR_AGENTS.into()
        }
    );
    // Writes only: reads are allowed without asking.
    let mut p = agents.policy();
    p.consent = ConsentMode::WritesOnly;
    agents.save_policy(p).await.unwrap();
    let read = call(&agents, 1, "run_query", CallClass::Read);
    assert_eq!(
        agents.authorize(&read, &ct).await,
        Verdict::Allowed(AuditOutcome::AllowedByPolicy)
    );
    assert!(events.try_recv().is_err(), "nobody was asked");
}

#[test]
fn hidden_sources_are_invisible() {
    let agents = agents();
    let shop = DataSourceId::new("shop-dev");
    assert!(agents.visible_source(&shop).is_some());
    assert!(agents.visible_source(&DataSourceId::new("nope")).is_none());
    let policy = AgentPolicy {
        sources: vec![(shop.clone(), AgentAccess::Hidden)],
        ..AgentPolicy::default()
    };
    futures::executor::block_on(agents.save_policy(policy)).unwrap();
    assert!(agents.visible_source(&shop).is_none());
    // Hidden by default, visible when listed.
    let policy = AgentPolicy {
        default_access: AgentAccess::Hidden,
        sources: vec![(shop.clone(), AgentAccess::ReadOnly)],
        ..AgentPolicy::default()
    };
    futures::executor::block_on(agents.save_policy(policy)).unwrap();
    assert!(agents.visible_source(&shop).is_some());
    assert!(
        agents
            .visible_source(&DataSourceId::new("catalog"))
            .is_none()
    );
}

#[test]
fn settings_and_token_persist() {
    let dir = tempfile::tempdir().unwrap();
    let secrets: Arc<dyn ferrodb_secrets::SecretStore> =
        Arc::new(ferrodb_secrets::MemoryStore::new());
    let options = AiOptions {
        config_dir: Some(dir.path().join("config")),
        data_dir: Some(dir.path().join("data")),
        secrets: secrets.clone(),
    };
    let backend: Arc<dyn Backend> = Arc::new(MockBackend::new());
    let agents = Agents::new(backend.clone(), options.clone());
    let policy = AgentPolicy {
        default_access: AgentAccess::MetadataOnly,
        sources: vec![(DataSourceId::new("shop-dev"), AgentAccess::ReadWrite)],
        consent: ConsentMode::QueriesOnly,
        max_rows: 50,
        consent_timeout_secs: 10,
    };
    futures::executor::block_on(agents.save_policy(policy.clone())).unwrap();
    // Disabled server, another port: saved without starting anything.
    let mcp = McpSettings {
        enabled: false,
        port: 7400,
    };
    futures::executor::block_on(agents.save_mcp_settings(mcp.clone())).unwrap();
    assert!(!agents.status().running);
    assert!(agents.connection_info().is_none());

    let token = agents.token().unwrap();
    assert_eq!(token.len(), 64);
    assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(agents.token().unwrap(), token);
    assert_eq!(secrets.get(TOKEN_REF).unwrap().get(TOKEN_KEY), Some(&token));

    let again = Agents::new(backend, options);
    assert_eq!(again.policy(), policy);
    assert_eq!(again.mcp_settings(), mcp);
    assert_eq!(again.token().unwrap(), token);
    futures::executor::block_on(again.regenerate_token()).unwrap();
    assert_ne!(again.token().unwrap(), token);
    assert_eq!(
        secrets.get(TOKEN_REF).unwrap().get(TOKEN_KEY),
        Some(&again.token().unwrap())
    );
}

#[test]
fn client_config_points_at_the_bridge() {
    let config = client_config(Path::new(r"C:\Program Files\FerroDb\ferrodb.exe"));
    let v: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(
        v["mcpServers"]["ferrodb"]["command"],
        r"C:\Program Files\FerroDb\ferrodb.exe"
    );
    assert_eq!(
        v["mcpServers"]["ferrodb"]["args"],
        serde_json::json!(["mcp"])
    );
}
