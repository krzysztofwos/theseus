//! Theseus's authored service implementation (L3).
//!
//! These are the operation handlers — the behavior leaves checked against the
//! generated [`TheseusService`](crate::generated::TheseusService) contract. An
//! operation without a handler here falls through to the trait's `unimplemented`
//! default, and `verify`'s coverage check reports it. This is the one file the
//! structured-edit tooling writes. The composition root and adapters in
//! [`main`](crate) stay hand-written.

use anyhow::Context;
use theseus_model::{AUTHORED_IMPL_PATH, generated_files};
use theseus_modeling::{
    CoverageReport, Edit, GeneratedFile, PatchOutcome, QueryOutcome, VerifyReport, apply_edit,
    coverage, describe, query, verify,
};

use crate::generated::{Ctx, PatchRequest, QueryRequest, TheseusService};
use crate::workspace_root;

impl TheseusService for Ctx<'_> {
    fn model(&self) -> anyhow::Result<String> {
        Ok(describe(self.model))
    }

    fn verify(&self) -> anyhow::Result<VerifyReport> {
        Ok(verify(
            self.model,
            &workspace_root(),
            &generated_files(self.model),
            AUTHORED_IMPL_PATH,
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

    fn coverage(&self) -> anyhow::Result<CoverageReport> {
        let source = std::fs::read_to_string(workspace_root().join(AUTHORED_IMPL_PATH))
            .with_context(|| format!("reading {AUTHORED_IMPL_PATH}"))?;
        Ok(coverage(self.model, &source)?)
    }

    fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let edit = build_edit(&request)?;
        let (outcome, proposed) = apply_edit(self.model, &edit, &request.expect_model_hash);
        if request.write
            && let Some(proposed) = proposed
        {
            // Reproject every file from the proposed model — the self-model source
            // and the generated scaffolding update together. A new operation's
            // handler defaults to unimplemented until authored here, and `coverage`
            // reports what is left to write.
            for file in generated_files(&proposed) {
                self.workspace.write_file(&file)?;
            }
        }
        Ok(outcome)
    }
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
