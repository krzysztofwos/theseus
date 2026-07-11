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

use theseus_modeling::Model;

use crate::{
    GatedCheckpoint, GatedWorkspace, ProjectContext,
    generated::{Checkpoint, Ctx, Toolchain, Workspace, dispatch_tool},
    service::{
        apply_patch, checkpoint_snapshot_request, checkpoint_state_request, edit_rust_item_model,
        ensure_checkpoint_project, generate_model, implement_model, persist_model, scaffold_model,
    },
};

/// A working model an agent edits by calling Theseus's own operations as tools.
/// Each accepted edit updates the working model, so a later call sees it. Disk
/// writes pass through the gated workspace port, permitted by `allow_writes`.
#[derive(Clone)]
pub struct SessionState {
    project: ProjectContext,
    pub(crate) working: Model,
    pub(crate) persisted: Model,
}

impl SessionState {
    pub fn new(project: ProjectContext) -> Self {
        let model = project.initial_model().clone();
        Self {
            project,
            persisted: model.clone(),
            working: model,
        }
    }

    pub(crate) fn adopt_rollback(&mut self, model: Model) {
        self.persisted = model.clone();
        self.working = model;
    }
}

pub struct Session<'a> {
    project: ProjectContext,
    state: SessionState,
    workspace: &'a dyn Workspace,
    checkpoint: &'a dyn Checkpoint,
    calculator: &'a dyn theseus_calculator::CalculatorService,
    toolchain: &'a dyn Toolchain,
    allow_writes: bool,
}

impl<'a> Session<'a> {
    /// Open a session over the model and immutable root policy in `project`.
    pub fn new(
        project: ProjectContext,
        workspace: &'a dyn Workspace,
        checkpoint: &'a dyn Checkpoint,
        calculator: &'a dyn theseus_calculator::CalculatorService,
        toolchain: &'a dyn Toolchain,
        allow_writes: bool,
    ) -> Self {
        let state = SessionState::new(project.clone());
        Self {
            project,
            state,
            workspace,
            checkpoint,
            calculator,
            toolchain,
            allow_writes,
        }
    }

    pub fn from_state(
        project: ProjectContext,
        state: SessionState,
        workspace: &'a dyn Workspace,
        checkpoint: &'a dyn Checkpoint,
        calculator: &'a dyn theseus_calculator::CalculatorService,
        toolchain: &'a dyn Toolchain,
        allow_writes: bool,
    ) -> Result<Self, crate::ProjectBindingError> {
        project.ensure_same_project(&state.project)?;
        Ok(Self {
            project,
            state,
            workspace,
            checkpoint,
            calculator,
            toolchain,
            allow_writes,
        })
    }

    /// The working model, taken by value for inspection or persistence.
    pub fn into_model(self) -> Model {
        self.state.working
    }

    /// Both the speculative and committed models, carried across server calls.
    pub fn into_state(self) -> SessionState {
        self.state
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
        if name == "generate" {
            let workspace = self.gate();
            let files = generate_model(
                &self.project,
                &self.state.working,
                &self.state.persisted,
                &workspace,
                self.toolchain,
            )
            .await?;
            self.state.persisted = self.state.working.clone();
            return Ok(serde_json::to_string(&files)?);
        }
        if name == "scaffold" {
            let workspace = self.gate();
            let files = scaffold_model(
                &self.project,
                &self.state.working,
                &self.state.persisted,
                &workspace,
                self.toolchain,
            )
            .await?;
            self.state.persisted = self.state.working.clone();
            return Ok(serde_json::to_string(&files)?);
        }
        if name == "implement" {
            let request = crate::generated::parse_implement_request_input(input)?;
            let workspace = self.gate();
            let result = implement_model(
                &self.project,
                &self.state.working,
                &self.state.persisted,
                request,
                &workspace,
                self.toolchain,
            )
            .await?;
            return Ok(serde_json::to_string(&result)?);
        }
        if name == "edit_rust_item" {
            let request = crate::generated::parse_rust_item_request_input(input)?;
            let workspace = self.gate();
            let result = edit_rust_item_model(
                &self.project,
                &self.state.working,
                &self.state.persisted,
                request,
                &workspace,
                self.toolchain,
            )
            .await?;
            return Ok(serde_json::to_string(&result)?);
        }
        if name == "snapshot" {
            let request = crate::generated::parse_snapshot_request_input(input)?;
            ensure_checkpoint_project(&self.project, self.checkpoint).await?;
            let plan =
                checkpoint_snapshot_request(&self.project, &self.state.persisted, request.label)?;
            let snapshot = self.checkpoint_gate().snapshot(&plan).await?;
            return Ok(snapshot.reference);
        }
        if name == "rollback" {
            let request = crate::generated::parse_snapshot_ref_input(input)?;
            ensure_checkpoint_project(&self.project, self.checkpoint).await?;
            let plan =
                checkpoint_state_request(&self.project, &self.state.persisted, request.reference)?;
            let restored = self.checkpoint_gate().restore(&plan).await?;
            self.state.adopt_rollback(restored.model);
            return Ok(restored.detail);
        }
        if name == "diff" {
            let request = crate::generated::parse_snapshot_ref_input(input)?;
            ensure_checkpoint_project(&self.project, self.checkpoint).await?;
            let plan =
                checkpoint_state_request(&self.project, &self.state.persisted, request.reference)?;
            return self.checkpoint_gate().diff(&plan).await;
        }
        let workspace = self.gate();
        let checkpoint = self.checkpoint_gate();
        let ctx = Ctx {
            model: &self.state.working,
            project: &self.project,
            workspace: &workspace,
            checkpoint: &checkpoint,
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
        let (outcome, proposed) = apply_patch(&self.state.working, &request)?;
        if let Some(proposed) = proposed {
            if write {
                persist_model(
                    &self.project,
                    &self.state.persisted,
                    &proposed,
                    &self.gate(),
                    self.toolchain,
                )
                .await?;
                self.state.persisted = proposed.clone();
            }
            self.state.working = proposed;
        }
        Ok(serde_json::to_string(&outcome)?)
    }

    /// The workspace port carrying this session's write permission.
    fn gate(&self) -> GatedWorkspace<&'a dyn Workspace> {
        GatedWorkspace {
            workspace: self.workspace,
            allow_writes: self.allow_writes,
        }
    }

    /// The checkpoint port carrying the same permission with its own policy.
    fn checkpoint_gate(&self) -> GatedCheckpoint<&'a dyn Checkpoint> {
        GatedCheckpoint {
            checkpoint: self.checkpoint,
            allow_writes: self.allow_writes,
        }
    }
}
