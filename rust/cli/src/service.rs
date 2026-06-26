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
    apply_edits, coverage, describe, handler_source, model_hash, query, verify,
};

use crate::generated::{
    Ctx, ImplementRequest, PatchRequest, QueryRequest, ShowRequest, TheseusService,
};
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

    fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let source = std::fs::read_to_string(workspace_root().join(AUTHORED_IMPL_PATH))
            .with_context(|| format!("reading {AUTHORED_IMPL_PATH}"))?;
        Ok(handler_source(self.model, &source, &request.method)?)
    }

    fn implement(&self, request: ImplementRequest) -> anyhow::Result<String> {
        let base = model_hash(self.model);
        if base != request.expect_model_hash {
            anyhow::bail!(
                "stale model hash: expected `{}`, current is `{base}`; run `theseus query`",
                request.expect_model_hash
            );
        }
        let source = std::fs::read_to_string(workspace_root().join(AUTHORED_IMPL_PATH))
            .with_context(|| format!("reading {AUTHORED_IMPL_PATH}"))?;
        let spliced = theseus_modeling::implement(
            self.model,
            &source,
            &request.method,
            &request.body,
            "crate::generated::",
        )?;
        self.workspace.write_file(&GeneratedFile {
            path: AUTHORED_IMPL_PATH.to_string(),
            contents: spliced,
        })?;
        Ok(format!(
            "wrote the handler for `{}` into {AUTHORED_IMPL_PATH}. Rebuild to load it",
            request.method
        ))
    }

    fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let (outcome, proposed) = if request.edit.is_empty() {
            let edit = build_edit(&request)?;
            apply_edit(self.model, &edit, &request.expect_model_hash)
        } else {
            let edits = request
                .edit
                .iter()
                .map(|spec| parse_edit_spec(spec))
                .collect::<anyhow::Result<Vec<_>>>()?;
            apply_edits(self.model, &edits, &request.expect_model_hash)
        };
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
/// adapter's wire-to-domain conversion for the verb vocabulary.
fn build_edit(request: &PatchRequest) -> anyhow::Result<Edit> {
    let verb = request.verb.as_deref().context("patch needs --verb or --edit")?;
    let target = request.target.clone().context("patch needs --target")?;
    make_edit(
        verb,
        target,
        request.kind.clone(),
        request.to.clone(),
        request.name.clone(),
        parse_assignments(&request.set)?,
    )
}

/// Parse one batch edit spec, `verb|target|key=value|…`, into an [`Edit`]. The
/// keys `kind`, `name`, and `to` set the matching fields. The rest are scalar
/// assignments. A pipe never appears in a value, so it is the field separator.
fn parse_edit_spec(spec: &str) -> anyhow::Result<Edit> {
    let mut parts = spec.split('|');
    let verb = parts.next().unwrap_or_default().trim();
    let target = parts
        .next()
        .context("edit spec must be `verb|target|…`")?
        .trim()
        .to_string();
    let (mut kind, mut name, mut to, mut attrs) = (None, None, None, Vec::new());
    for part in parts {
        let (key, value) = part
            .split_once('=')
            .with_context(|| format!("edit field `{part}` must be key=value"))?;
        match key.trim() {
            "kind" => kind = Some(value.to_string()),
            "name" => name = Some(value.to_string()),
            "to" => to = Some(value.to_string()),
            key => attrs.push((key.to_string(), value.to_string())),
        }
    }
    make_edit(verb, target, kind, to, name, attrs)
}

/// Assemble an [`Edit`] from a verb and its parts. A missing part the verb needs
/// is a usage error.
fn make_edit(
    verb: &str,
    target: String,
    kind: Option<String>,
    to: Option<String>,
    name: Option<String>,
    attrs: Vec<(String, String)>,
) -> anyhow::Result<Edit> {
    match verb {
        "add" => Ok(Edit::Add {
            parent: target,
            kind: kind.context("add needs a kind")?,
            name: name.context("add needs a name")?,
            attrs,
        }),
        "remove" => Ok(Edit::Remove { target }),
        "rename" => Ok(Edit::Rename {
            target,
            to: to.context("rename needs a new name")?,
        }),
        "set" => Ok(Edit::Set { target, attrs }),
        other => anyhow::bail!("unknown verb `{other}`; expected add, remove, rename, or set"),
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
