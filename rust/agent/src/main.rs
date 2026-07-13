//! The Theseus agent — the Agent inbound adapter (L4).
//!
//! An LLM drives Theseus's own operations as tools over a [`Session`], so Theseus
//! modifies itself. The same `Session` an external host drives over MCP is driven
//! here by an internal model. This entry point wires the model adapter and the
//! filesystem workspace and runs the loop over a single message.
//!
//! The loop's restart tool sails the session across a rebuild: the transcript
//! persists to `.theseus/session.json`, the workspace rebuilds, and this process
//! replaces itself with the new binary, which resumes the conversation with
//! `--resume`. A failed rebuild feeds the compiler's output back into the running
//! loop, so the model can repair the workspace from the old binary.
//!
//! With `--init`, an empty operator-selected root is initialized by the loop
//! itself before the session opens: the model sees one tool, `initialize`, and
//! chooses only the project's identity — the root comes from `--project` and
//! the engine's path from this binary's own workspace, so the bootstrap stays
//! inside the same boundaries every later edit obeys.

mod adapters;
mod agent;
mod generated;

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use adapters::{AnthropicLlm, OfflineLlm};
use agent::{
    Message, Outcome, Reply, answer_restart, load_transcript, opening, resume, run_agent,
    save_transcript,
};
use generated::Llm;
use serde_json::{Value, json};
use theseus::{
    CargoToolchain, FsWorkspace, GitCheckpoint, ProjectContext, Session, initialize_project,
    theseus_project,
};
use theseus_modeling::ProjectId;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let options = parse_args()?;

    // A real model when the API key is set; the offline stub otherwise, so the
    // binary runs with no network and the no-key path is obvious.
    match AnthropicLlm::from_env() {
        Some(llm) => run(&llm, options).await,
        None => {
            eprintln!("ANTHROPIC_API_KEY is unset; answering offline without tools");
            let llm = OfflineLlm::new([Reply::answer(
                "set ANTHROPIC_API_KEY to drive Theseus's tools with a real model",
            )]);
            run(&llm, options).await
        }
    }
}

/// Open the project, run the loop to its answer, and clean up the transcript.
/// With `--init`, the loop's bootstrap phase seeds the empty root first.
async fn run(llm: &impl Llm, options: Options) -> anyhow::Result<()> {
    let mut messages = match &options.mode {
        Mode::Start(message) => opening(message),
        Mode::Resume => Vec::new(),
    };

    if options.init {
        let root = options
            .project
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("--init initializes the root `--project` selects"))?;
        anyhow::ensure!(
            options.mode != Mode::Resume,
            "--init starts a fresh conversation; drop --resume"
        );
        anyhow::ensure!(
            !root.join("theseus.json").exists(),
            "{} is already initialized; drop --init",
            root.display()
        );
        if let Some(answer) = initialize_phase(llm, root, &mut messages).await? {
            println!("{answer}");
            return Ok(());
        }
    }

    let project = match &options.project {
        Some(root) => ProjectContext::open(root)?,
        None => theseus_project()?,
    };
    let workspace = FsWorkspace::for_project(&project);
    let checkpoint = GitCheckpoint::for_project(project.clone());
    let calculator = theseus_calculator::Calculator;
    let toolchain = CargoToolchain::for_project(&project);
    let mut session = Session::new(
        project.clone(),
        &workspace,
        &checkpoint,
        &calculator,
        &toolchain,
        options.allow_writes,
    );

    if options.mode == Mode::Resume {
        messages = resume(
            load_transcript(&session_path(&project))?,
            "rebuilt; this is the new binary, and its compiled model and tool \
catalog match the workspace",
            "the turn budget was spent and has been renewed; continue where you \
stopped, and finish with your answer",
        )?;
    }

    let answer = drive(llm, &mut session, messages, &project, &options).await?;
    // The conversation is complete, so a persisted transcript has served its
    // purpose and a later `--resume` should not find it.
    std::fs::remove_file(session_path(&project)).ok();
    println!("{answer}");
    Ok(())
}

