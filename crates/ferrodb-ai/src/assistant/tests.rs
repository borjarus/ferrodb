//! Tests without network access: a local HTTP/1.1 server answers with canned
//! responses per provider; context building runs on the mock backend.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use ferrodb_api::{
    AiProvider, AiSettings, AssistantEvent, AssistantHandle, AssistantRequest, AssistantService,
    AssistantTask, ChatRole, ChatTurn, DataSourceId, QueryLanguage, RiskKind,
};
use ferrodb_mock::MockBackend;
use ferrodb_secrets::{MemoryStore, SecretStore};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

use super::{
    Assistant, answer, context,
    providers::{self, Chat},
};
use crate::AiOptions;

// ------------------------------------------------------------ test server

struct Canned {
    status: u16,
    content_type: &'static str,
    chunks: Vec<String>,
    /// Keep the connection open after the last chunk (cancellation).
    hold: bool,
}

impl Canned {
    fn new(status: u16, content_type: &'static str, chunks: Vec<String>) -> Self {
        Self {
            status,
            content_type,
            chunks,
            hold: false,
        }
    }
    fn json(status: u16, body: Value) -> Self {
        Self::new(status, "application/json", vec![body.to_string()])
    }
}

#[derive(Clone, Debug)]
struct Captured {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

impl Captured {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).expect("JSON body")
    }
}

struct Server {
    url: String,
    requests: Arc<Mutex<Vec<Captured>>>,
    /// Set when a held connection was closed by the client.
    closed: Arc<tokio::sync::Notify>,
}

impl Server {
    fn requests(&self) -> Vec<Captured> {
        self.requests.lock().unwrap().clone()
    }
}

