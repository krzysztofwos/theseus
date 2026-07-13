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

use std::{
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

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
before the first write and keep the returned id pinned. Never call `release` or \
`prune`; snapshot retention belongs to the operator. After authoring, and before \
either restart or your final answer, prove the tree \
after the last write. A successful writing tool already compile-gates its result, \
and a later successful `test` proves compilation too; otherwise call `check`. \
Call `test` when behavior changed and `verify` for conformance. If the tree \
wedges and you cannot repair it, `rollback` to your snapshot and say so. When \
you are done, answer the user with a final text message and no tool call.";

/// The loop-level tool: rebuild the binary and resume the session in it.
pub const RESTART_TOOL: &str = "restart";

const OPERATOR_ONLY_TOOLS: [&str; 2] = ["release", "prune"];
const OPERATOR_ONLY_RESULT: &str = "error: snapshot retention is operator-owned; \
this active agent run cannot release or prune checkpoints";

fn operator_only_tool(name: &str) -> bool {
    OPERATOR_ONLY_TOOLS.contains(&name)
}

/// The loop's tool list: the session's catalog, with the restart tool appended
/// when the model does not already expose it. A modeled `restart` operation
/// carries its own catalog entry, and the loop still answers the call itself
/// either way — rebuilding the running binary is this inbound's affordance.
fn loop_tools() -> Vec<Value> {
    let mut tools = theseus::tool_catalog();
    tools.retain(|tool| !tool["name"].as_str().is_some_and(operator_only_tool));
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
    Apply and compile-gate the edits, test behavioral changes, and verify \
    conformance, then call this alone with no other tool in the turn.",
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

/// One turn's request: the framing, the conversation, and the tools offered.
pub fn turn(messages: Vec<Message>, tools: Vec<Value>) -> Turn {
    Turn {
        system: SYSTEM.to_string(),
        messages,
        tools,
    }
}

/// Record the assistant's turn — any text, then the tool calls it made.
pub fn push_assistant(messages: &mut Vec<Message>, reply: Reply) {
    let mut blocks = Vec::new();
    if !reply.text.is_empty() {
        blocks.push(Block::Text(reply.text));
    }
    blocks.extend(reply.tool_uses.into_iter().map(Block::ToolUse));
    messages.push(Message {
        role: Role::Assistant,
        blocks,
    });
}

/// Record one result per tool call, so the next request is complete.
pub fn push_results(messages: &mut Vec<Message>, results: Vec<(String, String)>) {
    messages.push(Message {
        role: Role::User,
        blocks: results
            .into_iter()
            .map(|(tool_use_id, content)| Block::ToolResult {
                tool_use_id,
                content,
            })
            .collect(),
    });
}

/// The most turns the bootstrap phase runs before giving up, the loop's budget.
pub fn phase_budget() -> usize {
    TURN_BUDGET
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
    /// The model port failed mid-run. The transcript carries everything up to
    /// the failure, so a resume replays nothing.
    Interrupted {
        transcript: Vec<Message>,
        error: anyhow::Error,
    },
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
    for turn_number in 1..=TURN_BUDGET {
        let turn = turn_number;
        let reply = match llm
            .complete(&crate::agent::turn(messages.clone(), tools.clone()))
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                return Ok(Outcome::Interrupted {
                    transcript: messages,
                    error,
                });
            }
        };
        if trace && !reply.text.is_empty() {
            eprintln!("[turn {turn}] say: {}", reply.text);
        }
        if reply.tool_uses.is_empty() {
            return Ok(Outcome::Answered(reply.text));
        }
        let solo_restart =
            matches!(reply.tool_uses.as_slice(), [tool] if tool.name == RESTART_TOOL);
        let calls = reply.tool_uses.clone();
        push_assistant(&mut messages, reply);

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
        for tool in &calls {
            if trace {
                eprintln!("[turn {turn}] call {}({})", tool.name, tool.input);
            }
            let content = if tool.name == RESTART_TOOL {
                "restart must be the only tool call in its turn; finish the \
other calls, then call it alone"
                    .to_string()
            } else if operator_only_tool(&tool.name) {
                OPERATOR_ONLY_RESULT.to_string()
            } else {
                session
                    .call(&tool.name, &tool.input)
                    .await
                    .unwrap_or_else(|error| format!("error: {error}"))
            };
            if trace {
                eprintln!("[turn {turn}]   -> {content}");
            }
            results.push((tool.id.clone(), content));
        }
        push_results(&mut messages, results);
    }
    Ok(Outcome::Exhausted(messages))
}

static NEXT_TRANSCRIPT_WRITE: AtomicU64 = AtomicU64::new(0);
const MAX_TRANSCRIPT_BYTES: u64 = 16 * 1024 * 1024;

