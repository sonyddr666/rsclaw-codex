//! Local Codex proxy provider.
//!
//! This adapts rsclaw to an external Codex bridge such as the user's
//! codex-llm / llm-codex service. rsclaw only talks to a local HTTP/SSE
//! adapter and does not handle ChatGPT session secrets itself.
//!
//! Default target:
//! - base URL: http://127.0.0.1:8787
//! - stream path: /api/chat/sse
//!
//! Env overrides:
//! - CODEX_PROXY_BASE_URL
//! - CODEX_PROXY_SSE_PATH

use std::sync::Arc;

use anyhow::{Context, Result};
use futures::{StreamExt, future::BoxFuture};
use serde_json::{Value, json};

use super::{
    ContentPart, LlmProvider, LlmRequest, LlmStream, Message, MessageContent, Role, StreamEvent,
    TokenUsage,
};

pub(crate) const CODEX_PROXY_DEFAULT_BASE: &str = "http://127.0.0.1:8787";
const CODEX_PROXY_DEFAULT_SSE_PATH: &str = "/api/chat/sse";

pub struct CodexProxyProvider {
    client: reqwest::Client,
    base_url: String,
    sse_path: String,
}

impl CodexProxyProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_user_agent(base_url, None)
    }

    pub fn with_user_agent(base_url: impl Into<String>, user_agent: Option<String>) -> Self {
        let base_url = std::env::var("CODEX_PROXY_BASE_URL").unwrap_or_else(|_| base_url.into());
        let sse_path = std::env::var("CODEX_PROXY_SSE_PATH")
            .unwrap_or_else(|_| CODEX_PROXY_DEFAULT_SSE_PATH.to_owned());

        Self {
            client: super::http_client_with_ua(user_agent.as_deref()),
            base_url,
            sse_path,
        }
    }

    async fn stream_proxy(&self, req: &LlmRequest) -> Result<LlmStream> {
        let body = build_proxy_body(req);
        let url = format!(
            "{}{}",
            self.base_url.trim_end_matches('/'),
            normalize_path(&self.sse_path)
        );

        tracing::info!(
            model = %req.model,
            url = %url,
            tools_count = req.tools.len(),
            "codex-proxy: forwarding request"
        );

        let resp = super::send_with_transport_retry(
            self.client
                .post(&url)
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .json(&body),
        )
        .await
        .context("Codex proxy request failed")?;

        let status = resp.status();
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();

        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Codex proxy error {status}: {body}");
        }

        if content_type.contains("json") && !content_type.contains("text/event-stream") {
            let body: Value = resp.json().await.context("Codex proxy: parse JSON response")?;
            let events = parse_json_response(&body);
            return Ok(Box::pin(futures::stream::iter(events)) as LlmStream);
        }

        let line_buffer = Arc::new(tokio::sync::Mutex::new(String::new()));
        let event_stream = resp
            .bytes_stream()
            .then(move |chunk| {
                let line_buffer = line_buffer.clone();
                async move {
                    let chunk = chunk.map_err(|e| anyhow::anyhow!("stream read error: {e}"));
                    parse_sse_chunk(chunk, &line_buffer).await
                }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(event_stream) as LlmStream)
    }
}

impl LlmProvider for CodexProxyProvider {
    fn name(&self) -> &str {
        "codex-proxy"
    }

    fn stream(&self, req: LlmRequest) -> BoxFuture<'_, Result<LlmStream>> {
        Box::pin(async move { self.stream_proxy(&req).await })
    }
}

fn build_proxy_body(req: &LlmRequest) -> Value {
    let messages = request_messages(req);
    let last_user_message = messages
        .iter()
        .rev()
        .find(|m| m.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let mut body = json!({
        "model": req.model,
        "stream": true,
        "message": last_user_message,
        "messages": messages,
    });

    if let Some(system) = &req.system {
        body["system"] = json!(system);
        body["instructions"] = json!(system);
    }

    if let Some(max_tokens) = req.max_tokens
        && max_tokens > 0
    {
        body["max_tokens"] = json!(max_tokens);
        body["max_output_tokens"] = json!(max_tokens);
    }

    if let Some(temp) = req.temperature {
        body["temperature"] = super::json_f32(temp);
    }

    if !req.tools.is_empty() {
        body["tools"] = json!(
            req.tools
                .iter()
                .map(|tool| json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }))
                .collect::<Vec<Value>>()
        );
    }

    body
}

fn request_messages(req: &LlmRequest) -> Vec<Value> {
    let mut messages = Vec::new();

    if let Some(system) = &req.system {
        messages.push(json!({ "role": "system", "content": system }));
    }

    for message in &req.messages {
        messages.push(message_to_proxy_message(message));
    }

    messages
}

fn message_to_proxy_message(message: &Message) -> Value {
    json!({
        "role": role_to_str(&message.role),
        "content": content_to_text(&message.content),
        "parts": content_to_parts(&message.content),
    })
}