/// The loop's bootstrap phase: over one tool, `initialize`, the model chooses
/// the project's identity and the empty root is seeded transactionally. The
/// phase ends when initialization succeeds — the conversation carries on into
/// the opened project — or when the model answers, which is the final answer.
/// A refused identity feeds its diagnostic back, so the model can repair it.
async fn initialize_phase(
    llm: &impl Llm,
    root: &Path,
    messages: &mut Vec<Message>,
) -> anyhow::Result<Option<String>> {
    let tools = vec![initialize_tool()];
    let trace = std::env::var("AGENT_TRACE").is_ok();
    for _ in 1..=agent::phase_budget() {
        let reply = llm
            .complete(&agent::turn(messages.clone(), tools.clone()))
            .await?;
        if reply.tool_uses.is_empty() {
            return Ok(Some(reply.text));
        }
        let calls = reply.tool_uses.clone();
        agent::push_assistant(messages, reply);
        let mut initialized = false;
        let mut results = Vec::new();
        for call in &calls {
            let content = if call.name == "initialize" && !initialized {
                match initialize_named(root, &call.input).await {
                    Ok(id) => {
                        initialized = true;
                        format!(
                            "initialized project `{id}`; the full tool catalog is now available"
                        )
                    }
                    Err(error) => format!("error: {error:#}"),
                }
            } else {
                "only `initialize` is available until the project is initialized".to_string()
            };
            if trace {
                eprintln!("[init] call {}({}) -> {content}", call.name, call.input);
            }
            results.push((call.id.clone(), content));
        }
        agent::push_results(messages, results);
        if initialized {
            return Ok(None);
        }
    }
    anyhow::bail!("the model did not initialize the project within the phase budget")
}

/// Seed the root with the identity the call names, through the transactional
/// initializer, with the engine taken from this binary's own workspace.
async fn initialize_named(root: &Path, input: &Value) -> anyhow::Result<String> {
    let id = input
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("`initialize` takes an `id` string"))?;
    let project_id = ProjectId::new(id)?;
    initialize_project(root, project_id, harness_modeling_path()).await?;
    Ok(id.to_string())
}

/// The engine crate this binary was built beside, the seed's one dependency.
fn harness_modeling_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the agent crate lives at <root>/rust/agent")
        .join("modeling")
}

/// The bootstrap phase's one tool: the model supplies only the identity.
fn initialize_tool() -> Value {
    json!({
        "name": "initialize",
        "description": "Initialize the empty project at the operator-selected root: write \
    its durable model record, manifest, workspace, and compile-checked seed, then continue \
    this conversation inside it with the full tool catalog. `id` is the project's stable \
    identity — lowercase words separated by hyphens, e.g. `notes-app`. One-time.",
        "input_schema": { "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }
    })
}

/// Run the loop to its final answer, sailing through any restart it asks for.
/// A successful rebuild persists the transcript and replaces this process with
/// the new binary. A failed one answers the restart call with the compiler's
/// output and keeps the loop running in the old binary.
async fn drive(
    llm: &impl Llm,
    session: &mut Session<'_>,
    mut messages: Vec<Message>,
    project: &ProjectContext,
    options: &Options,
) -> anyhow::Result<String> {
    loop {
        match run_agent(llm, session, messages).await? {
            Outcome::Answered(text) => return Ok(text),
            Outcome::Exhausted(transcript) => {
                save_transcript(&session_path(project), &transcript)?;
                anyhow::bail!(
                    "the agent did not finish within its turn budget; continue it \
with `agent{} --resume` (the transcript is saved at {})",
                    project_argument(options),
                    session_path(project).display()
                );
            }
            Outcome::Interrupted { transcript, error } => {
                save_transcript(&session_path(project), &transcript)?;
                return Err(error.context(format!(
                    "the model port failed mid-run; continue with `agent{} --resume` \
(the transcript is saved at {})",
                    project_argument(options),
                    session_path(project).display()
                )));
            }
            Outcome::Restart(transcript) => match rebuild(project).await {
                Ok(executable) => {
                    save_transcript(&session_path(project), &transcript)?;
                    return Err(resume_exec(&executable, project, options));
                }
                Err(diagnostics) => {
                    messages =
                        answer_restart(transcript, &format!("rebuild failed:\n{diagnostics}"))?;
                }
            },
        }
    }
}

/// Build the agent and its dependency graph, returning the compiler's output on
/// failure. The child dies with a dropped future.
async fn rebuild(project: &ProjectContext) -> Result<PathBuf, String> {
    let harness = theseus_project().map_err(|error| error.to_string())?;
    harness
        .ensure_same_project(project)
        .map_err(|error| format!("restart only rebuilds the Theseus harness project: {error}"))?;
    let root = project.root();
    let lease = theseus::FsMutation::begin_async(root.to_path_buf(), Vec::new())
        .await
        .map_err(|error| format!("locking the workspace for rebuild: {error}"))?;
    let output = tokio::process::Command::new("cargo")
        .args([
            "build",
            "-p",
            "theseus-agent",
            "--locked",
            "--message-format=json-render-diagnostics",
        ])
        .current_dir(root)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("running `cargo build`: {error}"))?;
    if output.status.success() {
        let executable = cargo_artifact(&output.stdout).ok_or_else(|| {
            "cargo succeeded without reporting the rebuilt agent executable".to_string()
        })?;
        lease
            .commit()
            .map_err(|error| format!("finishing the rebuild lease: {error}"))?;
        return Ok(executable);
    }
    let diagnostics = cargo_diagnostics(&output.stdout, &output.stderr);
    Err(theseus::head(diagnostics.trim()))
}

