//! Theseus's command-line interface (L3).
//!
//! This file is the composition root plus the authored leaves. The generated
//! [`generated`] module supplies the command surface, the parsed
//! [`Invocation`](generated::Invocation), the inbound
//! [`TheseusService`](generated::TheseusService) contract, the outbound port
//! traits, and the [`Ctx`](generated::Ctx) that carries the wired ports. Here we
//! implement the service (each operation returns a domain value, no I/O), present
//! each result in an exhaustive match on `Invocation`, and back the ports with
//! real filesystem adapters. The service impl, the presenter, and the adapters
//! are the authored leaves; a new operation forces both a service method and a
//! presentation arm.

mod generated;

use std::path::{Path, PathBuf};

use anyhow::Context;
use generated::{Ctx, Invocation, PatchRequest, QueryRequest, TheseusService, Workspace};
use theseus_model::{generated_files, theseus_model};
use theseus_modeling::{
    Edit, GeneratedFile, PatchOutcome, QueryOutcome, VerifyReport, apply_edit, describe, query,
    verify,
};

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
// Authored service impl — the operation logic, checked against the generated
// `TheseusService` contract. A missing operation is a build error.
// ============================================================================

impl TheseusService for Ctx<'_> {
    fn model(&self) -> anyhow::Result<String> {
        Ok(describe(self.model))
    }

    fn verify(&self) -> anyhow::Result<VerifyReport> {
        Ok(verify(
            self.model,
            &workspace_root(),
            &generated_files(self.model),
        ))
    }

    fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let files = generated_files(self.model);
        for file in &files {
            self.workspace.write_file(file)?;
        }
        Ok(files)
    }

    fn query(&self, request: QueryRequest) -> anyhow::Result<QueryOutcome> {
        let mut outcome = query(self.model, request.find.as_deref(), request.node.as_deref())?;
        if let Some(kind) = &request.kind {
            outcome.handles.retain(|handle| &handle.kind == kind);
        }
        Ok(outcome)
    }

    fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let edit = build_edit(&request)?;
        let (outcome, proposed) = apply_edit(self.model, &edit, &request.expect_model_hash);
        if request.write
            && let Some(proposed) = proposed
        {
            // Reproject every file from the proposed model — the self-model source
            // and the generated scaffolding update together. The next build then
            // requires the authored method and presentation arm, so the compiler is
            // the checklist for what's left to write by hand.
            for file in generated_files(&proposed) {
                self.workspace.write_file(&file)?;
            }
        }
        Ok(outcome)
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

/// Build the structured [`Edit`] from a parsed patch request — the inbound
/// adapter's wire-to-domain conversion for the verb vocabulary. The verb selects
/// the edit; a missing argument the verb needs is a usage error.
fn build_edit(request: &PatchRequest) -> anyhow::Result<Edit> {
    let target = request.target.clone();
    let attrs = parse_assignments(&request.set)?;
    match request.verb.as_str() {
        "add" => Ok(Edit::Add {
            parent: target,
            kind: request.kind.clone().context("add needs --kind")?,
            name: request.name.clone().context("add needs --name")?,
            attrs,
        }),
        "remove" => Ok(Edit::Remove { target }),
        "rename" => Ok(Edit::Rename {
            target,
            to: request.to.clone().context("rename needs --to")?,
        }),
        "set" => Ok(Edit::Set { target, attrs }),
        other => {
            anyhow::bail!("unknown verb `{other}`; expected add, remove, rename, or set")
        }
    }
}

/// Parse `--set key=value` assignments into attribute pairs. The first `=`
/// separates the key, so a value may itself contain `=`.
fn parse_assignments(set: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    set.iter()
        .map(|pair| {
            let (key, value) = pair
                .split_once('=')
                .with_context(|| format!("assignment `{pair}` must be key=value"))?;
            Ok((key.trim().to_string(), value.to_string()))
        })
        .collect()
}

/// The repository root (the directory containing `rust/`), derived from this
/// crate's compile-time location at `<root>/rust/cli`.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives at <root>/rust/cli")
        .to_path_buf()
}