fn content_to_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.clone()),
                ContentPart::ToolResult { content, .. } => Some(content.clone()),
                ContentPart::Image { url } => Some(format!("[image: {url}]")),
                ContentPart::ToolUse { name, input, .. } => Some(format!("[tool_call: {name} {input}]")),
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn content_to_parts(content: &MessageContent) -> Value {
    match content {
        MessageContent::Text(text) => json!([{ "type": "text", "text": text }]),
        MessageContent::Parts(parts) => json!(
            parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text { text } => json!({ "type": "text", "text": text }),
                    ContentPart::Image { url } => json!({ "type": "image", "url": url }),
                    ContentPart::ToolUse { id, name, input } => json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }),
                    ContentPart::ToolResult { tool_use_id, content, is_error } => json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                        "is_error": is_error,
                    }),
                })
                .collect::<Vec<Value>>()
        ),
    }
}

fn role_to_str(role: &Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

async fn parse_sse_chunk(
    chunk: Result<bytes::Bytes>,
    line_buffer: &Arc<tokio::sync::Mutex<String>>,
) -> Vec<Result<StreamEvent>> {
    let bytes = match chunk {
        Ok(bytes) => bytes,
        Err(e) => return vec![Err(e)],
    };

    let text = String::from_utf8_lossy(&bytes);
    let mut buffer = line_buffer.lock().await;
    buffer.push_str(&text);

    let mut events = Vec::new();
    while let Some(pos) = buffer.find('\n') {
        let raw_line: String = buffer.drain(..=pos).collect();
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with(':') || line.starts_with("event:") {
            continue;
        }

        let data = line.strip_prefix("data:").map(str::trim).unwrap_or(line);
        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        if let Ok(value) = serde_json::from_str::<Value>(data) {
            events.extend(parse_json_event(&value));
        } else {
            events.push(Ok(StreamEvent::TextDelta(data.to_owned())));
        }
    }

    events
}

fn parse_json_response(value: &Value) -> Vec<Result<StreamEvent>> {
    let mut events = Vec::new();
    if let Some(text) = extract_text(value) {
        events.push(Ok(StreamEvent::TextDelta(text)));
    }
    events.push(Ok(StreamEvent::Done { usage: usage_from_value(value) }));
    events
}

fn parse_json_event(value: &Value) -> Vec<Result<StreamEvent>> {
    let event_type = value
        .get("type")
        .or_else(|| value.get("event"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    if matches!(event_type, "done" | "complete" | "completed" | "response.completed") {
        return vec![Ok(StreamEvent::Done { usage: usage_from_value(value) })];
    }

    if matches!(event_type, "error" | "response.failed") {
        return vec![Ok(StreamEvent::Error(
            value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("Codex proxy stream error")
                .to_owned(),
        ))];
    }

    if matches!(
        event_type,
        "response.output_text.delta" | "text_delta" | "delta" | "message"
    ) {
        if let Some(text) = extract_text(value) {
            return vec![Ok(StreamEvent::TextDelta(text))];
        }
    }

    if let Some(tool_call) = parse_tool_call(value) {
        return vec![Ok(tool_call)];
    }

    extract_text(value)
        .map(|text| vec![Ok(StreamEvent::TextDelta(text))])
        .unwrap_or_default()
}

fn extract_text(value: &Value) -> Option<String> {
    let paths = [
        "/delta",
        "/text",
        "/content",
        "/message",
        "/response/output_text",
        "/output/0/content/0/text",
        "/choices/0/delta/content",
        "/choices/0/message/content",
    ];

    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn parse_tool_call(value: &Value) -> Option<StreamEvent> {
    let item = value.get("item").unwrap_or(value);
    let kind = item.get("type").and_then(Value::as_str).unwrap_or_default();
    if kind != "function_call" && kind != "tool_call" {
        return None;
    }

    let name = item.get("name").and_then(Value::as_str)?.to_owned();
    let id = item
        .get("call_id")
        .or_else(|| item.get("id"))
        .and_then(Value::as_str)
        .unwrap_or("call_codex")
        .to_owned();
    let arguments = item
        .get("arguments")
        .or_else(|| item.get("input"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let input = if let Some(s) = arguments.as_str() {
        serde_json::from_str(s).unwrap_or_else(|_| json!({ "raw": s }))
    } else {
        arguments
    };

    Some(StreamEvent::ToolCall { id, name, input })
}

fn usage_from_value(value: &Value) -> Option<TokenUsage> {
    let usage = value.get("usage").or_else(|| value.pointer("/response/usage"))?;
    let input = usage
        .get("input_tokens")
        .or_else(|| usage.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let output = usage
        .get("output_tokens")
        .or_else(|| usage.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    Some(TokenUsage { input, output })
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}
