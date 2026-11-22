//! Wire protocols: Anthropic Messages API (raw HTTP, SSE), OpenAI-compatible
//! Chat Completions (SSE) and Ollama's `/api/chat` (NDJSON).

use ferrodb_api::{AiProvider, ChatRole, TokenUsage};
use serde_json::{Value, json};

use super::{
    AiError,
    http::{self, Cancelled, Read},
};

pub(crate) const ANTHROPIC_VERSION: &str = "2026-10-21";
pub(crate) const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Models with a server-side refusal fallback on the official endpoint.
const FALLBACK_MODELS: &[&str] = &[
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-sonnet-5-5",
    "claude-fable-5-1",
    "claude-fable-5-2",
];
const OFFICIAL_ANTHROPIC_HOST: &str = "api.anthropic.com";
const OFFICIAL_OPENAI_HOST: &str = "api.openai.com";

/// One conversation as sent to a provider.
#[derive(Clone, Debug)]
pub(crate) struct Chat {
    pub model: String,
    pub max_tokens: u32,
    /// Stable system blocks, instructions first; the last one is the end of
    /// the cacheable prefix.
    pub system: Vec<String>,
    /// History and the new user turn, oldest first; starts with a user turn
    /// and alternates.
    pub messages: Vec<(ChatRole, String)>,
}

impl Chat {
    /// Size of the prompt in characters.
    pub fn chars(&self) -> usize {
        self.system.iter().map(|s| s.chars().count()).sum::<usize>()
            + self
                .messages
                .iter()
                .map(|(_, t)| t.chars().count())
                .sum::<usize>()
    }