fn cargo_artifact(stdout: &[u8]) -> Option<PathBuf> {
    let mut artifacts = stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| message["target"]["name"] == "agent")
        .filter_map(|message| message["executable"].as_str().map(PathBuf::from));
    artifacts.next_back()
}

fn cargo_diagnostics(stdout: &[u8], stderr: &[u8]) -> String {
    let mut diagnostics: Vec<String> = stdout
        .split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter_map(|message| message["message"]["rendered"].as_str().map(str::to_owned))
        .collect();
    let stderr = String::from_utf8_lossy(stderr);
    if !stderr.trim().is_empty() {
        diagnostics.push(stderr.into_owned());
    }
    diagnostics.join("\n")
}

/// Replace this process with a fresh run of the agent, resuming the persisted
/// session in the newly built binary. Returns only on failure to launch.
fn resume_exec(executable: &Path, project: &ProjectContext, options: &Options) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new(executable);
    if options.allow_writes {
        command.arg("--allow-writes");
    }
    if let Some(project) = &options.project {
        command.arg("--project").arg(project);
    }
    command.arg("--resume").current_dir(project.root());
    anyhow::Error::new(command.exec()).context("re-entering the rebuilt agent")
}

/// The persisted transcript's path, in the workspace's scratch directory.
fn session_path(project: &ProjectContext) -> PathBuf {
    project.root().join(".theseus/session.json")
}

/// How the agent starts: a fresh conversation over a message, or a resumed one
/// over the persisted transcript.
#[derive(Debug, Eq, PartialEq)]
enum Mode {
    Start(String),
    Resume,
}

struct Options {
    mode: Mode,
    allow_writes: bool,
    project: Option<PathBuf>,
    init: bool,
}

fn project_argument(options: &Options) -> String {
    options
        .project
        .as_ref()
        .map(|path| format!(" --project {path:?}"))
        .unwrap_or_default()
}

/// Parse `agent [--project ROOT] [--allow-writes] <message>` or a resume.
fn parse_args() -> anyhow::Result<Options> {
    parse_args_from(std::env::args().skip(1))
}

fn parse_args_from(args: impl IntoIterator<Item = String>) -> anyhow::Result<Options> {
    let mut allow_writes = false;
    let mut init = false;
    let mut resume = false;
    let mut message = None;
    let mut project = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--allow-writes" if allow_writes => {
                anyhow::bail!("--allow-writes was supplied more than once")
            }
            "--allow-writes" => allow_writes = true,
            "--init" if init => anyhow::bail!("--init was supplied more than once"),
            "--init" => init = true,
            "--resume" if resume => anyhow::bail!("--resume was supplied more than once"),
            "--resume" => resume = true,
            "--project" => {
                anyhow::ensure!(project.is_none(), "--project was supplied more than once");
                let root = args
                    .next()
                    .filter(|value| !value.is_empty() && !value.starts_with("--"))
                    .ok_or_else(|| anyhow::anyhow!("--project requires a root path"))?;
                project = Some(PathBuf::from(root));
            }
            flag if flag.starts_with("--") => {
                anyhow::bail!(
                    "unknown flag `{flag}`; the flags are --project, --init, --allow-writes, and --resume"
                )
            }
            _ if message.is_some() => anyhow::bail!("the agent accepts exactly one goal string"),
            _ => message = Some(arg),
        }
    }
    let mode = match (resume, message) {
        (true, None) => Mode::Resume,
        (false, Some(message)) => Mode::Start(message),
        (true, Some(_)) => {
            anyhow::bail!("--resume continues the persisted session; drop the message")
        }
        (false, None) => {
            anyhow::bail!(
                "usage: agent [--project ROOT] [--allow-writes] <message> | agent [--project ROOT] [--allow-writes] --resume"
            )
        }
    };
    Ok(Options {
        mode,
        allow_writes,
        project,
        init,
    })
}

