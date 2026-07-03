//! The [`Llm`] port's authored adapters: the Anthropic model adapter and the
//! offline stub.
//!
//! The Anthropic adapter drives the agent loop with a real model over the
//! Messages API — it renders the conversation and the tool catalog into a
//! `/v1/messages` request and reads the reply's content blocks back into
//! [`Reply`], text and the native `tool_use` blocks the loop dispatches. The
//! offline stub replays a scripted conversation, so the loop runs with no
//! network.

use anyhow::Context;
use serde_json::{Value, json};

use crate::{
    agent::{Block, Message, Reply, Role, ToolUse},
    generated::{Llm, Turn},
};

/// A model backed by the Anthropic Messages API, configured from the environment.
pub struct AnthropicLlm {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
}

impl AnthropicLlm {
    /// Build from the environment, or `None` when `ANTHROPIC_API_KEY` is unset.
    /// `ANTHROPIC_BASE_URL` and `ANTHROPIC_MODEL` override the defaults.
    pub fn from_env() -> Option<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").ok()?;
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".to_string());
        let model =
            std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
        Some(Self {
            client: reqwest::Client::new(),
            api_key,
            base_url,
            model,
        })
    }
}

#[async_trait::async_trait]
impl Llm for AnthropicLlm {
    async fn complete(&self, request: &Turn) -> anyhow::Result<Reply> {
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": request.system,
            "tools": request.tools,
            "messages": request.messages.iter().map(api_message).collect::<Vec<_>>(),
        });
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let response = self
            .client
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("calling the Anthropic Messages API")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .context("reading the Anthropic response")?;
        if !status.is_success() {
            anyhow::bail!("the Anthropic API returned {status}: {text}");
        }
        let value: Value = serde_json::from_str(&text).context("parsing the Anthropic response")?;
        parse_reply(&value)
    }
}

/// Render one conversation message as a Messages-API message object.
fn api_message(message: &Message) -> Value {
    let role = match message.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    let content: Vec<Value> = message
        .blocks
        .iter()
        .map(|block| match block {
            Block::Text(text) => json!({ "type": "text", "text": text }),
            Block::ToolUse(tool) => {
                json!({ "type": "tool_use", "id": tool.id, "name": tool.name, "input": tool.input })
            }
            Block::ToolResult {
                tool_use_id,
                content,
            } => json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content }),
        })
        .collect();
    json!({ "role": role, "content": content })
}

/// Read the model's reply from the response: the text it wrote and the tools it
/// wants to run. Content block types other than `text` and `tool_use` are ignored.
fn parse_reply(value: &Value) -> anyhow::Result<Reply> {
    let content = value
        .get("content")
        .and_then(Value::as_array)
        .context("the Anthropic response has no content")?;
    let mut text = String::new();
    let mut tool_uses = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(part) = block.get("text").and_then(Value::as_str) {
                    text.push_str(part);
                }
            }
            Some("tool_use") => tool_uses.push(ToolUse {
                id: string_field(block, "id"),
                name: string_field(block, "name"),
                input: block.get("input").cloned().unwrap_or(Value::Null),
            }),
            _ => {}
        }
    }
    Ok(Reply { text, tool_uses })
}

fn string_field(block: &Value, key: &str) -> String {
    block
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// A model that replays a fixed script of replies, ignoring the conversation, so
/// the loop runs with no network. The offline stub for the binary and tests.
pub struct OfflineLlm {
    replies: std::sync::Mutex<std::collections::VecDeque<Reply>>,
}

impl OfflineLlm {
    pub fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: std::sync::Mutex::new(replies.into_iter().collect()),
        }
    }
}

#[async_trait::async_trait]
impl Llm for OfflineLlm {
    async fn complete(&self, _request: &Turn) -> anyhow::Result<Reply> {
        use anyhow::Context;
        self.replies
            .lock()
            .expect("the offline script lock is not poisoned")
            .pop_front()
            .context("the offline model ran out of replies")
    }
}
