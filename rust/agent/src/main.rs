//! The Theseus agent — the Agent inbound adapter (L4).
//!
//! An LLM drives Theseus's own operations as tools over a [`Session`], so Theseus
//! modifies itself. The same `Session` an external host drives over MCP is driven
//! here by an internal model. This entry point wires the model adapter and the
//! filesystem workspace and runs the loop over a single message.

mod agent;

use anyhow::Context;
use theseus::{FsWorkspace, Session};
use theseus_model::theseus_model;

use agent::{OfflineLlm, Reply, run_agent};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let (message, allow_writes) = parse_args()?;
    let workspace = FsWorkspace::at_repo_root();
    let mut session = Session::new(theseus_model(), &workspace, allow_writes);

    // No real model adapter yet: the offline stub answers without calling tools.
    // Step 3b wires an Anthropic adapter behind the same `Llm` port.
    let llm = OfflineLlm::new([Reply::answer(
        "the offline agent has no model behind it; wire the Anthropic adapter to drive tools",
    )]);

    let answer = run_agent(&llm, &mut session, &message).await?;
    println!("{answer}");
    Ok(())
}

/// Parse `theseus-agent [--allow-writes] <message>`.
fn parse_args() -> anyhow::Result<(String, bool)> {
    let mut allow_writes = false;
    let mut message = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-writes" => allow_writes = true,
            _ => message = Some(arg),
        }
    }
    let message = message.context("usage: theseus-agent [--allow-writes] <message>")?;
    Ok((message, allow_writes))
}