#[cfg(test)]
mod bootstrap_tests {
    use super::*;
    use agent::{Block, Role};

    fn scripted(replies: impl IntoIterator<Item = Reply>) -> OfflineLlm {
        OfflineLlm::new(replies)
    }

    fn call(id: &str, name: &str, input: Value) -> agent::ToolUse {
        agent::ToolUse {
            id: id.to_string(),
            name: name.to_string(),
            input,
        }
    }

    fn calls(tools: Vec<agent::ToolUse>) -> Reply {
        Reply {
            text: String::new(),
            tool_uses: tools,
        }
    }

    #[tokio::test]
    async fn a_decline_is_the_final_answer() {
        let llm = scripted([Reply::answer("this root should stay empty")]);
        let dir = std::env::temp_dir().join(format!("theseus-init-decline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut messages = opening("do nothing");
        let answer = initialize_phase(&llm, &dir, &mut messages).await.unwrap();
        assert_eq!(answer.as_deref(), Some("this root should stay empty"));
        assert!(!dir.join("theseus.json").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn a_refused_identity_is_retried_and_the_root_is_seeded() {
        let dir = std::env::temp_dir().join(format!("theseus-init-seed-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&dir)
                .status()
                .unwrap()
                .success()
        );

        let llm = scripted([
            calls(vec![call(
                "1",
                "initialize",
                serde_json::json!({ "id": "Bad Name" }),
            )]),
            calls(vec![call(
                "2",
                "initialize",
                serde_json::json!({ "id": "seed-app" }),
            )]),
        ]);
        let mut messages = opening("initialize a seed");
        let answer = initialize_phase(&llm, &dir, &mut messages).await.unwrap();
        assert_eq!(answer, None);
        assert!(dir.join("theseus.json").exists());

        // The refusal came back as a tool result the model could read.
        let refusal = messages
            .iter()
            .filter(|message| matches!(message.role, Role::User))
            .flat_map(|message| message.blocks.iter())
            .find_map(|block| match block {
                Block::ToolResult { content, .. } if content.starts_with("error") => {
                    Some(content.clone())
                }
                _ => None,
            })
            .expect("the refused identity reports its diagnostic");
        assert!(refusal.contains("error"), "{refusal}");
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod argument_tests {
    use super::*;

    fn parse(args: &[&str]) -> anyhow::Result<Options> {
        parse_args_from(args.iter().map(|arg| (*arg).to_string()))
    }

    #[test]
    fn a_project_is_preserved_for_start_and_resume() {
        let start = parse(&["--project", "/tmp/foreign", "--allow-writes", "build it"]).unwrap();
        assert_eq!(start.mode, Mode::Start("build it".to_string()));
        assert!(start.allow_writes);
        assert_eq!(start.project, Some(PathBuf::from("/tmp/foreign")));

        let resume = parse(&["--resume", "--project", "/tmp/foreign"]).unwrap();
        assert_eq!(resume.mode, Mode::Resume);
        assert_eq!(resume.project, Some(PathBuf::from("/tmp/foreign")));
    }

    #[test]
    fn init_rides_with_project_and_start() {
        let options = parse(&[
            "--project",
            "/tmp/foreign",
            "--init",
            "--allow-writes",
            "go",
        ])
        .unwrap();
        assert!(options.init);
        assert_eq!(options.mode, Mode::Start("go".to_string()));
        assert!(parse(&["--init", "--init", "--project", "/tmp/x", "go"]).is_err());
    }

    #[test]
    fn project_and_mode_arguments_are_strict() {
        for args in [
            vec!["--project"],
            vec!["--project", "--resume"],
            vec!["--project", "one", "--project", "two", "goal"],
            vec!["one", "two"],
            vec!["--resume", "goal"],
        ] {
            assert!(parse(&args).is_err(), "accepted {args:?}");
        }
    }
}
