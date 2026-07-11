//! The `mcp-server` binary: a Model Context Protocol server exposing Theseus's
//! operations as tools to an external host over stdio.
//!
//! An external agent connects, lists the catalog, and calls tools by name. Each
//! call runs against a [`Session`](theseus::Session) over the working model, so the
//! host drives the same tool surface as the in-process agent loop. Writes are
//! refused unless the server is launched with `--allow-writes`.
//! `--project <ROOT>` selects a durable modeled project; without it the server
//! remains bound to Theseus's own repository.

mod server;

use std::path::PathBuf;

use anyhow::Context;
use rmcp::{ServiceExt, transport};
use theseus::ProjectContext;

use crate::server::TheseusMcp;

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;
    let project = match args.project {
        Some(root) => ProjectContext::open(root)?,
        None => theseus::theseus_project()?,
    };
    let server = TheseusMcp::new(project, args.allow_writes);
    let running = server
        .serve(transport::stdio())
        .await
        .context("starting the MCP server over stdio")?;
    running.waiting().await.context("serving MCP requests")?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct Args {
    allow_writes: bool,
    project: Option<PathBuf>,
}

impl Args {
    fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut arguments = arguments.into_iter();
        let mut allow_writes = false;
        let mut project = None;

        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--allow-writes" => {
                    anyhow::ensure!(!allow_writes, "`--allow-writes` may be passed only once");
                    allow_writes = true;
                }
                "--project" => {
                    anyhow::ensure!(project.is_none(), "`--project` may be passed only once");
                    let root = arguments
                        .next()
                        .filter(|root| !root.is_empty() && !root.starts_with('-'))
                        .context("`--project` requires a ROOT value")?;
                    project = Some(PathBuf::from(root));
                }
                _ => anyhow::bail!(
                    "unknown argument `{argument}`; usage: mcp-server [--allow-writes] [--project ROOT]"
                ),
            }
        }

        Ok(Self {
            allow_writes,
            project,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(arguments: &[&str]) -> anyhow::Result<Args> {
        Args::parse(arguments.iter().map(ToString::to_string))
    }

    #[test]
    fn parses_project_and_write_gate() {
        assert_eq!(
            args(&["--project", "adopters/journal", "--allow-writes"]).unwrap(),
            Args {
                allow_writes: true,
                project: Some(PathBuf::from("adopters/journal")),
            }
        );
    }

    #[test]
    fn refuses_missing_and_duplicate_options() {
        assert!(args(&["--project"]).is_err());
        assert!(args(&["--project", "--allow-writes"]).is_err());
        assert!(args(&["--project", "-x"]).is_err());
        assert!(args(&["--project", ".", "--project", "."]).is_err());
        assert!(args(&["--allow-writes", "--allow-writes"]).is_err());
    }

    #[test]
    fn refuses_unknown_arguments() {
        assert!(args(&["adopters/journal"]).is_err());
        assert!(args(&["--write"]).is_err());
    }
}
