//! The agent loop: an LLM drives Theseus's operations as tools over a [`Session`].
//!
//! Each turn the model is handed the conversation and the tool catalog. It either
//! calls tools — run against the session, their results fed back — or answers and
//! ends. The session holds the working model, so the model sees its own edits. The
//! generated [`Llm`] port is the only thing the offline stub and a real model
//! adapter differ on; the same loop drives both, for as many turns as the modeled
//! [`TURN_BUDGET`] allows.
//!
//! One tool belongs to the loop, not the session: [`RESTART_TOOL`]. A solo call
//! ends the run as [`Outcome::Restart`], and the caller rebuilds the binary and
//! resumes the persisted transcript in the new one — the running process is the
//! thing the edits changed, so re-entry is the loop's own affordance, the way an
//! external host restarts the MCP server.

use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use theseus::Session;

use crate::generated::{Llm, TURN_BUDGET, Turn};

/// The framing handed to the model: who it is, and the working discipline the
/// tool surface expects — checkpoint before writing, prove the tree before
/// restarting, roll back a wedge.
const SYSTEM: &str = "You are Theseus, a self-modeling tool. Inspect and edit your \
own model by calling the tools. Prefer `show` for an operation's handler or an \
adapter method; `read`, `search`, and `list` reach everything else in the \
workspace. Discipline for edits that write: call `snapshot` \
before the first write and keep the returned id. After authoring, prove the tree \
— `check` for compilation, `test` when behavior changed, `verify` for conformance \
— before `restart`. If the tree wedges and you cannot repair it, `rollback` to \
your snapshot and say so. When you are done, answer the user with a final text \
message and no tool call.";

/// The loop-level tool: rebuild the binary and resume the session in it.
pub const RESTART_TOOL: &str = "restart";

/// The loop's tool list: the session's catalog, with the restart tool appended
/// when the model does not already expose it. A modeled `restart` operation
/// carries its own catalog entry, and the loop still answers the call itself
/// either way — rebuilding the running binary is this inbound's affordance.
fn loop_tools() -> Vec<Value> {
    let mut tools = theseus::tool_catalog();
    if !tools.iter().any(|tool| tool["name"] == RESTART_TOOL) {
        tools.push(restart_tool());
    }
    tools
}

/// The restart tool's definition, the fallback for a model that does not
/// expose the operation. The loop answers it itself: rebuilding the running
/// binary is this inbound's affordance.
fn restart_tool() -> Value {
    serde_json::json!({
        "name": RESTART_TOOL,
        "description": "Rebuild the agent and resume this session in the new \
    binary, whose compiled model, tool catalog, and tool dispatch match the \
    workspace — an operation the applied patch exposed becomes a callable tool. \
    Apply the edits first — `patch` with write true, `implement` each handler, \
    `check` — then call this alone, with no other tool in the turn.",
        "input_schema": { "type": "object", "properties": {} }
    })
}

/// One tool call the model requests.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: Value,
}

/// The author of a conversation message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
}

/// A content block within a message. A model adapter reads these to build its API
/// request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Block {
    Text(String),
    ToolUse(ToolUse),
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

/// A message in the running conversation. A model adapter reads it to build its
/// API request. The transcript — the message list — serializes as JSON, so a
/// session survives the rebuild that a restart runs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub blocks: Vec<Block>,
}

/// The opening transcript: one user message carrying the goal.
pub fn opening(message: &str) -> Vec<Message> {
    vec![Message {
        role: Role::User,
        blocks: vec![Block::Text(message.to_string())],
    }]
}

/// How a run of the loop ended.
pub enum Outcome {
    /// The model answered, and the conversation is complete.
    Answered(String),
    /// The turn budget ran out. The transcript carries everything the run did,
    /// so the caller can persist and inspect it.
    Exhausted(Vec<Message>),
    /// The model asked to restart. The transcript ends with the assistant turn
    /// whose sole tool call is `restart`. The caller rebuilds, persists the
    /// transcript, and re-enters the new binary, which answers the pending call
    /// through [`answer_restart`] and carries on.
    Restart(Vec<Message>),
}

/// The model's reply to one turn: any text it wrote, and the tools it wants to run.
/// An empty `tool_uses` means the model is done and `text` is the final answer.
#[derive(Clone, Debug)]
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

