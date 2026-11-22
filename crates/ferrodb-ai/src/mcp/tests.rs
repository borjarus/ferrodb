//! End to end: the MCP server on the mock backend, an rmcp client over
//! streamable HTTP, the HTTP guards and the stdio bridge.

use std::{sync::Arc, time::Duration};

use ferrodb_api::{
    AgentAccess, AgentEvent, AgentPolicy, AgentService, AuditOutcome, Backend, ConsentDecision,
    ConsentMode, DataSourceId, McpSettings,
};
use ferrodb_mock::MockBackend;
use rmcp::{
    RoleClient, ServiceExt as _,
    model::{
        CallToolRequest, CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig,
        ClientRequest, Implementation,
    },
    service::{PeerRequestOptions, RunningService},
    transport::async_rw::AsyncRwTransport,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::{bridge, tools};
use crate::{AiOptions, agents::Agents};

struct Server {
    agents: Arc<Agents>,
    url: String,
    token: String,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let off = McpSettings {
            enabled: false,
            port: 0,
        };
        let _ = futures::executor::block_on(self.agents.save_mcp_settings(off));
    }
}

async fn server(policy: AgentPolicy) -> Server {
    let backend: Arc<dyn Backend> = Arc::new(MockBackend::new());
    let agents = Agents::new(backend, AiOptions::in_memory());
    agents.save_policy(policy).await.unwrap();
    // Port 0: any free port (the settings UI never offers it).
    agents
        .save_mcp_settings(McpSettings {
            enabled: true,
            port: 0,
        })
        .await
        .unwrap();
    let status = agents.status();
    assert!(status.running, "{status:?}");
    let info = agents.connection_info().expect("connection info");
    assert_eq!(status.url.as_deref(), Some(info.url.as_str()));
    let port = info
        .url
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    Server {
        agents,
        url: info.url,
        token: info.token,
        port,
    }
}

fn client_info() -> ClientConfig {
    ClientConfig::new(
        ClientCapabilities::default(),
        Implementation::new("test-agent", "1.0"),
    )
}

async fn connect(url: &str, token: &str) -> RunningService<RoleClient, ClientConfig> {
    client_info()
        .serve(bridge::http_transport(url, token))
        .await
        .expect("initialize")
}

/// Answers every consent request with `decision`; counts them (and whether
/// they modify).
fn auto_consent(
    agents: &Arc<Agents>,
    decision: ConsentDecision,
) -> async_channel::Receiver<(String, bool)> {
    let rx = agents.subscribe();
    let (tx, seen) = async_channel::unbounded();
    let a = agents.clone();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let AgentEvent::ConsentRequested(r) = ev {
                let _ = tx.send((r.tool.clone(), r.modifies)).await;
                a.decide(r.id, decision);
            }
        }
    });
    seen
}

async fn call(
    client: &RunningService<RoleClient, ClientConfig>,
    tool: &'static str,
    args: Value,
) -> CallToolResult {
    let Value::Object(args) = args else {
        panic!("arguments are an object");
    };
    client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
        .await
        .expect("tools/call")
}

fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect()
}

fn json_of(result: &CallToolResult) -> Value {
    assert_eq!(result.is_error, Some(false), "{}", text(result));
    serde_json::from_str(&text(result)).expect("JSON content")
}