async fn read_request(sock: &mut TcpStream) -> Captured {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let head_end = loop {
        let n = sock.read(&mut tmp).await.expect("read request");
        assert!(n > 0, "client closed before the request was complete");
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break p;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default().to_owned();
    let path = first.next().unwrap_or_default().to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < len {
        let n = sock.read(&mut tmp).await.expect("read body");
        assert!(n > 0);
        body.extend_from_slice(&tmp[..n]);
    }
    Captured {
        method,
        path,
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    }
}

/// Answers the n-th connection with the n-th canned response.
async fn serve(responses: Vec<Canned>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(tokio::sync::Notify::new());
    let (reqs, closed2) = (requests.clone(), closed.clone());
    tokio::spawn(async move {
        let mut responses = responses.into_iter();
        while let Ok((mut sock, _)) = listener.accept().await {
            let Some(canned) = responses.next() else {
                return;
            };
            let (reqs, closed) = (reqs.clone(), closed2.clone());
            tokio::spawn(async move {
                let captured = read_request(&mut sock).await;
                reqs.lock().unwrap().push(captured);
                let head = format!(
                    "HTTP/1.1 {} Canned\r\ncontent-type: {}\r\nconnection: close\r\n\r\n",
                    canned.status, canned.content_type
                );
                if sock.write_all(head.as_bytes()).await.is_err() {
                    return;
                }
                for chunk in &canned.chunks {
                    if sock.write_all(chunk.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = sock.flush().await;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                if canned.hold {
                    // Wait for the client to go away (read returns 0).
                    let mut b = [0u8; 64];
                    let gone = tokio::time::timeout(Duration::from_secs(30), async {
                        loop {
                            match sock.read(&mut b).await {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    })
                    .await;
                    if gone.is_ok() {
                        closed.notify_one();
                    }
                }
                let _ = sock.shutdown().await;
            });
        }
    });
    Server {
        url,
        requests,
        closed,
    }
}

fn sse(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            format!(
                "event: {}\ndata: {}\n\n",
                e["type"].as_str().unwrap_or("message"),
                e
            )
        })
        .collect()
}

// ------------------------------------------------------------ helpers

fn shop() -> Option<DataSourceId> {
    Some(DataSourceId::new("shop-dev"))
}

async fn assistant(
    provider: AiProvider,
    endpoint: &str,
    model: &str,
    key: Option<&str>,
) -> Arc<Assistant> {
    let a = Assistant::new(
        Arc::new(MockBackend::deterministic()),
        AiOptions::in_memory(),
    );
    a.loaded().await;
    a.save_settings(
        AiSettings {
            enabled: true,
            provider,
            endpoint: endpoint.into(),
            model: model.into(),
            ..AiSettings::default()
        },
        key.map(Into::into),
    )
    .await
    .unwrap();
    a
}

async fn events(h: AssistantHandle) -> Vec<AssistantEvent> {
    tokio::time::timeout(Duration::from_secs(30), async move {
        let mut out = Vec::new();
        while let Ok(e) = h.events.recv().await {
            let end = matches!(e, AssistantEvent::Finished(_) | AssistantEvent::Failed(_));
            out.push(e);
            if end {
                break;
            }
        }
        out
    })
    .await
    .expect("assistant events in time")
}

fn deltas(events: &[AssistantEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|e| match e {
            AssistantEvent::Delta(d) => Some(d.clone()),
            _ => None,
        })
        .collect()
}

fn failure(events: &[AssistantEvent]) -> String {
    match events.last() {
        Some(AssistantEvent::Failed(m)) => m.clone(),
        other => panic!("expected Failed, got {other:?}"),
    }
}

fn anthropic_ok_stream(stop: &str) -> Vec<String> {
    sse(&[
        json!({"type": "message_start", "message": {"id": "msg_1", "type": "message", "role": "assistant",
            "model": "claude-opus-5-5", "content": [], "stop_reason": null,
            "usage": {"input_tokens": 12, "cache_read_input_tokens": 900, "cache_creation_input_tokens": 0, "output_tokens": 1}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hidden"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "ping"}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "Here is the query:\n"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "```sql\nSELECT * FROM orders;\n```\n"}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "It lists orders."}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": 42}}),
        json!({"type": "message_stop"}),
    ])
}

// ------------------------------------------------------------ Anthropic

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_streams_deltas_usage_and_queries() {
    // Split one event across two chunks: lines must be reassembled.
    let mut chunks = anthropic_ok_stream("end_turn");
    let split = chunks.remove(6);
    let (a, b) = split.split_at(split.len() / 2);
    chunks.insert(6, b.to_owned());
    chunks.insert(6, a.to_owned());
    let server = serve(vec![Canned::new(200, "text/event-stream", chunks)]).await;
    let a = assistant(AiProvider::Anthropic, &server.url, "", Some("sk-test")).await;
    let mut req = AssistantRequest::generate(shop(), "list all orders");
    req.schema = Some("public".into());
    let ev = events(a.ask(req)).await;

    let AssistantEvent::Context { objects, chars } = &ev[0] else {
        panic!("Context first, got {:?}", ev[0]);
    };
    assert_eq!(objects.first().map(String::as_str), Some("public.orders"));
    assert!(*chars > 100);
    assert_eq!(
        deltas(&ev),
        vec![
            "Here is the query:\n",
            "```sql\nSELECT * FROM orders;\n```\n",
            "It lists orders."
        ]
    );
    let Some(AssistantEvent::Finished(answer)) = ev.last() else {
        panic!("expected Finished, got {:?}", ev.last());
    };
    assert_eq!(
        answer.text,
        "Here is the query:\n```sql\nSELECT * FROM orders;\n```\nIt lists orders."
    );
    assert_eq!(answer.model, "claude-opus-5-5");
    let usage = answer.usage.expect("usage");
    assert_eq!(
        (usage.input, usage.cached_input, usage.output),
        (912, 900, 42)
    );
    assert_eq!(answer.queries.len(), 1);
    let q = &answer.queries[0];
    assert_eq!(q.text, "SELECT * FROM orders;");
    assert_eq!(q.language, QueryLanguage::sql("postgresql"));
    assert!(!q.modifies && q.risks.is_empty());

    // The request.
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("POST", "/v1/messages")
    );
    assert_eq!(r.headers["x-api-key"], "sk-test");
    assert_eq!(r.headers["anthropic-version"], "2026-10-21");
    assert_eq!(r.headers["content-type"], "application/json");
    assert!(!r.headers.contains_key("anthropic-beta"));
    let body = r.json();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["stream"], true);
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["output_config"]["effort"], "medium");
    for absent in ["temperature", "top_p", "top_k", "thinking", "fallbacks"] {
        assert!(body.get(absent).is_none(), "{absent} must not be sent");
    }
    let system = body["system"].as_array().expect("system blocks");
    assert_eq!(system.len(), 2);
    assert!(system[0].get("cache_control").is_none());
    assert_eq!(system[1]["cache_control"]["type"], "ephemeral");
    assert!(system[0]["text"].as_str().unwrap().contains("PostgreSQL"));
    assert!(
        system[1]["text"]
            .as_str()
            .unwrap()
            .contains("public.orders")
    );
    // The question is not in the cached prefix.
    assert!(
        !system
            .iter()
            .any(|b| b["text"].as_str().unwrap().contains("list all orders"))
    );
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    assert!(
        messages[0]["content"]
            .as_str()
            .unwrap()
            .contains("list all orders")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_max_tokens_finishes_with_a_note() {
    let server = serve(vec![Canned::new(
        200,
        "text/event-stream",
        anthropic_ok_stream("max_tokens"),
    )])
    .await;
    let a = assistant(AiProvider::Anthropic, &server.url, "", Some("k")).await;
    let ev = events(a.ask(AssistantRequest::generate(shop(), "orders"))).await;
    let Some(AssistantEvent::Finished(answer)) = ev.last() else {
        panic!("expected Finished, got {:?}", ev.last());
    };
    assert!(answer.text.contains("cut off"));
    assert!(deltas(&ev).last().unwrap().contains("cut off"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_refusal_fails_with_the_category() {
    let chunks = sse(&[
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5", "usage": {"input_tokens": 5, "output_tokens": 0}}}),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Partial"}}),
        json!({"type": "message_delta", "delta": {"stop_reason": "refusal",
            "stop_details": {"type": "refusal", "category": "cyber", "explanation": null}}, "usage": {"output_tokens": 3}}),
        json!({"type": "message_stop"}),
    ]);
    let server = serve(vec![Canned::new(200, "text/event-stream", chunks)]).await;
    let a = assistant(AiProvider::Anthropic, &server.url, "", Some("k")).await;
    let ev = events(a.ask(AssistantRequest::generate(shop(), "orders"))).await;
    assert_eq!(deltas(&ev), vec!["Partial"]);
    let msg = failure(&ev);
    assert!(msg.contains("declined") && msg.contains("cyber"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_http_errors_are_explained() {
    let server = serve(vec![
        Canned::json(
            401,
            json!({"type": "error", "error": {"type": "authentication_error", "message": "invalid x-api-key"}}),
        ),
        Canned::json(
            529,
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ),
    ])
    .await;
    let a = assistant(AiProvider::Anthropic, &server.url, "", Some("bad")).await;
    let msg = failure(&events(a.ask(AssistantRequest::generate(shop(), "orders"))).await);
    assert!(msg.contains("Invalid API key (HTTP 401)"), "{msg}");
    assert!(msg.contains("invalid x-api-key"), "{msg}");
    let msg = failure(&events(a.ask(AssistantRequest::generate(shop(), "orders"))).await);
    assert!(msg.contains("overloaded") && msg.contains("529"), "{msg}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelling_stops_the_stream() {
    let chunks = sse(&[
        json!({"type": "message_start", "message": {"model": "claude-opus-5-5", "usage": {"input_tokens": 5, "output_tokens": 0}}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "First"}}),
    ]);
    let mut canned = Canned::new(200, "text/event-stream", chunks);
    canned.hold = true;
    let server = serve(vec![canned]).await;
    let a = assistant(AiProvider::Anthropic, &server.url, "", Some("k")).await;
    let h = a.ask(AssistantRequest::generate(shop(), "orders"));
    let rx = h.events.clone();
    let wait = async {
        loop {
            match rx.recv().await.expect("event") {
                AssistantEvent::Delta(d) => break d,
                AssistantEvent::Context { .. } => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    };
    let first = tokio::time::timeout(Duration::from_secs(20), wait)
        .await
        .unwrap();
    assert_eq!(first, "First");
    h.cancel.cancel();
    let next = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("ends quickly after cancel")
        .unwrap();
    assert_eq!(next, AssistantEvent::Failed("Cancelled.".into()));
    // The connection was dropped.
    tokio::time::timeout(Duration::from_secs(5), server.closed.notified())
        .await
        .expect("connection closed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_models_and_test() {
    let server = serve(vec![
        Canned::json(
            200,
            json!({"data": [{"id": "claude-opus-5-5", "type": "model"}, {"id": "claude-sonnet-5-5", "type": "model"}],
                "has_more": false, "first_id": "claude-opus-5-5", "last_id": "claude-sonnet-5-5"}),
        ),
        Canned::json(
            200,
            json!({"id": "msg_1", "type": "message", "role": "assistant", "model": "claude-opus-5-5",
                "content": [{"type": "text", "text": "OK"}], "stop_reason": "end_turn",
                "usage": {"input_tokens": 9, "output_tokens": 2}}),
        ),
    ])
    .await;
    let a = assistant(AiProvider::Anthropic, &server.url, "", Some("sk-m")).await;
    assert_eq!(
        a.models().await.unwrap(),
        vec!["claude-opus-5-5", "claude-sonnet-5-5"]
    );
    assert_eq!(a.test().await.unwrap(), "claude-opus-5-5");
    let reqs = server.requests();
    assert_eq!(reqs[0].method, "GET");
    assert!(reqs[0].path.starts_with("/v1/models"), "{}", reqs[0].path);
    assert_eq!(reqs[0].headers["x-api-key"], "sk-m");
    assert_eq!(reqs[0].headers["anthropic-version"], "2026-10-21");
    assert_eq!(reqs[1].path, "/v1/messages");
    let body = reqs[1].json();
    assert!(body.get("stream").is_none());
    assert_eq!(body["max_tokens"], 64);
    assert!(body.get("temperature").is_none());
}

fn chat(model: &str, history: bool) -> Chat {
    let mut messages = Vec::new();
    if history {
        messages.push((ChatRole::User, "first".to_owned()));
        messages.push((ChatRole::Assistant, "answer".to_owned()));
    }
    messages.push((ChatRole::User, "question".to_owned()));
    Chat {
        model: model.into(),
        max_tokens: 100,
        system: vec!["instructions".into(), "schema".into()],
        messages,
    }
}

#[test]
fn fallbacks_only_on_the_official_endpoint() {
    let (url, body, beta) = providers::anthropic_request(
        "https://api.anthropic.com",
        &chat("claude-opus-5-5", false),
        true,
    );
    assert_eq!(url, "https://api.anthropic.com/v1/messages");
    assert_eq!(body["fallbacks"], "default");
    assert_eq!(beta, Some("server-side-fallback-2026-07-01"));

    let (_, body, beta) = providers::anthropic_request(
        "https://api.anthropic.com/",
        &chat("claude-haiku-5-5", false),
        true,
    );
    assert!(body.get("fallbacks").is_none() && beta.is_none());

    let (url, body, beta) = providers::anthropic_request(
        "https://proxy.example/v1/",
        &chat("claude-opus-5-5", false),
        true,
    );
    assert_eq!(url, "https://proxy.example/v1/messages");
    assert!(body.get("fallbacks").is_none() && beta.is_none());

    // Models before Opus 4.5 reject `effort`.
    let (_, body, _) = providers::anthropic_request(
        "https://api.anthropic.com",
        &chat("claude-haiku-4-5", false),
        true,
    );
    assert!(body.get("output_config").is_none());
}

#[test]
fn history_adds_a_cache_breakpoint_on_the_new_turn() {
    let (_, body, _) =
        providers::anthropic_request("http://localhost", &chat("claude-opus-5-5", true), true);
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["content"], "first");
    assert_eq!(messages[2]["content"][0]["text"], "question");
    assert_eq!(
        messages[2]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    let system = body["system"].as_array().unwrap();
    assert_eq!(system[1]["cache_control"]["type"], "ephemeral");
}

#[test]
fn history_is_normalised() {
    let ctx = context_without_source();
    let mut req = AssistantRequest::generate(None, "next");
    req.history = vec![
        ChatTurn {
            role: ChatRole::Assistant,
            text: "hello".into(),
        },
        ChatTurn {
            role: ChatRole::User,
            text: "a".into(),
        },
        ChatTurn {
            role: ChatRole::User,
            text: "b".into(),
        },
        ChatTurn {
            role: ChatRole::Assistant,
            text: " ".into(),
        },
        ChatTurn {
            role: ChatRole::Assistant,
            text: "c".into(),
        },
    ];
    let chat = super::prompt::chat(&req, &ctx, "m".into(), 10);
    let roles: Vec<ChatRole> = chat.messages.iter().map(|(r, _)| *r).collect();
    assert_eq!(
        roles,
        vec![ChatRole::User, ChatRole::Assistant, ChatRole::User]
    );
    assert_eq!(chat.messages[0].1, "a\n\nb");
}

// ------------------------------------------------------------ OpenAI / Ollama

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_compatible_streams() {
    let data = |v: Value| format!("data: {v}\n\n");
    let chunks = vec![
        data(
            json!({"id": "c1", "model": "gpt-x", "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]}),
        ),
        data(
            json!({"id": "c1", "model": "gpt-x", "choices": [{"index": 0, "delta": {"content": "```sql\nDELETE FROM t\n```"}}]}),
        ),
        data(
            json!({"id": "c1", "model": "gpt-x", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
        ),
        data(json!({"id": "c1", "model": "gpt-x", "choices": [],
            "usage": {"prompt_tokens": 50, "completion_tokens": 7, "prompt_tokens_details": {"cached_tokens": 10}}})),
        "data: [DONE]\n\n".to_owned(),
    ];
    let server = serve(vec![
        Canned::new(200, "text/event-stream", chunks),
        Canned::json(
            200,
            json!({"object": "list", "data": [{"id": "b-model"}, {"id": "a-model"}]}),
        ),
    ])
    .await;
    let endpoint = format!("{}/v1", server.url);
    let a = assistant(
        AiProvider::OpenAiCompatible,
        &endpoint,
        "gpt-x",
        Some("sk-o"),
    )
    .await;
    let ev = events(a.ask(AssistantRequest::generate(
        shop(),
        "delete everything from t",
    )))
    .await;
    let Some(AssistantEvent::Finished(answer)) = ev.last() else {
        panic!("expected Finished, got {:?}", ev.last());
    };
    assert_eq!(deltas(&ev), vec!["```sql\nDELETE FROM t\n```"]);
    assert_eq!(answer.model, "gpt-x");
    let usage = answer.usage.unwrap();
    assert_eq!((usage.input, usage.output, usage.cached_input), (50, 7, 10));
    assert!(answer.queries[0].modifies);
    assert!(answer.queries[0].risks.contains(&RiskKind::DeleteAll));
    assert_eq!(a.models().await.unwrap(), vec!["a-model", "b-model"]);

    let reqs = server.requests();
    assert_eq!(reqs[0].path, "/v1/chat/completions");
    assert_eq!(reqs[0].headers["authorization"], "Bearer sk-o");
    let body = reqs[0].json();
    assert_eq!(body["stream"], true);
    assert_eq!(body["model"], "gpt-x");
    assert_eq!(body["max_tokens"], 16_000);
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(reqs[1].path, "/v1/models");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ollama_streams_ndjson() {
    let line = |v: Value| format!("{v}\n");
    let chunks = vec![
        line(
            json!({"model": "llama3", "message": {"role": "assistant", "content": "Hi "}, "done": false}),
        ),
        line(
            json!({"model": "llama3", "message": {"role": "assistant", "content": "there"}, "done": false}),
        ),
        line(
            json!({"model": "llama3", "message": {"role": "assistant", "content": ""}, "done": true,
            "done_reason": "stop", "prompt_eval_count": 30, "eval_count": 5}),
        ),
    ];
    let tags = json!({"models": [{"name": "qwen:7b"}, {"name": "llama3:latest"}]});
    let server = serve(vec![
        Canned::new(200, "application/x-ndjson", chunks),
        Canned::json(200, tags.clone()),
        Canned::json(200, tags),
    ])
    .await;
    let a = assistant(AiProvider::Ollama, &server.url, "llama3", None).await;
    let ev = events(a.ask(AssistantRequest::generate(None, "hello"))).await;
    let Some(AssistantEvent::Finished(answer)) = ev.last() else {
        panic!("expected Finished, got {:?}", ev.last());
    };
    assert_eq!(answer.text, "Hi there");
    assert_eq!(answer.usage.unwrap().input, 30);
    assert_eq!(answer.usage.unwrap().output, 5);
    assert_eq!(a.models().await.unwrap(), vec!["llama3:latest", "qwen:7b"]);
    assert_eq!(a.test().await.unwrap(), "llama3");

    let reqs = server.requests();
    assert_eq!(reqs[0].path, "/api/chat");
    assert!(!reqs[0].headers.contains_key("authorization"));
    let body = reqs[0].json();
    assert_eq!(body["stream"], true);
    assert_eq!(body["options"]["num_predict"], 16_000);
    assert_eq!(reqs[1].path, "/api/tags");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_fails_clearly_without_settings() {
    let a = Assistant::new(
        Arc::new(MockBackend::deterministic()),
        AiOptions::in_memory(),
    );
    a.loaded().await;
    let msg = failure(&events(a.ask(AssistantRequest::generate(shop(), "x"))).await);
    assert!(msg.contains("enable it in Settings"), "{msg}");

    let a = assistant(AiProvider::OpenAiCompatible, "http://127.0.0.1:9", "", None).await;
    let msg = failure(&events(a.ask(AssistantRequest::generate(shop(), "x"))).await);
    assert!(msg.contains("model"), "{msg}");

    let a = assistant(AiProvider::Anthropic, "http://127.0.0.1:9", "", None).await;
    let msg = failure(&events(a.ask(AssistantRequest::generate(shop(), "x"))).await);
    assert!(msg.contains("API key"), "{msg}");
}

// ------------------------------------------------------------ context

fn context_without_source() -> context::SourceContext {
    context::SourceContext {
        data_source: None,
        source_name: None,
        product: None,
        data_model: None,
        language: QueryLanguage::sql("generic"),
        joins: true,
        console_schema: None,
        shared: true,
        schema_text: None,
        objects: Vec::new(),
    }
}

fn settings(budget: usize, share: bool) -> AiSettings {
    AiSettings {
        enabled: true,
        share_schema: share,
        schema_budget: budget,
        ..AiSettings::default()
    }
}

async fn build(ds: &str, prompt: &str, settings: &AiSettings) -> context::SourceContext {
    let backend = MockBackend::deterministic();
    let mut req = AssistantRequest::generate(Some(DataSourceId::new(ds)), prompt);
    req.schema = (ds == "shop-dev").then(|| "public".to_owned());
    context::build(&backend, &req, settings, &|| false).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_ranks_relevant_tables_first_within_budget() {
    let ctx = build(
        "shop-dev",
        "total of order items per supplier",
        &settings(1000, true),
    )
    .await;
    let text = ctx.schema_text.as_deref().expect("schema");
    assert!(
        text.chars().count() <= 1000,
        "{} chars",
        text.chars().count()
    );
    assert_eq!(ctx.objects[0], "public.order_items", "{:?}", ctx.objects);
    assert!(
        ctx.objects.iter().take(3).any(|o| o == "public.suppliers"),
        "{:?}",
        ctx.objects
    );
    assert!(
        text.starts_with("table public.order_items(id Int64"),
        "{text}"
    );
    // Keys come from the tree; the unknown referencing column stays apart.
    assert!(text.contains("fk_customer→customers(id)"), "{text}");
    assert!(text.contains("Not shown in detail:"), "{text}");
    assert_eq!(ctx.product.as_deref(), Some("PostgreSQL"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_names_from_the_console_query_come_first() {
    let backend = MockBackend::deterministic();
    let mut req = AssistantRequest::generate(shop(), "why is this slow");
    req.task = AssistantTask::Optimize;
    req.context = Some("SELECT * FROM public.warehouses w WHERE w.city = 'X'".into());
    let ctx = context::build(&backend, &req, &settings(100_000, true), &|| false).await;
    assert_eq!(ctx.objects[0], "public.warehouses");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_includes_everything_that_fits() {
    let ctx = build("shop-dev", "anything", &settings(1_000_000, true)).await;
    let text = ctx.schema_text.unwrap();
    assert!(!text.contains("Not shown in detail"));
    // 3 schemas × (24 tables + 1 view), deduplicated across databases.
    assert_eq!(ctx.objects.len(), 75, "{:?}", ctx.objects);
    assert!(
        text.contains(
            "Functions and procedures: public.calculate_total(order_id bigint) → numeric"
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_without_sharing_sends_no_names() {
    let ctx = build("shop-dev", "orders per customer", &settings(24_000, false)).await;
    assert!(ctx.schema_text.is_none() && ctx.objects.is_empty());
    let system = super::prompt::system_blocks(&ctx).join("\n");
    assert!(system.contains("PostgreSQL"));
    assert!(system.contains("does not share the schema"));
    for name in ["customers", "order_items", "public"] {
        assert!(!system.contains(name), "{name} leaked");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn context_for_documents_and_keys() {
    let ctx = build("catalog", "latest reviews", &settings(24_000, true)).await;
    assert_eq!(ctx.language, QueryLanguage::Mql);
    assert_eq!(ctx.objects[0], "catalog.reviews");
    let text = ctx.schema_text.clone().unwrap();
    assert!(
        text.contains("collection catalog.reviews(_id objectId, name string"),
        "{text}"
    );
    let system = super::prompt::system_blocks(&ctx).join("\n");
    assert!(system.contains("db.orders.find"));

    let ctx = build("cache", "sessions", &settings(24_000, true)).await;
    let text = ctx.schema_text.unwrap();
    assert!(text.starts_with("Key prefixes in db0: "), "{text}");
    assert!(ctx.objects.iter().all(|o| o.starts_with("db0 ")));

    let ctx = build("events", "events of a user", &settings(24_000, true)).await;
    assert_eq!(ctx.language, QueryLanguage::Cql);
    assert!(super::prompt::system_blocks(&ctx)[0].contains("partition key"));
}

#[test]
fn identifiers_split_and_singularise() {
    assert_eq!(
        context::words("orderItems_HTTPServer"),
        vec!["order", "items", "http", "server"]
    );
    assert_eq!(context::stem("categories"), "category");
    assert_eq!(context::stem("boxes"), "box");
    assert_eq!(context::stem("orders"), "order");
    assert_eq!(context::stem("status"), "status");
    assert_eq!(context::stem("address"), "address");
}

// ------------------------------------------------------------ answers

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn code_blocks_become_checked_queries() {
    let text = "Delete all:\n```sql\nDELETE FROM t\n```\nThen:\n~~~\nSELECT * FROM t\n~~~\n\
                ```python\nprint(1)\n```\nUnfinished:\n```sql\nSELECT";
    let blocks = answer::code_blocks(text);
    assert_eq!(blocks.len(), 3);
    assert_eq!(blocks[2].tag, "python");
    let backend = MockBackend::deterministic();
    let mut ctx = context_without_source();
    ctx.language = QueryLanguage::sql("postgresql");
    let queries = answer::queries(&backend, text, &ctx).await;
    assert_eq!(queries.len(), 2);
    assert_eq!(queries[0].text, "DELETE FROM t");
    assert!(queries[0].modifies);
    assert_eq!(queries[0].risks, vec![RiskKind::DeleteAll]);
    assert!(!queries[1].modifies && queries[1].risks.is_empty());

    ctx.language = QueryLanguage::Mql;
    let queries = answer::queries(
        &backend,
        "```javascript\ndb.orders.deleteMany({})\n```",
        &ctx,
    )
    .await;
    assert!(queries[0].modifies, "{queries:?}");
}

// ------------------------------------------------------------ settings

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let store: Arc<dyn SecretStore> = Arc::new(MemoryStore::new());
    let options = AiOptions {
        config_dir: Some(dir.path().to_owned()),
        data_dir: None,
        secrets: store.clone(),
    };
    let backend = Arc::new(MockBackend::deterministic());
    let a = Assistant::new(backend.clone(), options.clone());
    a.loaded().await;
    assert_eq!(a.settings(), AiSettings::default());

    let s = AiSettings {
        enabled: true,
        provider: AiProvider::OpenAiCompatible,
        endpoint: "http://localhost:1234/v1".into(),
        model: "qwen".into(),
        max_tokens: 1234,
        share_schema: false,
        share_samples: false,
        schema_budget: 999,
        has_key: false,
    };
    a.save_settings(s.clone(), Some("sk-secret".into()))
        .await
        .unwrap();
    assert!(a.settings().has_key);
    let file = std::fs::read_to_string(dir.path().join("ai.toml")).unwrap();
    assert!(
        !file.contains("sk-secret") && !file.contains("has_key"),
        "{file}"
    );
    assert_eq!(
        store.get("secret://ai-openai-compatible").unwrap()["api_key"],
        "sk-secret"
    );

    let b = Assistant::new(backend.clone(), options.clone());
    b.loaded().await;
    assert_eq!(
        b.settings(),
        AiSettings {
            has_key: true,
            ..s.clone()
        }
    );

    // `None` keeps the key, `Some("")` removes it.
    b.save_settings(s.clone(), None).await.unwrap();
    assert!(b.settings().has_key);
    b.save_settings(s.clone(), Some(String::new()))
        .await
        .unwrap();
    assert!(!b.settings().has_key);
    assert!(
        store
            .get("secret://ai-openai-compatible")
            .unwrap()
            .is_empty()
    );

    // The key belongs to the provider.
    b.save_settings(s.clone(), Some("k1".into())).await.unwrap();
    let other = AiSettings {
        provider: AiProvider::Anthropic,
        ..s
    };
    b.save_settings(other, None).await.unwrap();
    assert!(!b.settings().has_key);
}

// ------------------------------------------------------------ live

/// `ANTHROPIC_API_KEY=… cargo test -p ferrodb-ai live_anthropic -- --ignored`
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs ANTHROPIC_API_KEY and network"]
async fn live_anthropic() {
    let Ok(key) = std::env::var("ANTHROPIC_API_KEY") else {
        return;
    };
    let a = assistant(AiProvider::Anthropic, "", "", Some(&key)).await;
    assert!(a.test().await.unwrap().starts_with("claude"));
    let ev = events(a.ask(AssistantRequest::generate(
        shop(),
        "10 most recent orders with customer names",
    )))
    .await;
    let Some(AssistantEvent::Finished(answer)) = ev.last() else {
        panic!("expected Finished, got {:?}", ev.last());
    };
    assert!(!answer.queries.is_empty(), "{}", answer.text);
    assert!(answer.usage.is_some());
}
