//! The Theseus agent — the Agent inbound adapter (L4).
//!
//! An LLM drives Theseus's own operations as tools over a [`Session`], so Theseus
//! modifies itself. The same `Session` an external host drives over MCP is driven
//! here by an internal model. This entry point wires the model adapter and the
//! filesystem workspace and runs the loop over a single message.

mod agent;
mod anthropic;

use anyhow::Context;
use theseus::{FsWorkspace, Session};
use theseus_model::theseus_model;

use agent::{OfflineLlm, Reply, run_agent};
use anthropic::AnthropicLlm;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (message, allow_writes) = parse_args()?;
    let workspace = FsWorkspace::at_repo_root();
    let mut session = Session::new(theseus_model(), &workspace, allow_writes);

    // A real model when the API key is set; the offline stub otherwise, so the
    // binary runs with no network and the no-key path is obvious.
    let answer = match AnthropicLlm::from_env() {
        Some(llm) => run_agent(&llm, &mut session, &message).await?,
        None => {
            eprintln!("ANTHROPIC_API_KEY is unset; answering offline without tools");
            let llm = OfflineLlm::new([Reply::answer(
                "set ANTHROPIC_API_KEY to drive Theseus's tools with a real model",
            )]);
            run_agent(&llm, &mut session, &message).await?
        }
    };
    println!("{answer}");
    Ok(())
}

/// Parse `agent [--allow-writes] <message>`.
fn parse_args() -> anyhow::Result<(String, bool)> {
    let mut allow_writes = false;
    let mut message = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-writes" => allow_writes = true,
            _ => message = Some(arg),
        }
    }
    let message = message.context("usage: agent [--allow-writes] <message>")?;
    Ok((message, allow_writes))
}
