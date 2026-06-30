//! The `mcp-server` binary: a Model Context Protocol server exposing Theseus's
//! operations as tools to an external host over stdio.
//!
//! An external agent connects, lists the catalog, and calls tools by name. Each
//! call runs against a [`Session`](theseus::Session) over the working model, so the
//! host drives the same tool surface as the in-process agent loop. Writes are
//! refused unless the server is launched with `--allow-writes`.

mod server;

use anyhow::Context;
use rmcp::{ServiceExt, transport};
use theseus_model::theseus_model;

use crate::server::TheseusMcp;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let allow_writes = std::env::args().skip(1).any(|arg| arg == "--allow-writes");
    let server = TheseusMcp::new(theseus_model(), allow_writes);
    let running = server
        .serve(transport::stdio())
        .await
        .context("starting the MCP server over stdio")?;
    running.waiting().await.context("serving MCP requests")?;
    Ok(())
}
