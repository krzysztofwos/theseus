//! The session: a working model an agent edits by calling Theseus's operations.
//!
//! A [`Session`] holds a working copy of the model, the ports that reach the
//! world, and the write permission. [`call`](Session::call) answers `patch`
//! itself — the one tool that mutates session state, adopting each accepted
//! edit into the working model — and hands every other tool to the generated
//! dispatch over a composition root built on that model, so the trait handlers
//! see the session's edits. Writes reach disk through a gated workspace port,
//! so every operation that writes — present, or patched in later — is permitted
//! the same way. Both the agent loop and an external host over MCP drive the
//! same `Session`, so they see one tool surface with one set of semantics.

use anyhow::Context;
use theseus_model::generated_files;
use theseus_modeling::{Edit, GeneratedFile, Model, apply_edits};

use crate::{
    generated::{Ctx, Toolchain, Workspace, dispatch_tool},
    service::crate_is_scaffolded,
    workspace_root,
};

/// The result fed back when a write tool runs without the permission gate.
pub(crate) const WRITE_REFUSED: &str =
    "writes are not permitted; rerun with write permission to apply this edit";

/// A working model an agent edits by calling Theseus's own operations as tools.
/// Each accepted edit updates the working model, so a later call sees it. Disk
/// writes pass through the gated workspace port, permitted by `allow_writes`.
pub struct Session<'a> {
    model: Model,
    workspace: &'a dyn Workspace,
    toolchain: &'a dyn Toolchain,
    allow_writes: bool,
}

impl<'a> Session<'a> {
    /// Open a session over a working copy of `model`.
    pub fn new(
        model: Model,
        workspace: &'a dyn Workspace,
        toolchain: &'a dyn Toolchain,
        allow_writes: bool,
    ) -> Self {
        Self {
            model,
            workspace,
            toolchain,
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
        // `patch` mutates the session — an accepted edit updates the working
        // model — so the session answers it and shadows the generated arm.
        if name == "patch" {
            return self.patch(input);
        }
        let workspace = self.gate();
        let calculator = theseus_calculator::Calculator;
        let ctx = Ctx {
            model: &self.model,
            workspace: &workspace,
            calculator: &calculator,
            toolchain: self.toolchain,
        };
        dispatch_tool(&ctx, name, input)
    }

    /// Apply a `patch` tool call to the working model. Every accepted edit updates
    /// it, so a later call sees it. A `write` reprojects to disk through the gated
    /// workspace port.
    fn patch(&mut self, input: &serde_json::Value) -> anyhow::Result<String> {
        let edits: Vec<Edit> =
            serde_json::from_value(input.get("edit").cloned().unwrap_or_default())
                .context("patch `edit` must be a list of edits")?;
        if edits.is_empty() {
            anyhow::bail!("patch needs at least one edit");
        }
        let write = input
            .get("write")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let (outcome, proposed) = apply_edits(&self.model, &edits);
        if let Some(proposed) = proposed {
            if write {
                persist(&proposed, &self.gate())?;
            }
            self.model = proposed;
        }
        Ok(serde_json::to_string(&outcome)?)
    }

    /// The workspace port carrying this session's write permission.
    fn gate(&self) -> GatedWorkspace<'a> {
        GatedWorkspace {
            workspace: self.workspace,
            allow_writes: self.allow_writes,
        }
    }
}

/// A workspace port carrying the session's write permission. A permitted write
/// passes through and a refused one reports the gate, so every operation that
/// reaches disk through the port is gated the same way.
struct GatedWorkspace<'a> {
    workspace: &'a dyn Workspace,
    allow_writes: bool,
}

impl Workspace for GatedWorkspace<'_> {
    fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        if !self.allow_writes {
            anyhow::bail!(WRITE_REFUSED);
        }
        self.workspace.write_file(file)
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