/// Run the agent loop: the model calls tools against `session` until it answers
/// or asks to restart. The transcript starts from [`opening`] on a fresh run, or
/// from a persisted session passed through [`answer_restart`] on a resumed one.
pub async fn run_agent(
    llm: &impl Llm,
    session: &mut Session<'_>,
    mut messages: Vec<Message>,
) -> anyhow::Result<Outcome> {
    let tools = loop_tools();
    // `AGENT_TRACE` set in the environment streams each turn's tool calls and
    // results to stderr, so a run can be watched without touching the answer.
    let trace = std::env::var("AGENT_TRACE").is_ok();
    for turn in 1..=TURN_BUDGET {
        let request = Turn {
            system: SYSTEM.to_string(),
            messages: messages.clone(),
            tools: tools.clone(),
        };
        let reply = llm.complete(&request).await?;
        if trace && !reply.text.is_empty() {
            eprintln!("[turn {turn}] say: {}", reply.text);
        }
        if reply.tool_uses.is_empty() {
            return Ok(Outcome::Answered(reply.text));
        }
        let solo_restart =
            matches!(reply.tool_uses.as_slice(), [tool] if tool.name == RESTART_TOOL);

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

        // A solo restart ends this hull's run. The pending call is answered by
        // the resumed binary, so the transcript stays one result short here.
        if solo_restart {
            if trace {
                eprintln!("[turn {turn}] restart requested");
            }
            return Ok(Outcome::Restart(messages));
        }

        // Run each tool against the session and feed the results back. A failed
        // tool returns its error as the result, so the model can recover. A
        // restart beside other calls is refused the same way, so every call in
        // the turn carries a result before the next request.
        let mut results = Vec::new();
        for tool in &reply.tool_uses {
            if trace {
                eprintln!("[turn {turn}] call {}({})", tool.name, tool.input);
            }
            let content = if tool.name == RESTART_TOOL {
                "restart must be the only tool call in its turn; finish the \
other calls, then call it alone"
                    .to_string()
            } else {
                session
                    .call(&tool.name, &tool.input)
                    .await
                    .unwrap_or_else(|error| format!("error: {error}"))
            };
            if trace {
                eprintln!("[turn {turn}]   -> {content}");
            }
            results.push(Block::ToolResult {
                tool_use_id: tool.id.clone(),
                content,
            });
        }
        messages.push(Message {
            role: Role::User,
            blocks: results,
        });
    }
    Ok(Outcome::Exhausted(messages))
}

/// Write the transcript as JSON, creating its directory.
pub fn save_transcript(path: &Path, messages: &[Message]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(messages)?;
    std::fs::write(path, json).with_context(|| format!("writing {}", path.display()))
}

/// Read a persisted transcript back.
pub fn load_transcript(path: &Path) -> anyhow::Result<Vec<Message>> {
    let json =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_str(&json).with_context(|| format!("parsing {}", path.display()))
}

/// The pending restart call a transcript ends with, when one does: the id of
/// the restart tool use in a final assistant message.
fn pending_restart(messages: &[Message]) -> Option<String> {
    messages
        .last()
        .filter(|message| matches!(message.role, Role::Assistant))
        .into_iter()
        .flat_map(|message| message.blocks.iter())
        .find_map(|block| match block {
            Block::ToolUse(tool) if tool.name == RESTART_TOOL => Some(tool.id.clone()),
            _ => None,
        })
}

/// Answer the pending restart call a persisted transcript ends with, so the
/// resumed loop's next request carries a result for every tool call. `content`
/// reports how the restart went — the rebuilt greeting, or the failure.
pub fn answer_restart(mut messages: Vec<Message>, content: &str) -> anyhow::Result<Vec<Message>> {
    let pending = pending_restart(&messages)
        .context("the transcript does not end with a pending restart call")?;
    messages.push(Message {
        role: Role::User,
        blocks: vec![Block::ToolResult {
            tool_use_id: pending,
            content: content.to_string(),
        }],
    });
    Ok(messages)
}