async fn eventually(mut check: impl FnMut() -> bool) {
    for _ in 0..100 {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("condition not reached");
}

#[tokio::test(flavor = "multi_thread")]
async fn tools_through_the_permission_layer() {
    let policy = AgentPolicy {
        sources: vec![(DataSourceId::new("cache"), AgentAccess::Hidden)],
        ..AgentPolicy::default()
    };
    let s = server(policy).await;
    let seen = auto_consent(&s.agents, ConsentDecision::AllowOnce);
    let client = connect(&s.url, &s.token).await;
    eventually(|| s.agents.status().clients == ["test-agent"]).await;

    let tools = client.list_all_tools().await.unwrap();
    let names: Vec<_> = tools.iter().map(|t| t.name.to_string()).collect();
    assert_eq!(
        names,
        [
            tools::LIST_DATA_SOURCES,
            tools::LIST_OBJECTS,
            tools::DESCRIBE_OBJECT,
            tools::RUN_QUERY,
            tools::EXPLAIN_QUERY
        ]
    );
    let run = tools.iter().find(|t| t.name == tools::RUN_QUERY).unwrap();
    let hints = run.annotations.as_ref().unwrap();
    assert_eq!(hints.read_only_hint, Some(false));
    assert_eq!(hints.destructive_hint, Some(true));
    assert!(run.input_schema["required"] == json!(["data_source", "query"]));

    // Hidden sources are listed nowhere.
    let list = json_of(&call(&client, tools::LIST_DATA_SOURCES, json!({})).await);
    let ids: Vec<&str> = list["data_sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"shop-dev") && ids.contains(&"catalog"),
        "{ids:?}"
    );
    assert!(!ids.contains(&"cache"));
    let shop = list["data_sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == "shop-dev")
        .unwrap();
    assert_eq!(shop["access"], "read-only");
    assert_eq!(shop["data_model"], "tabular");

    // A query: the first rows, capped by `max_rows`.
    let r = json_of(
        &call(
            &client,
            tools::RUN_QUERY,
            json!({ "data_source": "shop-dev", "query": "select * from orders", "max_rows": 5 }),
        )
        .await,
    );
    let result = &r["results"][0];
    assert_eq!(result["kind"], "table");
    assert_eq!(result["rows"].as_array().unwrap().len(), 5);
    assert_eq!(result["count"], 5);
    assert_eq!(result["truncated"], true);
    assert!(!result["columns"].as_array().unwrap().is_empty());

    // Documents.
    let r = json_of(
        &call(
            &client,
            tools::RUN_QUERY,
            json!({ "data_source": "catalog", "query": "db.customers.find({})", "max_rows": 3 }),
        )
        .await,
    );
    assert_eq!(r["results"][0]["documents"].as_array().unwrap().len(), 3);

    // A write under the default read-only access: refused, nobody asked.
    let asked = seen.len();
    let denied = call(
        &client,
        tools::RUN_QUERY,
        json!({ "data_source": "shop-dev", "query": "delete from orders where id = 1" }),
    )
    .await;
    assert_eq!(denied.is_error, Some(true));
    assert!(
        text(&denied).contains("read-only for agents"),
        "{}",
        text(&denied)
    );
    assert_eq!(seen.len(), asked);

    // Hidden source: not found.
    let hidden = call(
        &client,
        tools::RUN_QUERY,
        json!({ "data_source": "cache", "query": "keys *" }),
    )
    .await;
    assert_eq!(hidden.is_error, Some(true));
    assert!(text(&hidden).contains("not found"));

    // Explorer tree and definitions.
    let top = json_of(
        &call(
            &client,
            tools::LIST_OBJECTS,
            json!({ "data_source": "shop-dev" }),
        )
        .await,
    );
    assert!(
        top["objects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["name"] == "shop")
    );
    assert!(top["hierarchy"].is_array());
    let tables = json_of(
        &call(
            &client,
            tools::LIST_OBJECTS,
            json!({ "data_source": "shop-dev", "path": ["shop", "public"] }),
        )
        .await,
    );
    let table = tables["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["kind"] == "Table")
        .unwrap()["name"]
        .as_str()
        .unwrap()
        .to_owned();
    let ddl = json_of(
        &call(
            &client,
            tools::DESCRIBE_OBJECT,
            json!({ "data_source": "shop-dev", "path": ["shop", "public", table] }),
        )
        .await,
    );
    assert!(ddl["definition"].as_str().unwrap().contains("CREATE TABLE"));
    let fields = json_of(
        &call(
            &client,
            tools::DESCRIBE_OBJECT,
            json!({ "data_source": "catalog", "path": ["catalog", "customers"] }),
        )
        .await,
    );
    assert!(
        fields["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == "_id")
    );
    let missing = call(
        &client,
        tools::DESCRIBE_OBJECT,
        json!({ "data_source": "shop-dev", "path": ["shop", "nope"] }),
    )
    .await;
    assert_eq!(missing.is_error, Some(true));

    let plan = call(
        &client,
        tools::EXPLAIN_QUERY,
        json!({ "data_source": "shop-dev", "query": "select * from orders" }),
    )
    .await;
    assert_eq!(plan.is_error, Some(false), "{}", text(&plan));
    assert!(!text(&plan).trim().is_empty());

    // Everything is in the audit log, newest first, without result data.
    let audit = s.agents.audit(100).await.unwrap();
    assert_eq!(audit[0].tool, tools::EXPLAIN_QUERY);
    let query = audit
        .iter()
        .find(|e| e.text.as_deref() == Some("select * from orders") && e.tool == tools::RUN_QUERY)
        .unwrap();
    assert_eq!(query.outcome, AuditOutcome::AllowedByUser);
    assert_eq!(query.items, Some(5));
    assert_eq!(query.client, "test-agent");
    let refused = audit
        .iter()
        .find(|e| e.text.as_deref() == Some("delete from orders where id = 1"))
        .unwrap();
    assert_eq!(refused.outcome, AuditOutcome::DeniedByPolicy);

    client.cancel().await.unwrap();
    eventually(|| s.agents.status().clients.is_empty()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn writes_need_consent_every_time() {
    let policy = AgentPolicy {
        sources: vec![
            (DataSourceId::new("shop-dev"), AgentAccess::ReadWrite),
            (DataSourceId::new("shop-prod"), AgentAccess::ReadWrite),
        ],
        consent: ConsentMode::WritesOnly,
        ..AgentPolicy::default()
    };
    let s = server(policy).await;
    let seen = auto_consent(&s.agents, ConsentDecision::AllowForSession);
    let client = connect(&s.url, &s.token).await;

    // Reads are not asked under "writes only".
    let r = call(
        &client,
        tools::RUN_QUERY,
        json!({ "data_source": "shop-dev", "query": "select 1" }),
    )
    .await;
    assert_eq!(r.is_error, Some(false));
    assert_eq!(seen.len(), 0);

    for _ in 0..2 {
        let r = json_of(
            &call(
                &client,
                tools::RUN_QUERY,
                json!({ "data_source": "shop-dev", "query": "insert into orders values (1)" }),
            )
            .await,
        );
        assert_eq!(r["affected_rows"], 1);
    }
    // "Allow for session" is never remembered for writes.
    assert_eq!(seen.len(), 2);
    assert_eq!(seen.try_recv().unwrap(), ("run_query".to_owned(), true));

    // The production source is read-only itself.
    let denied = call(
        &client,
        tools::RUN_QUERY,
        json!({ "data_source": "shop-prod", "query": "update orders set total = 0 where id = 1" }),
    )
    .await;
    assert_eq!(denied.is_error, Some(true));
    assert!(text(&denied).contains("read-only"), "{}", text(&denied));
    client.cancel().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn denied_and_cancelled_calls() {
    let s = server(AgentPolicy::default()).await;
    let client = connect(&s.url, &s.token).await;

    // Denied by the user.
    let seen = auto_consent(&s.agents, ConsentDecision::Deny);
    let r = call(&client, tools::LIST_DATA_SOURCES, json!({})).await;
    assert_eq!(r.is_error, Some(true));
    assert_eq!(text(&r), "denied by the user");
    assert_eq!(seen.len(), 1);
    drop(seen);

    // Cancelled by the client while the user has not answered.
    let s2 = server(AgentPolicy::default()).await;
    let events = s2.agents.subscribe();
    let client2 = connect(&s2.url, &s2.token).await;
    let request = ClientRequest::CallToolRequest(CallToolRequest::new(CallToolRequestParams::new(
        tools::LIST_DATA_SOURCES,
    )));
    let handle = client2
        .peer()
        .send_cancellable_request(request, PeerRequestOptions::no_options())
        .await
        .unwrap();
    let id = loop {
        if let AgentEvent::ConsentRequested(r) = events.recv().await.unwrap() {
            break r.id;
        }
    };
    handle.cancel(Some("test".into())).await.unwrap();
    loop {
        if let AgentEvent::ConsentClosed { id: closed } = events.recv().await.unwrap() {
            assert_eq!(closed, id);
            break;
        }
    }
    assert!(s2.agents.pending().is_empty());
    eventually(|| {
        futures::executor::block_on(s2.agents.audit(1))
            .unwrap()
            .first()
            .is_some_and(|e| e.error.as_deref() == Some("cancelled by the client"))
    })
    .await;
    client.cancel().await.unwrap();
    client2.cancel().await.unwrap();
}

/// Sends one raw HTTP request and returns the status code.
async fn raw_status(port: u16, headers: &[(&str, &str)]) -> u16 {
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let mut req = format!(
        "POST /mcp HTTP/1.1\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\nConnection: close\r\n\
         Content-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    stream.write_all(req.as_bytes()).await.unwrap();
    let mut buf = vec![0; 64];
    let n = stream.read(&mut buf).await.unwrap();
    let line = String::from_utf8_lossy(&buf[..n]).to_string();
    line.split(' ').nth(1).unwrap().parse().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn http_guards() {
    let s = server(AgentPolicy::default()).await;
    let host = format!("127.0.0.1:{}", s.port);
    let bearer = format!("Bearer {}", s.token);
    assert_eq!(raw_status(s.port, &[("Host", &host)]).await, 401);
    assert_eq!(
        raw_status(s.port, &[("Host", &host), ("Authorization", "Bearer nope")]).await,
        401
    );
    // DNS rebinding: a foreign Host or Origin is refused even with the token.
    assert_eq!(
        raw_status(
            s.port,
            &[("Host", "evil.example:80"), ("Authorization", &bearer)]
        )
        .await,
        403
    );
    assert_eq!(
        raw_status(
            s.port,
            &[
                ("Host", &host),
                ("Origin", "https://evil.example"),
                ("Authorization", &bearer)
            ]
        )
        .await,
        403
    );
    // With the token the request reaches MCP (a ping outside a session is
    // not a 401/403).
    let ok = raw_status(s.port, &[("Host", &host), ("Authorization", &bearer)]).await;
    assert!(ok != 401 && ok != 403, "{ok}");

    // An rmcp client with a wrong token cannot initialize.
    assert!(
        client_info()
            .serve(bridge::http_transport(&s.url, "wrong"))
            .await
            .is_err()
    );

    // A new token: the old one stops working.
    let old = s.token.clone();
    s.agents.regenerate_token().await.unwrap();
    let new = s.agents.connection_info().unwrap();
    assert_ne!(new.token, old);
    let port: u16 = new
        .url
        .trim_start_matches("http://127.0.0.1:")
        .trim_end_matches("/mcp")
        .parse()
        .unwrap();
    let host = format!("127.0.0.1:{port}");
    assert_eq!(
        raw_status(
            port,
            &[("Host", &host), ("Authorization", &format!("Bearer {old}"))]
        )
        .await,
        401
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn busy_port_is_reported() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let backend: Arc<dyn Backend> = Arc::new(MockBackend::new());
    let agents = Agents::new(backend, AiOptions::in_memory());
    let events = agents.subscribe();
    agents
        .save_mcp_settings(McpSettings {
            enabled: true,
            port,
        })
        .await
        .unwrap();
    let status = agents.status();
    assert!(!status.running);
    assert!(status.error.unwrap().contains("already in use"));
    assert!(matches!(events.try_recv(), Ok(AgentEvent::Status(_))));
    assert!(agents.connection_info().is_none());
}

/// The stdio bridge: an MCP client on one end of a pipe, the bridge in the
/// middle, the IDE's HTTP endpoint on the other side.
#[tokio::test(flavor = "multi_thread")]
async fn bridge_forwards_both_ways() {
    let policy = AgentPolicy {
        consent: ConsentMode::WritesOnly,
        ..AgentPolicy::default()
    };
    let s = server(policy).await;
    let (client_end, bridge_end) = tokio::io::duplex(1 << 16);
    let (bridge_read, bridge_write) = tokio::io::split(bridge_end);
    let forwarding = tokio::spawn(bridge::forward(
        AsyncRwTransport::new_server(bridge_read, bridge_write),
        bridge::http_transport(&s.url, &s.token),
    ));
    let client = client_info()
        .serve(tokio::io::split(client_end))
        .await
        .expect("initialize through the bridge");
    eventually(|| s.agents.status().clients == ["test-agent"]).await;
    assert_eq!(client.list_all_tools().await.unwrap().len(), 5);
    let r = json_of(&call(&client, tools::LIST_DATA_SOURCES, json!({})).await);
    assert!(r["data_sources"].as_array().unwrap().len() >= 5);
    let r = json_of(
        &call(
            &client,
            tools::RUN_QUERY,
            json!({ "data_source": "cache", "query": "scan 0 match user:*", "max_rows": 2 }),
        )
        .await,
    );
    assert_eq!(r["results"][0]["kind"], "keys");
    // Closing the client side ends the bridge cleanly.
    client.cancel().await.unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(10), forwarding)
        .await
        .expect("bridge ends")
        .unwrap();
    assert_eq!(ended, Ok(()));
    eventually(|| s.agents.status().clients.is_empty()).await;
}