    fn system_text(&self) -> String {
        self.system
            .iter()
            .filter(|s| !s.trim().is_empty())
            .cloned()
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

/// Where and how to send.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Target<'a> {
    pub provider: AiProvider,
    pub endpoint: &'a str,
    pub key: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    End,
    /// Cut off at `max_tokens`.
    MaxTokens,
    /// The model (or a safety classifier) declined; the message explains.
    Refusal(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub(crate) struct Outcome {
    pub text: String,
    pub usage: Option<TokenUsage>,
    /// Model that answered, as reported by the provider.
    pub model: Option<String>,
    pub stop: Stop,
}

fn role(r: ChatRole) -> &'static str {
    match r {
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
    }
}

fn host_is(endpoint: &str, host: &str) -> bool {
    reqwest::Url::parse(endpoint.trim())
        .ok()
        .and_then(|u| u.host_str().map(|h| h.eq_ignore_ascii_case(host)))
        .unwrap_or(false)
}

/// Streams an answer; `on_delta` receives the text as it arrives.
pub(crate) async fn stream(
    client: &reqwest::Client,
    target: Target<'_>,
    chat: &Chat,
    cancelled: Cancelled<'_>,
    on_delta: &mut (dyn FnMut(&str) + Send),
) -> Result<Outcome, AiError> {
    match target.provider {
        AiProvider::Anthropic => anthropic_stream(client, target, chat, cancelled, on_delta).await,
        AiProvider::OpenAiCompatible => {
            openai_stream(client, target, chat, cancelled, on_delta).await
        }
        AiProvider::Ollama => ollama_stream(client, target, chat, cancelled, on_delta).await,
    }
}

/// Models offered by the provider, sorted.
pub(crate) async fn models(
    client: &reqwest::Client,
    target: Target<'_>,
) -> Result<Vec<String>, AiError> {
    let mut out = match target.provider {
        AiProvider::Anthropic => anthropic_models(client, target).await?,
        AiProvider::OpenAiCompatible => {
            let v = get_json(client, &http::join(target.endpoint, "models"), target).await?;
            ids(&v, "data", "id")
        }
        AiProvider::Ollama => {
            let v = get_json(client, &http::join(target.endpoint, "api/tags"), target).await?;
            ids(&v, "models", "name")
        }
    };
    if target.provider != AiProvider::Anthropic {
        out.sort();
    }
    out.dedup();
    Ok(out)
}

/// Checks the provider; returns the model that will answer. Anthropic: a
/// tiny non-streaming request. OpenAI-compatible and Ollama: the model list
/// must offer the model (nothing is generated).
pub(crate) async fn test(
    client: &reqwest::Client,
    target: Target<'_>,
    model: &str,
) -> Result<String, AiError> {
    match target.provider {
        AiProvider::Anthropic => {
            let chat = Chat {
                model: model.to_owned(),
                max_tokens: 64,
                system: Vec::new(),
                messages: vec![(ChatRole::User, "Reply with the single word OK.".into())],
            };
            let (url, mut body, beta) = anthropic_request(target.endpoint, &chat, false);
            if body.get("output_config").is_some() {
                body["output_config"] = json!({"effort": "low"});
            }
            let resp = anthropic_post(client, &url, target, beta, &body)
                .send()
                .await
                .map_err(http::send_error)?;
            let v: Value = http::check(resp)
                .await?
                .json()
                .await
                .map_err(|e| AiError(format!("Unexpected answer from the provider: {e}")))?;
            Ok(v["model"].as_str().unwrap_or(model).to_owned())
        }
        AiProvider::OpenAiCompatible | AiProvider::Ollama => {
            let list = models(client, target).await?;
            let offered = |m: &str| m == model || m.strip_suffix(":latest") == Some(model);
            if list.is_empty() || list.iter().any(|m| offered(m)) {
                Ok(model.to_owned())
            } else {
                let mut shown: Vec<&str> = list.iter().take(8).map(String::as_str).collect();
                if list.len() > shown.len() {
                    shown.push("…");
                }
                Err(AiError(format!(
                    "The server does not offer the model `{model}` (available: {}).",
                    shown.join(", ")
                )))
            }
        }
    }
}

fn ids(v: &Value, list: &str, field: &str) -> Vec<String> {
    v[list]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m[field].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

async fn get_json(
    client: &reqwest::Client,
    url: &str,
    target: Target<'_>,
) -> Result<Value, AiError> {
    let mut req = client.get(url);
    match target.provider {
        AiProvider::Anthropic => {
            req = req.header("anthropic-version", ANTHROPIC_VERSION);
            if let Some(key) = target.key {
                req = req.header("x-api-key", key);
            }
        }
        _ => {
            if let Some(key) = target.key {
                req = req.bearer_auth(key);
            }
        }
    }
    let resp = req.send().await.map_err(http::send_error)?;
    http::check(resp)
        .await?
        .json()
        .await
        .map_err(|e| AiError(format!("Unexpected answer from the provider: {e}")))
}

// ------------------------------------------------------------------ Anthropic

/// Effort is accepted from Opus 4.5 on; older models reject it.
fn supports_effort(model: &str) -> bool {
    const OLD: &[&str] = &[
        "claude-3",
        "claude-2",
        "claude-instant",
        "claude-haiku-4",
        "claude-sonnet-4-0",
        "claude-sonnet-4-2025",
        "claude-sonnet-4-5",
        "claude-opus-4-0",
        "claude-opus-4-1",
        "claude-opus-4-2025",
    ];
    !OLD.iter().any(|p| model.starts_with(p))
}

/// URL, body and the `anthropic-beta` header (if any) of a Messages request.
///
/// The system prompt is an array of text blocks whose last one carries
/// `cache_control`, so instructions and schema form a cached prefix; with
/// history the new user turn is a second breakpoint (the conversation
/// prefix is reused by the next turn). No sampling parameters and no
/// `thinking` (current models think adaptively and reject both).
pub(crate) fn anthropic_request(
    endpoint: &str,
    chat: &Chat,
    stream: bool,
) -> (String, Value, Option<&'static str>) {
    let base = endpoint.trim().trim_end_matches('/');
    let url = if base.ends_with("/v1") {
        http::join(base, "messages")
    } else {
        http::join(base, "v1/messages")
    };
    let mut system: Vec<Value> = chat
        .system
        .iter()
        .filter(|t| !t.trim().is_empty())
        .map(|t| json!({"type": "text", "text": t}))
        .collect();
    if let Some(last) = system.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
    }
    let history = chat.messages.len() > 1;
    let last = chat.messages.len().saturating_sub(1);
    let messages: Vec<Value> = chat
        .messages
        .iter()
        .enumerate()
        .map(|(i, (r, t))| {
            if history && i == last {
                json!({"role": role(*r), "content": [
                    {"type": "text", "text": t, "cache_control": {"type": "ephemeral"}}
                ]})
            } else {
                json!({"role": role(*r), "content": t})
            }
        })
        .collect();
    let mut body = json!({
        "model": chat.model,
        "max_tokens": chat.max_tokens,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if stream {
        body["stream"] = json!(true);
    }
    if supports_effort(&chat.model) {
        body["output_config"] = json!({"effort": "medium"});
    }
    let fallback = host_is(endpoint, OFFICIAL_ANTHROPIC_HOST)
        && FALLBACK_MODELS.contains(&chat.model.as_str());
    if fallback {
        body["fallbacks"] = json!("default");
    }
    (url, body, fallback.then_some(FALLBACK_BETA))
}

fn anthropic_post(
    client: &reqwest::Client,
    url: &str,
    target: Target<'_>,
    beta: Option<&str>,
    body: &Value,
) -> reqwest::RequestBuilder {
    let mut req = client
        .post(url)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(key) = target.key {
        req = req.header("x-api-key", key);
    }
    if let Some(beta) = beta {
        req = req.header("anthropic-beta", beta);
    }
    req
}

async fn anthropic_models(
    client: &reqwest::Client,
    target: Target<'_>,
) -> Result<Vec<String>, AiError> {
    let base = target.endpoint.trim().trim_end_matches('/');
    let path = if base.ends_with("/v1") {
        "models"
    } else {
        "v1/models"
    };
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    // A handful of pages at most (the list is short).
    for _ in 0..10 {
        let mut url = format!("{}?limit=1000", http::join(base, path));
        if let Some(a) = &after {
            url.push_str("&after_id=");
            url.push_str(a);
        }
        let v = get_json(client, &url, target).await?;
        out.extend(ids(&v, "data", "id"));
        match (v["has_more"].as_bool(), v["last_id"].as_str()) {
            (Some(true), Some(last)) => after = Some(last.to_owned()),
            _ => break,
        }
    }
    Ok(out)
}

fn anthropic_usage(u: &Value, usage: &mut TokenUsage) {
    let n = |k: &str| u.get(k).and_then(Value::as_u64);
    if let Some(input) = n("input_tokens") {
        let read = n("cache_read_input_tokens").unwrap_or(0);
        let written = n("cache_creation_input_tokens").unwrap_or(0);
        usage.input = input + read + written;
        usage.cached_input = read;
    }
    if let Some(out) = n("output_tokens") {
        usage.output = out;
    }
}

fn refusal_message(details: &Value) -> String {
    let mut msg = String::from("The model declined to answer this request");
    if let Some(category) = details["category"].as_str() {
        msg.push_str(&format!(" (category: {category})"));
    }
    msg.push('.');
    if let Some(explanation) = details["explanation"].as_str().filter(|s| !s.is_empty()) {
        msg.push(' ');
        msg.push_str(explanation);
    }
    msg.push_str(" Rephrase the question or choose another model.");
    msg
}

async fn anthropic_stream(
    client: &reqwest::Client,
    target: Target<'_>,
    chat: &Chat,
    cancelled: Cancelled<'_>,
    on_delta: &mut (dyn FnMut(&str) + Send),
) -> Result<Outcome, AiError> {
    let (url, body, beta) = anthropic_request(target.endpoint, chat, true);
    let send = anthropic_post(client, &url, target, beta, &body)
        .header("accept", "text/event-stream")
        .send();
    let Some(resp) = http::unless_cancelled(send, cancelled).await else {
        return Ok(cancelled_outcome());
    };
    let resp = http::check(resp.map_err(http::send_error)?).await?;

    let mut text = String::new();
    let mut usage = TokenUsage::default();
    let mut has_usage = false;
    let mut model: Option<String> = None;
    let mut stop_reason: Option<String> = None;
    let mut stop_details = Value::Null;
    let end = http::read_lines(resp, cancelled, |line| {
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(false);
        };
        let Ok(ev) = serde_json::from_str::<Value>(data.trim()) else {
            return Ok(false);
        };
        match ev["type"].as_str().unwrap_or_default() {
            "message_start" => {
                let m = &ev["message"];
                if let Some(name) = m["model"].as_str() {
                    model = Some(name.to_owned());
                }
                if m["usage"].is_object() {
                    anthropic_usage(&m["usage"], &mut usage);
                    has_usage = true;
                }
            }
            "content_block_start" => {
                let block = &ev["content_block"];
                match block["type"].as_str() {
                    // Server-side fallback: another model continues.
                    Some("fallback") => {
                        if let Some(to) = block["to"]["model"].as_str() {
                            model = Some(to.to_owned());
                        }
                    }
                    Some("text") => {
                        if let Some(t) = block["text"].as_str().filter(|t| !t.is_empty()) {
                            text.push_str(t);
                            on_delta(t);
                        }
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = &ev["delta"];
                if delta["type"] == "text_delta"
                    && let Some(t) = delta["text"].as_str()
                {
                    text.push_str(t);
                    on_delta(t);
                }
            }
            "message_delta" => {
                if let Some(r) = ev["delta"]["stop_reason"].as_str() {
                    stop_reason = Some(r.to_owned());
                }
                if ev["delta"]["stop_details"].is_object() {
                    stop_details = ev["delta"]["stop_details"].clone();
                }
                if ev["usage"].is_object() {
                    anthropic_usage(&ev["usage"], &mut usage);
                    has_usage = true;
                }
            }
            "message_stop" => return Ok(true),
            "error" => {
                let e = &ev["error"];
                let kind = e["type"].as_str().unwrap_or("error");
                let message = e["message"].as_str().unwrap_or("unknown error");
                let what = match kind {
                    "overloaded_error" => "The AI provider is overloaded, try again later",
                    "rate_limit_error" => "Rate limited, try again later",
                    _ => "The AI provider reported an error",
                };
                return Err(AiError(format!("{what}: {message} ({kind})")));
            }
            _ => {} // ping, content_block_stop, thinking blocks…
        }
        Ok(false)
    })
    .await?;
    let stop = match end {
        Read::Cancelled => Stop::Cancelled,
        Read::Eof if stop_reason.is_none() => {
            return Err(AiError(
                "The connection closed before the answer was complete.".into(),
            ));
        }
        _ => match stop_reason.as_deref() {
            Some("refusal") => Stop::Refusal(refusal_message(&stop_details)),
            Some("max_tokens") => Stop::MaxTokens,
            _ => Stop::End,
        },
    };
    Ok(Outcome {
        text,
        usage: has_usage.then_some(usage),
        model,
        stop,
    })
}

fn cancelled_outcome() -> Outcome {
    Outcome {
        text: String::new(),
        usage: None,
        model: None,
        stop: Stop::Cancelled,
    }
}

// ------------------------------------------------------- OpenAI-compatible

pub(crate) fn openai_request(endpoint: &str, chat: &Chat) -> (String, Value) {
    let mut messages = Vec::new();
    let system = chat.system_text();
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(
        chat.messages
            .iter()
            .map(|(r, t)| json!({"role": role(*r), "content": t})),
    );
    let mut body = json!({
        "model": chat.model,
        "messages": messages,
        "stream": true,
        "stream_options": {"include_usage": true},
    });
    // OpenAI's own reasoning models reject `max_tokens`; compatible servers
    // often know only `max_tokens`.
    if host_is(endpoint, OFFICIAL_OPENAI_HOST) {
        body["max_completion_tokens"] = json!(chat.max_tokens);
    } else {
        body["max_tokens"] = json!(chat.max_tokens);
    }
    (http::join(endpoint, "chat/completions"), body)
}

async fn openai_stream(
    client: &reqwest::Client,
    target: Target<'_>,
    chat: &Chat,
    cancelled: Cancelled<'_>,
    on_delta: &mut (dyn FnMut(&str) + Send),
) -> Result<Outcome, AiError> {
    let (url, body) = openai_request(target.endpoint, chat);
    let mut req = client
        .post(&url)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(body.to_string());
    if let Some(key) = target.key {
        req = req.bearer_auth(key);
    }
    let Some(resp) = http::unless_cancelled(req.send(), cancelled).await else {
        return Ok(cancelled_outcome());
    };
    let resp = http::check(resp.map_err(http::send_error)?).await?;

    let mut text = String::new();
    let mut usage: Option<TokenUsage> = None;
    let mut model: Option<String> = None;
    let mut finish: Option<String> = None;
    let end = http::read_lines(resp, cancelled, |line| {
        let Some(data) = line.strip_prefix("data:") else {
            return Ok(false);
        };
        let data = data.trim();
        if data == "[DONE]" {
            return Ok(true);
        }
        let Ok(ev) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        if let Some(e) = ev.get("error") {
            let message = e["message"]
                .as_str()
                .or(e.as_str())
                .unwrap_or("unknown error");
            return Err(AiError(format!(
                "The AI provider reported an error: {message}"
            )));
        }
        if let Some(m) = ev["model"].as_str() {
            model = Some(m.to_owned());
        }
        let choice = &ev["choices"][0];
        if let Some(t) = choice["delta"]["content"]
            .as_str()
            .filter(|t| !t.is_empty())
        {
            text.push_str(t);
            on_delta(t);
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            finish = Some(f.to_owned());
        }
        let u = &ev["usage"];
        if u.is_object() {
            usage = Some(TokenUsage {
                input: u["prompt_tokens"].as_u64().unwrap_or(0),
                output: u["completion_tokens"].as_u64().unwrap_or(0),
                cached_input: u["prompt_tokens_details"]["cached_tokens"]
                    .as_u64()
                    .unwrap_or(0),
            });
        }
        Ok(false)
    })
    .await?;
    let stop = match end {
        Read::Cancelled => Stop::Cancelled,
        Read::Eof if finish.is_none() => {
            return Err(AiError(
                "The connection closed before the answer was complete.".into(),
            ));
        }
        _ => match finish.as_deref() {
            Some("length") => Stop::MaxTokens,
            Some("content_filter") => Stop::Refusal(
                "The provider's content filter stopped the answer. Rephrase the question.".into(),
            ),
            _ => Stop::End,
        },
    };
    Ok(Outcome {
        text,
        usage,
        model,
        stop,
    })
}

// ------------------------------------------------------------------ Ollama

pub(crate) fn ollama_request(endpoint: &str, chat: &Chat) -> (String, Value) {
    let mut messages = Vec::new();
    let system = chat.system_text();
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    messages.extend(
        chat.messages
            .iter()
            .map(|(r, t)| json!({"role": role(*r), "content": t})),
    );
    let body = json!({
        "model": chat.model,
        "messages": messages,
        "stream": true,
        "options": {"num_predict": chat.max_tokens},
    });
    (http::join(endpoint, "api/chat"), body)
}

async fn ollama_stream(
    client: &reqwest::Client,
    target: Target<'_>,
    chat: &Chat,
    cancelled: Cancelled<'_>,
    on_delta: &mut (dyn FnMut(&str) + Send),
) -> Result<Outcome, AiError> {
    let (url, body) = ollama_request(target.endpoint, chat);
    let mut req = client
        .post(&url)
        .header("content-type", "application/json")
        .body(body.to_string());
    if let Some(key) = target.key {
        req = req.bearer_auth(key);
    }
    let Some(resp) = http::unless_cancelled(req.send(), cancelled).await else {
        return Ok(cancelled_outcome());
    };
    let resp = http::check(resp.map_err(http::send_error)?).await?;

    let mut text = String::new();
    let mut usage: Option<TokenUsage> = None;
    let mut model: Option<String> = None;
    let mut done_reason: Option<String> = None;
    let end = http::read_lines(resp, cancelled, |line| {
        let Ok(ev) = serde_json::from_str::<Value>(line.trim()) else {
            return Ok(false);
        };
        if let Some(e) = ev["error"].as_str() {
            return Err(AiError(format!("Ollama reported an error: {e}")));
        }
        if let Some(m) = ev["model"].as_str() {
            model = Some(m.to_owned());
        }
        if let Some(t) = ev["message"]["content"].as_str().filter(|t| !t.is_empty()) {
            text.push_str(t);
            on_delta(t);
        }
        if ev["done"].as_bool() == Some(true) {
            done_reason = Some(ev["done_reason"].as_str().unwrap_or("stop").to_owned());
            if ev.get("prompt_eval_count").is_some() || ev.get("eval_count").is_some() {
                usage = Some(TokenUsage {
                    input: ev["prompt_eval_count"].as_u64().unwrap_or(0),
                    output: ev["eval_count"].as_u64().unwrap_or(0),
                    cached_input: 0,
                });
            }
            return Ok(true);
        }
        Ok(false)
    })
    .await?;
    let stop = match end {
        Read::Cancelled => Stop::Cancelled,
        Read::Eof if done_reason.is_none() => {
            return Err(AiError(
                "The connection closed before the answer was complete.".into(),
            ));
        }
        _ => match done_reason.as_deref() {
            Some("length") => Stop::MaxTokens,
            _ => Stop::End,
        },
    };
    Ok(Outcome {
        text,
        usage,
        model,
        stop,
    })
}