/// Atomically write the transcript as private JSON, creating its directory.
pub fn save_transcript(path: &Path, messages: &[Message]) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("transcript path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    set_private_control_directory(parent)?;
    let json = serde_json::to_string_pretty(messages)?;
    let nonce = NEXT_TRANSCRIPT_WRITE.fetch_add(1, Ordering::Relaxed);
    let temporary = parent.join(format!(".session-{}-{nonce}.tmp", std::process::id()));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        set_private_file(&file)?;
        file.write_all(json.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("publishing {}", path.display()))?;
        sync_directory(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Read a persisted transcript back.
pub fn load_transcript(path: &Path) -> anyhow::Result<Vec<Message>> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("transcript path has no parent: {}", path.display()))?;
    set_private_control_directory(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && transcript_has_one_link(&metadata),
        "transcript {} is not a private regular file",
        path.display()
    );
    anyhow::ensure!(
        metadata.len() <= MAX_TRANSCRIPT_BYTES,
        "transcript {} is larger than {} bytes",
        path.display(),
        MAX_TRANSCRIPT_BYTES
    );
    set_private_file(&file)?;
    let mut json = String::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_TRANSCRIPT_BYTES + 1)
        .read_to_string(&mut json)
        .with_context(|| format!("reading {}", path.display()))?;
    anyhow::ensure!(
        json.len() as u64 <= MAX_TRANSCRIPT_BYTES,
        "transcript {} grew beyond {} bytes while it was read",
        path.display(),
        MAX_TRANSCRIPT_BYTES
    );
    serde_json::from_str(&json).with_context(|| format!("parsing {}", path.display()))
}

