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

use theseus_model::{crate_is_scaffolded, generated_files};
use theseus_modeling::Model;

use crate::{
    GatedWorkspace,
    generated::{Ctx, Toolchain, Workspace, dispatch_tool},
    service::apply_patch,
    workspace_root,
};

/// A working model an agent edits by calling Theseus's own operations as tools.
/// Each accepted edit updates the working model, so a later call sees it. Disk
/// writes pass through the gated workspace port, permitted by `allow_writes`.
pub struct Session<'a> {
    model: Model,
    workspace: &'a dyn Workspace,
    calculator: &'a dyn theseus_calculator::CalculatorService,
    toolchain: &'a dyn Toolchain,
    allow_writes: bool,
}

impl<'a> Session<'a> {
    /// Open a session over a working copy of `model`.
    pub fn new(
        model: Model,
        workspace: &'a dyn Workspace,
        calculator: &'a dyn theseus_calculator::CalculatorService,
        toolchain: &'a dyn Toolchain,
        allow_writes: bool,
    ) -> Self {
        Self {
            model,
            workspace,
            calculator,
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
    pub async fn call(&mut self, name: &str, input: &serde_json::Value) -> anyhow::Result<String> {
        // `patch` mutates the session — an accepted edit updates the working
        // model — so the session answers it and shadows the generated arm.
        if name == "patch" {
            return self.patch(input).await;
        }
        let workspace = self.gate();
        let ctx = Ctx {
            model: &self.model,
            workspace: &workspace,
            calculator: self.calculator,
            toolchain: self.toolchain,
        };
        dispatch_tool(&ctx, name, input).await
    }

    /// Apply a `patch` tool call to the working model. Every accepted edit updates
    /// it, so a later call sees it. A `write` reprojects to disk through the gated
    /// workspace port. The request parses through the generated conversion and
    /// applies through the shared rule, so the session and the trait handler
    /// read one contract.
    async fn patch(&mut self, input: &serde_json::Value) -> anyhow::Result<String> {
        let request = crate::generated::parse_patch_request_input(input)?;
        let write = request.write;
        let (outcome, proposed) = apply_patch(&self.model, &request)?;
        if let Some(proposed) = proposed {
            if write {
                persist(&proposed, &self.gate()).await?;
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

/// Reproject a model to disk through the workspace port, writing each generated
/// file whose crate is scaffolded.
pub(crate) async fn persist(model: &Model, workspace: &dyn Workspace) -> anyhow::Result<()> {
    let root = workspace_root();
    for file in generated_files(model) {
        if crate_is_scaffolded(&root, &file) {
            workspace.write_file(&file).await?;
        }
    }
    Ok(())
}
