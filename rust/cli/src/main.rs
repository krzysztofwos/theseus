//! Theseus's command-line interface (L3).
//!
//! This file is the composition root and the inbound adapters. The generated
//! [`generated`] module supplies the command surface, the parsed
//! [`Invocation`](generated::Invocation), the inbound
//! [`TheseusService`](generated::TheseusService) contract, the outbound port
//! traits, and the [`Ctx`](generated::Ctx) that carries the wired ports. Here we
//! override presentation for the operations that need bespoke output — an exit
//! code, per-file lines, a follow-up notice — and delegate the rest to the
//! generated default presentation, then back the ports with real filesystem
//! adapters. The operation handlers are the authored leaves in [`service`]. A new
//! operation surfaces through the default, so adding one needs no change here.

mod generated;
mod service;

use std::path::{Path, PathBuf};

use generated::{Ctx, Invocation, Llm, TheseusService, Workspace};
use theseus_model::theseus_model;
use theseus_modeling::GeneratedFile;

fn main() -> anyhow::Result<()> {
    let model = theseus_model();
    let workspace = FsWorkspace {
        root: workspace_root(),
    };
    let calculator = theseus_calculator::Calculator;
    let llm = OfflineLlm;
    let ctx = Ctx {
        model: &model,
        workspace: &workspace,
        calculator: &calculator,
        llm: &llm,
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
        let path = self.root.join(&request.path);
        // Scaffolding a new crate writes into a directory that does not exist yet.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &request.contents)?;
        Ok(())
    }
}

/// The offline model adapter: a scripted stand-in for a real model, so the agent
/// loop runs with no network. Phase 1 wires the port. The scripted replies that
/// drive the loop arrive with the `chat` handler.
struct OfflineLlm;

impl Llm for OfflineLlm {
    fn complete(&self, _transcript: &str) -> anyhow::Result<String> {
        Ok("offline model: the loop reached me, with no real model behind it yet.".to_string())
    }
}

// ============================================================================
// Authored presentation — the CLI inbound adapter's response side. Each arm
// overrides the default for an operation that needs bespoke output: an exit code,
// per-file lines, a follow-up notice. Every other operation falls to the generated
// `present`, so a new operation surfaces as text or JSON without a change here.
// ============================================================================

/// Run a parsed invocation against the service and write the result.
fn run(service: &impl TheseusService, invocation: Invocation) -> anyhow::Result<()> {
    match invocation {
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
        Invocation::Scaffold => {
            let written = service.scaffold()?;
            if written.is_empty() {
                println!("every library service crate is already scaffolded");
            }
            for file in &written {
                println!("scaffolded {}", file.path);
            }
        }
        Invocation::Patch(request) => {
            let writing = request.write;
            let outcome = service.patch(request)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            if writing && outcome.ok {
                println!(
                    "applied and reprojected. Rebuild, then `coverage` shows any handler left to author"
                );
            }
            if !outcome.ok {
                std::process::exit(1);
            }
        }
        other => generated::dispatch(service, other)?,
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