#[cfg(unix)]
fn set_private_control_directory(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if path.file_name().is_some_and(|name| name == ".theseus") {
        let metadata = std::fs::symlink_metadata(path)?;
        anyhow::ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "transcript directory {} is not a real directory",
            path.display()
        );
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_private_control_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file(file: &std::fs::File) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file(_file: &std::fs::File) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn transcript_has_one_link(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    metadata.nlink() == 1
}

#[cfg(not(unix))]
fn transcript_has_one_link(_metadata: &std::fs::Metadata) -> bool {
    true
}

fn sync_directory(path: &Path) -> anyhow::Result<()> {
    std::fs::File::open(path)?.sync_all()?;
    Ok(())
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;
    use theseus::{Session, Toolchain, Workspace};

    use super::*;
    use crate::adapters::OfflineLlm;

    fn project() -> theseus::ProjectContext {
        theseus::theseus_project().expect("Theseus project context is valid")
    }

    /// A workspace that writes nowhere. The loop's read-only tools never reach it.
    struct NoopWorkspace;

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {}

    /// A checkpoint on its trait defaults, for a loop that never snapshots.
    struct StubCheckpoint;

    #[async_trait::async_trait]
    impl theseus::Checkpoint for StubCheckpoint {}

    struct LifecycleCheckpoint {
        project: theseus::ProjectContext,
        releases: AtomicUsize,
        prunes: AtomicUsize,
    }

    impl LifecycleCheckpoint {
        fn new(project: theseus::ProjectContext) -> Self {
            Self {
                project,
                releases: AtomicUsize::new(0),
                prunes: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl theseus::Checkpoint for LifecycleCheckpoint {
        async fn context(&self) -> anyhow::Result<theseus::ProjectContext> {
            Ok(self.project.clone())
        }

        async fn release(&self, _request: &str) -> anyhow::Result<String> {
            self.releases.fetch_add(1, Ordering::Relaxed);
            Ok("released".to_string())
        }

        async fn prune(&self, _request: &theseus::SnapshotRetention) -> anyhow::Result<String> {
            self.prunes.fetch_add(1, Ordering::Relaxed);
            Ok("pruned".to_string())
        }
    }

    /// A toolchain that reports success without running a build, so the loop's
    /// tests stay in-process.
    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {
        async fn check(&self) -> anyhow::Result<theseus::CheckReport> {
            Ok(theseus::CheckReport::success(
                "the workspace compiles (stub)",
            ))
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
    async fn a_model_failure_keeps_the_transcript() {
        // The script runs dry after one turn, so the second request fails the
        // way a network or billing failure would.
        let llm = OfflineLlm::new([calls(vec![call(
            "1",
            "query",
            json!({ "kind": "operation" }),
        )])]);
        let project = theseus::theseus_project().expect("the harness project opens");
        let workspace = theseus::FsWorkspace::for_project(&project);
        let checkpoint = theseus::GitCheckpoint::for_project(project.clone());
        let toolchain = theseus::CargoToolchain::for_project(&project);
        let mut session = theseus::Session::new(
            project,
            &workspace,
            &checkpoint,
            &theseus_calculator::Calculator,
            &toolchain,
            false,
        );
        let outcome = run_agent(&llm, &mut session, opening("go"))
            .await
            .expect("an interrupted run is an outcome, not an error");
        let Outcome::Interrupted { transcript, error } = outcome else {
            panic!("the dry script should interrupt the run");
        };
        assert!(error.to_string().contains("out of replies"), "{error}");
        // The transcript holds the completed turn: goal, call, and result.
        assert_eq!(transcript.len(), 3);
    }

    #[tokio::test]
    async fn the_loop_calls_a_tool_then_answers() {
        let llm = OfflineLlm::new([
            calls(vec![call("1", "query", json!({ "kind": "operation" }))]),
            Reply::answer("Theseus exposes a verify operation."),
        ]);
        let mut session = Session::new(
            project(),
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
            project(),
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

    #[cfg(unix)]
    #[test]
    fn persisted_transcripts_are_private() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let nonce = NEXT_TRANSCRIPT_WRITE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "theseus-private-transcript-{}-{nonce}",
            std::process::id()
        ));
        let path = root.join(".theseus/session.json");
        save_transcript(&path, &opening("private prompt")).expect("the transcript saves");

        assert_eq!(
            std::fs::metadata(root.join(".theseus"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let hardlink = root.join(".theseus/session-hardlink.json");
        std::fs::hard_link(&path, &hardlink).unwrap();
        let error = load_transcript(&path).expect_err("hardlinked transcripts are refused");
        assert!(error.to_string().contains("private regular file"));
        std::fs::remove_file(hardlink).unwrap();

        let link = root.join(".theseus/session-link.json");
        symlink(&path, &link).unwrap();
        assert!(load_transcript(&link).is_err());
        std::fs::remove_dir_all(root).ok();
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
            project(),
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
        let tools = loop_tools();
        let restarts = tools
            .iter()
            .filter(|tool| tool["name"] == RESTART_TOOL)
            .count();
        assert_eq!(restarts, 1);
        for operator_only in OPERATOR_ONLY_TOOLS {
            assert!(
                tools.iter().all(|tool| tool["name"] != operator_only),
                "{operator_only} must stay outside the active agent catalog",
            );
        }
    }

    #[test]
    fn the_catalog_teaches_foreign_project_repairs() {
        let tools = loop_tools();
        let find = |name: &str| {
            tools
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap_or_else(|| panic!("the catalog lacks {name}"))
        };

        let patch = find("patch");
        assert!(
            patch["description"]
                .as_str()
                .is_some_and(|description| description.contains("not `input`/`output`")),
            "{}",
            patch["description"]
        );
        assert!(
            patch["description"]
                .as_str()
                .is_some_and(|description| description.contains("`tool` for CLI-only")),
            "{}",
            patch["description"]
        );
        assert!(
            patch["input_schema"]["properties"]["edit"]["items"]["oneOf"][0]["properties"]["attrs"]
                ["description"]
                .as_str()
                .is_some_and(|description| description.contains("`request` and `response`")),
            "{}",
            patch["input_schema"]
        );
        assert!(
            find("edit_rust_item")["description"]
                .as_str()
                .is_some_and(|description| description.contains("Cargo.toml"))
        );
        assert!(
            find("read")["description"]
                .as_str()
                .is_some_and(|description| description.contains("call `list` with `{}` first"))
        );
    }

    #[tokio::test]
    async fn operator_only_checkpoint_calls_are_refused_before_dispatch() {
        for (name, input) in [
            ("release", json!({ "reference": "snapshot-id" })),
            ("prune", json!({ "keep": 0 })),
        ] {
            let llm = OfflineLlm::new([
                calls(vec![call("lifecycle", name, input)]),
                Reply::answer("the operator owns retention"),
            ]);
            let project = project();
            let checkpoint = LifecycleCheckpoint::new(project.clone());
            let mut session = Session::new(
                project,
                &NoopWorkspace,
                &checkpoint,
                &theseus_calculator::Calculator,
                &StubToolchain,
                true,
            );

            let outcome = run_agent(&llm, &mut session, opening("clean up checkpoints"))
                .await
                .expect("the loop refuses the lifecycle call and continues");

            assert!(
                matches!(outcome, Outcome::Answered(answer) if answer == "the operator owns retention")
            );
            assert_eq!(checkpoint.releases.load(Ordering::Relaxed), 0);
            assert_eq!(checkpoint.prunes.load(Ordering::Relaxed), 0);
        }
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
