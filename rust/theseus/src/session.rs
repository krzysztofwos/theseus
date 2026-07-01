//! The session: a working model an agent edits by calling Theseus's operations.
//!
//! A [`Session`] holds a working copy of the model, the workspace port that
//! persists writes, and the write permission. [`call`](Session::call) runs one
//! tool — one of Theseus's operations — against the working model: a read reports,
//! a `patch` edits the working model and, when permitted, reprojects to disk. Both
//! the agent loop and an external host over MCP drive the same `Session`, so they
//! see one tool surface with one set of semantics.

use anyhow::Context;
use theseus_model::{authored_impl_path, authored_impls, generated_files};
use theseus_modeling::{
    GeneratedFile, Model, apply_edits, coverage, describe, handler_source, query, verify,
};

use crate::generated::Workspace;
use crate::service::{crate_is_scaffolded, handler_path, parse_edit_spec};
use crate::workspace_root;

/// The result fed back when a write tool runs without the permission gate.
pub(crate) const WRITE_REFUSED: &str =
    "writes are not permitted; rerun with write permission to apply this edit";

/// A working model an agent edits by calling Theseus's own operations as tools.
/// Each accepted edit updates the working model, so a later call sees it. A
/// `patch` that writes reprojects to disk through the workspace port, gated by
/// `allow_writes`.
pub struct Session<'a> {
    model: Model,
    workspace: &'a dyn Workspace,
    allow_writes: bool,
}

impl<'a> Session<'a> {
    /// Open a session over a working copy of `model`.
    pub fn new(model: Model, workspace: &'a dyn Workspace, allow_writes: bool) -> Self {
        Self {
            model,
            workspace,
            allow_writes,
        }
    }

    /// The working model, taken by value. An adapter that reconstructs a session
    /// per call reads the model back here to carry accepted edits into the next
    /// one.
    pub fn into_model(self) -> Model {
        self.model
    }

    /// Run one tool against the working model and return its result as a JSON
    /// string. The tool surface is Theseus's own operations, so the session edits
    /// the model it inspects.
    pub fn call(&mut self, name: &str, input: &serde_json::Value) -> anyhow::Result<String> {
        match name {
            "model" => Ok(describe(&self.model)),
            "verify" => {
                let root = workspace_root();
                let report = verify(
                    &self.model,
                    &root,
                    &generated_files(&self.model),
                    &authored_impls(&self.model),
                );
                Ok(serde_json::to_string(&report)?)
            }
            "coverage" => {
                let root = workspace_root();
                let report = coverage(&self.model, |service| -> anyhow::Result<String> {
                    let path = authored_impl_path(&self.model, service);
                    std::fs::read_to_string(root.join(&path))
                        .with_context(|| format!("reading {path}"))
                })?;
                Ok(serde_json::to_string(&report)?)
            }
            "query" => {
                let text = |key: &str| input.get(key).and_then(serde_json::Value::as_str);
                let mut outcome = query(&self.model, text("find"), text("node"))?;
                if let Some(kind) = text("kind") {
                    outcome.handles.retain(|handle| handle.kind == kind);
                }
                Ok(serde_json::to_string(&outcome)?)
            }
            "patch" => self.patch(input),
            "show" => {
                let method = input
                    .get("method")
                    .and_then(serde_json::Value::as_str)
                    .context("show needs a `method`, an operation name from query")?;
                let path = handler_path(&self.model, method)?;
                let source = std::fs::read_to_string(workspace_root().join(&path))
                    .with_context(|| format!("reading {path}"))?;
                Ok(handler_source(&self.model, &source, method)?)
            }
            "implement" => self.implement(input),
            other => anyhow::bail!(
                "unknown tool `{other}`; tools are model, query, verify, coverage, patch, show, implement"
            ),
        }
    }

    /// Apply a `patch` tool call to the working model. Every accepted edit updates
    /// it, so a later call sees it. A `write` reprojects to disk, refused without
    /// `allow_writes`.
    fn patch(&mut self, input: &serde_json::Value) -> anyhow::Result<String> {
        let specs: Vec<&str> = input
            .get("edit")
            .and_then(serde_json::Value::as_array)
            .map(|items| items.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        if specs.is_empty() {
            anyhow::bail!("patch needs an `edit` list of `verb|target|key=value` strings");
        }
        let edits = specs
            .iter()
            .map(|spec| parse_edit_spec(spec))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let write = input
            .get("write")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let (outcome, proposed) = apply_edits(&self.model, &edits);
        if let Some(proposed) = proposed {
            if write {
                if !self.allow_writes {
                    return Ok(WRITE_REFUSED.to_string());
                }
                persist(&proposed, self.workspace)?;
            }
            self.model = proposed;
        }
        Ok(serde_json::to_string(&outcome)?)
    }

    /// Write an authored handler body for an operation into the service impl,
    /// gated by `allow_writes`. The operation must exist in the working model, so
    /// this follows a `patch` that adds it. The running binary still holds the old
    /// code, hence the rebuild note, but `verify` reads the written source, so the
    /// workspace conforms before the rebuild.
    fn implement(&self, input: &serde_json::Value) -> anyhow::Result<String> {
        let method = input
            .get("method")
            .and_then(serde_json::Value::as_str)
            .context("implement needs a `method`, an operation name")?;
        let body = input
            .get("body")
            .and_then(serde_json::Value::as_str)
            .context("implement needs a `body`, the Rust handler body")?;
        if !self.allow_writes {
            return Ok(WRITE_REFUSED.to_string());
        }
        let path = handler_path(&self.model, method)?;
        let source = std::fs::read_to_string(workspace_root().join(&path))
            .with_context(|| format!("reading {path}"))?;
        let spliced =
            theseus_modeling::implement(&self.model, &source, method, body, "crate::generated::")?;
        self.workspace.write_file(&GeneratedFile {
            path: path.clone(),
            contents: spliced,
        })?;
        Ok(format!(
            "wrote the handler for `{method}` into {path}. Rebuild to load it"
        ))
    }
}

/// Reproject a model to disk through the workspace port, writing each generated
/// file whose crate is scaffolded.
pub(crate) fn persist(model: &Model, workspace: &dyn Workspace) -> anyhow::Result<()> {
    let root = workspace_root();
    for file in generated_files(model) {
        if crate_is_scaffolded(&root, &file) {
            workspace.write_file(&file)?;
        }
    }
    Ok(())
}
