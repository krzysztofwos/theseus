//! The agent loop: an LLM drives Theseus's operations as tools over a [`Session`].
//!
//! Each turn the model is handed the conversation and the tool catalog. It either
//! calls tools — run against the session, their results fed back — or answers and
//! ends. The session holds the working model, so the model sees its own edits. The
//! [`Llm`] port is the only thing the offline stub and a real model adapter differ
//! on; the same loop drives both.

use std::future::Future;

use serde_json::{Value, json};
use theseus::Session;

/// The most turns the loop runs before giving up.
const MAX_TURNS: usize = 16;

/// The framing handed to the model.
const SYSTEM: &str = "You are Theseus, a self-modeling tool. Inspect and edit your \
own model by calling the tools. When you are done, answer the user with a final \
text message and no tool call.";

/// One tool call the model requests.
#[derive(Clone)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The author of a conversation message.
pub enum Role {
    User,
    Assistant,
}

/// A content block within a message. A model adapter reads these to build its API
/// request; the offline stub ignores the conversation, so the fields read as
/// unused until the Anthropic adapter lands.
#[allow(dead_code)]
pub enum Block {
    Text(String),
    ToolUse(ToolUse),
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// A message in the running conversation. A model adapter reads it to build its
/// API request.
#[allow(dead_code)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// The model's reply to one turn: any text it wrote, and the tools it wants to run.
/// An empty `tool_uses` means the model is done and `text` is the final answer.
pub struct Reply {
    pub text: String,
    pub tool_uses: Vec<ToolUse>,
}

impl Reply {
    /// A final answer with no tool call.
    pub fn answer(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tool_uses: Vec::new(),
        }
    }
}

/// The model port: complete one turn from the conversation and the tool catalog.
/// The agent's offline stub and a real model adapter both implement it.
pub trait Llm {
    fn complete(
        &self,
        system: &str,
        messages: &[Message],
        tools: &[Value],
    ) -> impl Future<Output = anyhow::Result<Reply>>;
}

/// Theseus's tool catalog: its read and edit operations as JSON-schema tool
/// definitions. Hand-written for now; a later step generates these from the model.
fn tools() -> Vec<Value> {
    vec![
        json!({
            "name": "model",
            "description": "Return Theseus's model of itself as JSON.",
            "input_schema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "query",
            "description": "List model element handles, optionally filtered by `find` (a substring), `node` (an exact handle), or `kind`.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "find": { "type": "string" },
                    "node": { "type": "string" },
                    "kind": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "verify",
            "description": "Check that the workspace conforms to the model.",
            "input_schema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "coverage",
            "description": "Report which operations have no authored handler.",
            "input_schema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "patch",
            "description": "Edit the model. `edit` is a list of `verb|target|key=value` strings; `write` true reprojects to disk.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "edit": { "type": "array", "items": { "type": "string" } },
                    "write": { "type": "boolean" }
                },
                "required": ["edit"]
            }
        }),
    ]
}

/// Run the agent loop: the model calls tools against `session` until it answers.
pub async fn run_agent(
    llm: &impl Llm,
    session: &mut Session<'_>,
    message: &str,
) -> anyhow::Result<String> {
    let tools = tools();
    let mut messages = vec![Message {
        role: Role::User,
        blocks: vec![Block::Text(message.to_string())],
    }];
    for _ in 0..MAX_TURNS {
        let reply = llm.complete(SYSTEM, &messages, &tools).await?;
        if reply.tool_uses.is_empty() {
            return Ok(reply.text);
        }

        // Record the assistant's turn — any text, then the tool calls it made.
        let mut blocks = Vec::new();
        if !reply.text.is_empty() {
            blocks.push(Block::Text(reply.text));
        }
        blocks.extend(reply.tool_uses.iter().cloned().map(Block::ToolUse));
        messages.push(Message {
            role: Role::Assistant,
            blocks,
        });

        // Run each tool against the session and feed the results back. A failed
        // tool returns its error as the result, so the model can recover.
        let results = reply
            .tool_uses
            .iter()
            .map(|tool| Block::ToolResult {
                tool_use_id: tool.id.clone(),
                content: session
                    .call(&tool.name, &tool.input)
                    .unwrap_or_else(|error| format!("error: {error}")),
            })
            .collect();
        messages.push(Message {
            role: Role::User,
            blocks: results,
        });
    }
    anyhow::bail!("the agent did not finish within {MAX_TURNS} turns")
}

/// A model that replays a fixed script of replies, ignoring the conversation, so
/// the loop runs with no network. The offline stub for the binary and tests.
pub struct OfflineLlm {
    replies: std::cell::RefCell<std::collections::VecDeque<Reply>>,
}

impl OfflineLlm {
    pub fn new(replies: impl IntoIterator<Item = Reply>) -> Self {
        Self {
            replies: std::cell::RefCell::new(replies.into_iter().collect()),
        }
    }
}

impl Llm for OfflineLlm {
    async fn complete(
        &self,
        _system: &str,
        _messages: &[Message],
        _tools: &[Value],
    ) -> anyhow::Result<Reply> {
        use anyhow::Context;
        self.replies
            .borrow_mut()
            .pop_front()
            .context("the offline model ran out of replies")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use theseus::{Session, Workspace};
    use theseus_model::theseus_model;
    use theseus_modeling::GeneratedFile;

    use super::*;

    /// A workspace that writes nowhere. The loop's read-only tools never reach it.
    struct NoopWorkspace;

    impl Workspace for NoopWorkspace {
        fn write_file(&self, _file: &GeneratedFile) -> anyhow::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn the_loop_calls_a_tool_then_answers() {
        let llm = OfflineLlm::new([
            Reply {
                text: String::new(),
                tool_uses: vec![ToolUse {
                    id: "1".to_string(),
                    name: "query".to_string(),
                    input: json!({ "kind": "operation" }),
                }],
            },
            Reply::answer("Theseus exposes a verify operation."),
        ]);
        let mut session = Session::new(theseus_model(), &NoopWorkspace, false);
        let answer = run_agent(&llm, &mut session, "what can you do?")
            .await
            .expect("the loop answers");
        assert_eq!(answer, "Theseus exposes a verify operation.");
    }
}
