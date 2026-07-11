//! The `http-server` binary: Theseus's operations over HTTP — the Http inbound
//! adapter (L4).
//!
//! One route serves every operation, `POST /{operation}` with a JSON body. The
//! generated handlers parse the body into the operation's request, run it against
//! a serialized stateful session, and map the outcome onto the status line:
//! 200 a result, 400 a body that does not parse, 404 an unknown operation, 501
//! an operation with no authored handler, 403 a write the gate refused, 500 any
//! other error. Writes are refused unless the server is launched with
//! `--allow-writes`. `--project <ROOT>` selects a durable modeled project;
//! without it the server remains bound to Theseus's own repository.

use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use theseus::{ProjectContext, StatefulSession};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse(std::env::args().skip(1))?;
    let session = match args.project {
        Some(root) => {
            let project = ProjectContext::open(root)?;
            StatefulSession::for_project(project, args.allow_writes)
        }
        None => StatefulSession::at_repo_root(args.allow_writes)?,
    };
    let router = theseus_http::router(Arc::new(session));
    let listener = tokio::net::TcpListener::bind(&args.listen).await?;
    eprintln!("listening on http://{}", args.listen);
    axum::serve(listener, router).await?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
struct Args {
    allow_writes: bool,
    project: Option<PathBuf>,
    listen: String,
}

impl Args {
    fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Self> {
        let mut arguments = arguments.into_iter();
        let mut allow_writes = false;
        let mut project = None;
        let mut listen = None;

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
                flag if flag.starts_with('-') => anyhow::bail!(
                    "unknown flag `{flag}`; usage: http-server [--allow-writes] [--project ROOT] [address]"
                ),
                _ => {
                    anyhow::ensure!(listen.is_none(), "listen address may be passed only once");
                    listen = Some(argument);
                }
            }
        }

        Ok(Self {
            allow_writes,
            project,
            listen: listen.unwrap_or_else(|| "127.0.0.1:4870".to_string()),
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
    fn parses_project_write_gate_and_address() {
        assert_eq!(
            args(&[
                "--project",
                "adopters/journal",
                "--allow-writes",
                "0.0.0.0:9000",
            ])
            .unwrap(),
            Args {
                allow_writes: true,
                project: Some(PathBuf::from("adopters/journal")),
                listen: "0.0.0.0:9000".to_string(),
            }
        );
    }

    #[test]
    fn preserves_the_default_address() {
        assert_eq!(args(&[]).unwrap().listen, "127.0.0.1:4870");
    }

    #[test]
    fn refuses_missing_and_duplicate_values() {
        assert!(args(&["--project"]).is_err());
        assert!(args(&["--project", "--allow-writes"]).is_err());
        assert!(args(&["--project", "-x"]).is_err());
        assert!(args(&["--project", ".", "--project", "."]).is_err());
        assert!(args(&["--allow-writes", "--allow-writes"]).is_err());
        assert!(args(&["127.0.0.1:1", "127.0.0.1:2"]).is_err());
        assert!(args(&["-x"]).is_err());
    }
}