/// Resume a persisted transcript, whichever way its run ended. A transcript
/// ending with a pending restart call — the drydock handoff — takes
/// `restart_answer` as that call's result. An exhausted run's transcript ends
/// with its last tool results, and takes `budget_note` as a user message, so
/// the model knows its budget is renewed and picks up where it stopped.
pub fn resume(
    messages: Vec<Message>,
    restart_answer: &str,
    budget_note: &str,
) -> anyhow::Result<Vec<Message>> {
    if pending_restart(&messages).is_some() {
        return answer_restart(messages, restart_answer);
    }
    let mut messages = messages;
    messages.push(Message {
        role: Role::User,
        blocks: vec![Block::Text(budget_note.to_string())],
    });
    Ok(messages)
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use theseus::{Session, Toolchain, Workspace};
    use theseus_model::theseus_model;
    use theseus_modeling::GeneratedFile;

    use super::*;
    use crate::adapters::OfflineLlm;

    /// A workspace that writes nowhere. The loop's read-only tools never reach it.
    struct NoopWorkspace;

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {
        async fn write_file(&self, _file: &GeneratedFile) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A checkpoint on its trait defaults, for a loop that never snapshots.
    struct StubCheckpoint;

    #[async_trait::async_trait]
    impl theseus::Checkpoint for StubCheckpoint {}

    /// A toolchain that reports success without running a build, so the loop's
    /// tests stay in-process.
    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {
        async fn check(&self) -> anyhow::Result<String> {
            Ok("the workspace compiles (stub)".to_string())
        }
    }

    /// One tool call, for scripting replies.
    fn call(id: &str, name: &str, input: serde_json::Value) -> ToolUse {
        ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }
    }

    /// A reply that calls the given tools and says nothing.
    fn calls(tools: Vec<ToolUse>) -> Reply {
        Reply {
            text: String::new(),
            tool_uses: tools,
        }
    }

    #[tokio::test]
    async fn the_loop_calls_a_tool_then_answers() {
        let llm = OfflineLlm::new([
            calls(vec![call("1", "query", json!({ "kind": "operation" }))]),
            Reply::answer("Theseus exposes a verify operation."),
        ]);
        let mut session = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        );
        let outcome = run_agent(&llm, &mut session, opening("what can you do?"))
            .await
            .expect("the loop answers");
        let Outcome::Answered(answer) = outcome else {
            panic!("the loop should answer");
        };
        assert_eq!(answer, "Theseus exposes a verify operation.");
    }

    #[tokio::test]
    async fn a_solo_restart_ends_the_run_and_the_resumed_transcript_continues() {
        let llm = OfflineLlm::new([calls(vec![call("r1", RESTART_TOOL, json!({}))])]);
        let mut session = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        );
        let outcome = run_agent(&llm, &mut session, opening("restart yourself"))
            .await
            .expect("the loop runs");
        let Outcome::Restart(transcript) = outcome else {
            panic!("a solo restart should end the run as Restart");
        };

        // The transcript survives persistence, and the resumed loop picks up the
        // pending call's answer and carries on to the final text.
        let path = std::env::temp_dir().join(format!("theseus-agent-test-{}", std::process::id()));
        save_transcript(&path, &transcript).expect("the transcript saves");
        let restored = load_transcript(&path).expect("the transcript loads");
        std::fs::remove_file(&path).ok();
        let resumed =
            answer_restart(restored, "rebuilt; this is the new binary").expect("a pending call");

        let llm = OfflineLlm::new([Reply::answer("back aboard the new hull")]);
        let outcome = run_agent(&llm, &mut session, resumed)
            .await
            .expect("the resumed loop answers");
        let Outcome::Answered(answer) = outcome else {
            panic!("the resumed loop should answer");
        };
        assert_eq!(answer, "back aboard the new hull");
    }

    #[tokio::test]
    async fn a_restart_beside_other_calls_is_refused_and_the_loop_continues() {
        let llm = OfflineLlm::new([
            calls(vec![
                call("1", "query", json!({ "kind": "operation" })),
                call("r1", RESTART_TOOL, json!({})),
            ]),
            Reply::answer("done"),
        ]);
        let mut session = Session::new(
            theseus_model(),
            &NoopWorkspace,
            &StubCheckpoint,
            &theseus_calculator::Calculator,
            &StubToolchain,
            false,
        );
        let outcome = run_agent(&llm, &mut session, opening("query then restart"))
            .await
            .expect("the loop runs");
        assert!(
            matches!(outcome, Outcome::Answered(answer) if answer == "done"),
            "a mixed restart should be refused inline, not end the run",
        );
    }

    #[test]
    fn the_loop_carries_exactly_one_restart_tool() {
        let restarts = loop_tools()
            .iter()
            .filter(|tool| tool["name"] == RESTART_TOOL)
            .count();
        assert_eq!(restarts, 1);
    }

    #[test]
    fn resume_answers_a_pending_restart() {
        let transcript = vec![
            Message {
                role: Role::User,
                blocks: vec![Block::Text("go".to_string())],
            },
            Message {
                role: Role::Assistant,
                blocks: vec![Block::ToolUse(call("r1", RESTART_TOOL, json!({})))],
            },
        ];
        let resumed = resume(transcript, "rebuilt", "renewed").expect("the handoff resumes");
        let Some(Block::ToolResult {
            tool_use_id,
            content,
        }) = resumed.last().and_then(|m| m.blocks.last())
        else {
            panic!("the pending call should carry a result");
        };
        assert_eq!(tool_use_id, "r1");
        assert_eq!(content, "rebuilt");
    }

    #[test]
    fn resume_continues_an_exhausted_transcript() {
        // An exhausted run ends with the last turn's tool results.
        let transcript = vec![
            Message {
                role: Role::User,
                blocks: vec![Block::Text("go".to_string())],
            },
            Message {
                role: Role::Assistant,
                blocks: vec![Block::ToolUse(call("1", "query", json!({})))],
            },
            Message {
                role: Role::User,
                blocks: vec![Block::ToolResult {
                    tool_use_id: "1".to_string(),
                    content: "handles".to_string(),
                }],
            },
        ];
        let resumed = resume(transcript, "rebuilt", "the budget is renewed; continue")
            .expect("an exhausted run resumes");
        assert_eq!(resumed.len(), 4);
        let Some(Block::Text(note)) = resumed.last().and_then(|m| m.blocks.last()) else {
            panic!("the resumed transcript should end with the budget note");
        };
        assert_eq!(note, "the budget is renewed; continue");
    }

    #[test]
    fn answer_restart_needs_a_pending_call() {
        let error = answer_restart(opening("hello"), "rebuilt").expect_err("no pending call");
        assert!(error.to_string().contains("pending restart"));
    }
}
