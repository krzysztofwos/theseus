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
//! loop instead, so the model can repair the workspace from the old binary.

mod agent;
mod anthropic;
mod generated;

use std::{path::PathBuf, process::Command};

use agent::{
    Message, OfflineLlm, Outcome, Reply, answer_restart, load_transcript, opening, run_agent,
    save_transcript,
};
use anthropic::AnthropicLlm;
use generated::Llm;
use theseus::{CargoToolchain, FsWorkspace, Session, workspace_root};
use theseus_model::theseus_model;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (mode, allow_writes) = parse_args()?;
    let workspace = FsWorkspace::at_repo_root();
    let calculator = theseus_calculator::Calculator;
    let toolchain = CargoToolchain;
    let mut session = Session::new(
        theseus_model(),
        &workspace,
        &calculator,
        &toolchain,
        allow_writes,
    );

    let messages = match &mode {
        Mode::Start(message) => opening(message),
        Mode::Resume => answer_restart(
            load_transcript(&session_path())?,
            "rebuilt; this is the new binary, and its compiled model and tool \
catalog match the workspace",
        )?,
    };

    // A real model when the API key is set; the offline stub otherwise, so the
    // binary runs with no network and the no-key path is obvious.
    let answer = match AnthropicLlm::from_env() {
        Some(llm) => drive(&llm, &mut session, messages, allow_writes).await?,
        None => {
            eprintln!("ANTHROPIC_API_KEY is unset; answering offline without tools");
            let llm = OfflineLlm::new([Reply::answer(
                "set ANTHROPIC_API_KEY to drive Theseus's tools with a real model",
            )]);
            drive(&llm, &mut session, messages, allow_writes).await?
        }
    };
    // The conversation is complete, so a persisted transcript has served its
    // purpose and a later `--resume` should not find it.
    std::fs::remove_file(session_path()).ok();
    println!("{answer}");
    Ok(())
}

/// Run the loop to its final answer, sailing through any restart it asks for.
/// A successful rebuild persists the transcript and replaces this process with
/// the new binary. A failed one answers the restart call with the compiler's
/// output and keeps the loop running in the old binary.
async fn drive(
    llm: &impl Llm,
    session: &mut Session<'_>,
    mut messages: Vec<Message>,
    allow_writes: bool,
) -> anyhow::Result<String> {
    loop {
        match run_agent(llm, session, messages).await? {
            Outcome::Answered(text) => return Ok(text),
            Outcome::Restart(transcript) => match rebuild().await {
                Ok(()) => {
                    save_transcript(&session_path(), &transcript)?;
                    return Err(resume_exec(allow_writes));
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
async fn rebuild() -> Result<(), String> {
    let output = tokio::process::Command::new("cargo")
        .args(["build", "-p", "theseus-agent"])
        .current_dir(workspace_root())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("running `cargo build`: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    Err(theseus::head(diagnostics.trim()))
}

/// Replace this process with a fresh run of the agent, resuming the persisted
/// session in the newly built binary. Returns only on failure to launch.
fn resume_exec(allow_writes: bool) -> anyhow::Error {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("cargo");
    command.args(["run", "-p", "theseus-agent", "--"]);
    if allow_writes {
        command.arg("--allow-writes");
    }
    command.arg("--resume").current_dir(workspace_root());
    anyhow::Error::new(command.exec()).context("re-entering the rebuilt agent")
}

/// The persisted transcript's path, in the workspace's scratch directory.
fn session_path() -> PathBuf {
    workspace_root().join(".theseus/session.json")
}

/// How the agent starts: a fresh conversation over a message, or a resumed one
/// over the persisted transcript.
enum Mode {
    Start(String),
    Resume,
}

/// Parse `agent [--allow-writes] <message>` or `agent [--allow-writes] --resume`.
fn parse_args() -> anyhow::Result<(Mode, bool)> {
    let mut allow_writes = false;
    let mut resume = false;
    let mut message = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-writes" => allow_writes = true,
            "--resume" => resume = true,
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
                "usage: agent [--allow-writes] <message> | agent [--allow-writes] --resume"
            )
        }
    };
    Ok((mode, allow_writes))
}
