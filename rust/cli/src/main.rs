//! Theseus's command-line interface (L3).
//!
//! This file is the composition root and the inbound adapters. The generated
//! [`generated`] module supplies the command surface, the parsed
//! [`Invocation`](generated::Invocation), the inbound
//! [`TheseusService`](generated::TheseusService) contract, the outbound port
//! traits, and the [`Ctx`](generated::Ctx) that carries the wired ports. Here we
//! present each result in an exhaustive match on `Invocation` and back the ports
//! with real filesystem adapters. The operation handlers are the authored leaves
//! in [`service`]; a new operation forces a presentation arm here, while its
//! handler defaults to unimplemented until authored.

mod generated;
mod service;

use std::path::{Path, PathBuf};

use generated::{Ctx, Invocation, TheseusService, Workspace};
use theseus_model::theseus_model;
use theseus_modeling::GeneratedFile;

fn main() -> anyhow::Result<()> {
    let model = theseus_model();
    let workspace = FsWorkspace {
        root: workspace_root(),
    };
    let ctx = Ctx {
        model: &model,
        workspace: &workspace,
    };

    // `arg_required_else_help(true)` in the generated surface means a bare
    // invocation prints help and exits, so there is always a subcommand to parse.
    let matches = generated::command().get_matches();
    run(&ctx, Invocation::from_matches(&matches))
}

// ============================================================================
// Authored outbound adapters — the leaves that implement the generated ports.
// ============================================================================

/// Writes generated files relative to the workspace root.
struct FsWorkspace {
    root: PathBuf,
}

impl Workspace for FsWorkspace {
    fn write_file(&self, request: &GeneratedFile) -> anyhow::Result<()> {
        std::fs::write(self.root.join(&request.path), &request.contents)?;
        Ok(())
    }
}

// ============================================================================
// Authored presentation — the CLI inbound adapter's response side. The match is
// exhaustive over `Invocation`, so a new operation forces a presentation decision
// here rather than a runtime surprise. Each arm is free to render however it
// needs: JSON, human lines, an exit code, or a follow-up notice.
// ============================================================================

/// Run a parsed invocation against the service and write the result.
fn run(service: &impl TheseusService, invocation: Invocation) -> anyhow::Result<()> {
    match invocation {
        Invocation::Model => println!("{}", service.model()?),
        Invocation::Verify => {
            let report = service.verify()?;
            println!("{}", report.render());
            if !report.conformant {
                std::process::exit(1);
            }
        }
        Invocation::Generate => {
            for file in service.generate()? {
                println!("wrote {}", file.path);
            }
        }
        Invocation::Query(request) => {
            let outcome = service.query(request)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
        }
        Invocation::Coverage => {
            let report = service.coverage()?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Invocation::Implement(request) => {
            println!("{}", service.implement(request)?);
        }
        Invocation::Patch(request) => {
            let applied = request.write.then(|| request.verb.clone());
            let outcome = service.patch(request)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            if let Some(verb) = applied.filter(|_| outcome.ok) {
                println!(
                    "applied `{verb}` and reprojected; rebuild — the compiler flags anything left to author"
                );
            }
            if !outcome.ok {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

/// The repository root (the directory containing `rust/`), derived from this
/// crate's compile-time location at `<root>/rust/cli`.
pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives at <root>/rust/cli")
        .to_path_buf()
}
