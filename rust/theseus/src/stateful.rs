//! A serialized, stateful service composition for long-lived inbounds.
//!
//! CLI calls can borrow one immutable model because the process exits after the
//! call. Servers instead need accepted patches to become the model used by the
//! next request. [`StatefulSession`] owns that working model behind one lock and
//! holds the lock across each operation, so reads and writes observe one ordered
//! revision history.

use theseus_modeling::{
    CoverageReport, GeneratedFile, Model, PatchOutcome, QueryOutcome, VerifyReport,
};
use tokio::sync::Mutex;

use crate::{
    CargoToolchain, Checkpoint, Ctx, FsWorkspace, GatedCheckpoint, GatedToolchain, GatedWorkspace,
    GitCheckpoint, ImplementRequest, ListRequest, PatchRequest, ProjectContext,
    ProjectContextError, QueryRequest, ReadRequest, RustItemRequest, ShowRequest, SnapshotRef,
    SnapshotRequest, SourceDocument, TheseusService, Toolchain, Workspace,
    service::{
        apply_patch, checkpoint_snapshot_request, checkpoint_state_request, edit_rust_item_model,
        ensure_checkpoint_project, generate_model, implement_model, persist_model, scaffold_model,
    },
};

/// A long-lived service over one serialized working model.
///
/// The concrete adapters are owned beside the model. Generated write gates wrap
/// the workspace and checkpoint once at construction, preserving the same
/// read-only default as [`crate::Session`].
pub struct StatefulSession<W, C, A, T> {
    project: ProjectContext,
    state: Mutex<crate::SessionState>,
    workspace: GatedWorkspace<W>,
    checkpoint: GatedCheckpoint<C>,
    calculator: A,
    toolchain: GatedToolchain<T>,
}

impl<W, C, A, T> StatefulSession<W, C, A, T> {
    /// Build a serialized service over one immutable project and its adapters.
    pub fn new(
        project: ProjectContext,
        workspace: W,
        checkpoint: C,
        calculator: A,
        toolchain: T,
        allow_writes: bool,
    ) -> Self {
        let state = crate::SessionState::new(project.clone());
        Self {
            project,
            state: Mutex::new(state),
            workspace: GatedWorkspace {
                workspace,
                allow_writes,
            },
            toolchain: GatedToolchain {
                toolchain,
                allow_writes,
            },
            checkpoint: GatedCheckpoint {
                checkpoint,
                allow_writes,
            },
            calculator,
        }
    }
}

impl StatefulSession<FsWorkspace, GitCheckpoint, theseus_calculator::Calculator, CargoToolchain> {
    /// A complete server composition rooted in one already-opened project.
    pub fn for_project(project: ProjectContext, allow_writes: bool) -> Self {
        Self::new(
            project.clone(),
            FsWorkspace::for_project(&project),
            GitCheckpoint::for_project(project.clone()),
            theseus_calculator::Calculator,
            CargoToolchain::for_project(&project),
            allow_writes,
        )
    }

    /// Theseus's repository-rooted server composition.
    pub fn at_repo_root(allow_writes: bool) -> Result<Self, ProjectContextError> {
        let project = crate::theseus_project()?;
        Ok(Self::for_project(project, allow_writes))
    }
}

impl<W, C, A, T> StatefulSession<W, C, A, T>
where
    W: Workspace,
    C: Checkpoint,
    A: theseus_calculator::CalculatorService,
    T: Toolchain,
{
    fn ctx<'a>(&'a self, model: &'a Model) -> Ctx<'a> {
        Ctx {
            model,
            project: &self.project,
            workspace: &self.workspace,
            checkpoint: &self.checkpoint,
            calculator: &self.calculator,
            toolchain: &self.toolchain,
        }
    }
}

#[async_trait::async_trait]
impl<W, C, A, T> TheseusService for StatefulSession<W, C, A, T>
where
    W: Workspace,
    C: Checkpoint,
    A: theseus_calculator::CalculatorService,
    T: Toolchain,
{
    async fn model(&self) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).model().await
    }

    async fn verify(&self) -> anyhow::Result<VerifyReport> {
        let state = self.state.lock().await;
        self.ctx(&state.working).verify().await
    }

    async fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let mut state = self.state.lock().await;
        let files = generate_model(
            &self.project,
            &state.working,
            &state.persisted,
            &self.workspace,
            &self.toolchain,
        )
        .await?;
        state.persisted = state.working.clone();
        Ok(files)
    }

    async fn query(&self, request: QueryRequest) -> anyhow::Result<QueryOutcome> {
        let state = self.state.lock().await;
        self.ctx(&state.working).query(request).await
    }

    async fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let mut state = self.state.lock().await;
        let write = request.write;
        let (outcome, proposed) = apply_patch(&state.working, &request)?;
        if let Some(proposed) = proposed {
            if write {
                persist_model(
                    &self.project,
                    &state.persisted,
                    &proposed,
                    &self.workspace,
                    &self.toolchain,
                )
                .await?;
                state.persisted = proposed.clone();
            }
            state.working = proposed;
        }
        Ok(outcome)
    }

    async fn coverage(&self) -> anyhow::Result<CoverageReport> {
        let state = self.state.lock().await;
        self.ctx(&state.working).coverage().await
    }

    async fn implement(&self, request: ImplementRequest) -> anyhow::Result<crate::ImplementResult> {
        let state = self.state.lock().await;
        implement_model(
            &self.project,
            &state.working,
            &state.persisted,
            request,
            &self.workspace,
            &self.toolchain,
        )
        .await
    }

    async fn edit_rust_item(
        &self,
        request: RustItemRequest,
    ) -> anyhow::Result<crate::RustItemResult> {
        let state = self.state.lock().await;
        edit_rust_item_model(
            &self.project,
            &state.working,
            &state.persisted,
            request,
            &self.workspace,
            &self.toolchain,
        )
        .await
    }

    async fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).show(request).await
    }

    async fn check(&self) -> anyhow::Result<crate::CheckReport> {
        let state = self.state.lock().await;
        self.ctx(&state.working).check().await
    }

    async fn calc(&self, request: crate::CalcRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).calc(request).await
    }

    async fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let mut state = self.state.lock().await;
        let files = scaffold_model(
            &self.project,
            &state.working,
            &state.persisted,
            &self.workspace,
            &self.toolchain,
        )
        .await?;
        state.persisted = state.working.clone();
        Ok(files)
    }

    async fn test(&self) -> anyhow::Result<crate::CheckReport> {
        let state = self.state.lock().await;
        self.ctx(&state.working).test().await
    }

    async fn skills(&self, request: crate::SkillsRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).skills(request).await
    }

    async fn drive(&self, request: crate::DriveRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).drive(request).await
    }

    async fn lint(&self) -> anyhow::Result<crate::CheckReport> {
        let state = self.state.lock().await;
        self.ctx(&state.working).lint().await
    }

    async fn snapshot(&self, request: SnapshotRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        ensure_checkpoint_project(&self.project, &self.checkpoint).await?;
        let plan = checkpoint_snapshot_request(&self.project, &state.persisted, request.label)?;
        Ok(self.checkpoint.snapshot(&plan).await?.reference)
    }

    async fn rollback(&self, request: SnapshotRef) -> anyhow::Result<String> {
        let mut state = self.state.lock().await;
        ensure_checkpoint_project(&self.project, &self.checkpoint).await?;
        let plan = checkpoint_state_request(&self.project, &state.persisted, request.reference)?;
        let restored = self.checkpoint.restore(&plan).await?;
        state.adopt_rollback(restored.model);
        Ok(restored.detail)
    }

    async fn diff(&self, request: SnapshotRef) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        ensure_checkpoint_project(&self.project, &self.checkpoint).await?;
        let plan = checkpoint_state_request(&self.project, &state.persisted, request.reference)?;
        self.checkpoint.diff(&plan).await
    }

    async fn release(&self, request: SnapshotRef) -> anyhow::Result<String> {
        let _state = self.state.lock().await;
        ensure_checkpoint_project(&self.project, &self.checkpoint).await?;
        self.checkpoint.release(&request.reference).await
    }

    async fn prune(&self, request: crate::SnapshotRetention) -> anyhow::Result<String> {
        let _state = self.state.lock().await;
        ensure_checkpoint_project(&self.project, &self.checkpoint).await?;
        self.checkpoint.prune(&request).await
    }

    async fn restart(&self) -> anyhow::Result<()> {
        let state = self.state.lock().await;
        self.ctx(&state.working).restart().await
    }

    async fn read(&self, request: ReadRequest) -> anyhow::Result<SourceDocument> {
        let state = self.state.lock().await;
        self.ctx(&state.working).read(request).await
    }

    async fn search(&self, request: crate::SearchRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).search(request).await
    }

    async fn list(&self, request: ListRequest) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).list(request).await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use theseus_model::theseus_model;
    use theseus_modeling::Edit;

    use super::*;

    fn project() -> ProjectContext {
        crate::theseus_project().expect("Theseus project context is valid")
    }

    struct NoopWorkspace;

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {
        async fn context(&self) -> anyhow::Result<ProjectContext> {
            Ok(project())
        }
    }

    struct StubCheckpoint;

    #[async_trait::async_trait]
    impl Checkpoint for StubCheckpoint {
        async fn context(&self) -> anyhow::Result<ProjectContext> {
            Ok(project())
        }
    }

    #[derive(Default)]
    struct ModelCheckpoint(std::sync::Mutex<Option<Model>>);

    #[async_trait::async_trait]
    impl Checkpoint for ModelCheckpoint {
        async fn context(&self) -> anyhow::Result<ProjectContext> {
            Ok(project())
        }

        async fn snapshot(
            &self,
            request: &crate::CheckpointSnapshotRequest,
        ) -> anyhow::Result<crate::CheckpointSnapshot> {
            *self.0.lock().unwrap() = Some(request.model.clone());
            Ok(crate::CheckpointSnapshot {
                reference: "stateful-snapshot".to_string(),
            })
        }

        async fn restore(
            &self,
            _request: &crate::CheckpointStateRequest,
        ) -> anyhow::Result<crate::CheckpointRestore> {
            let model = self
                .0
                .lock()
                .unwrap()
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no snapshot"))?;
            Ok(crate::CheckpointRestore {
                detail: "restored stateful-snapshot".to_string(),
                model,
            })
        }
    }

    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {
        async fn context(&self) -> anyhow::Result<ProjectContext> {
            Ok(project())
        }
    }

    type TestSession = StatefulSession<
        NoopWorkspace,
        StubCheckpoint,
        theseus_calculator::Calculator,
        StubToolchain,
    >;

    fn session() -> TestSession {
        StatefulSession::new(
            project(),
            NoopWorkspace,
            StubCheckpoint,
            theseus_calculator::Calculator,
            StubToolchain,
            false,
        )
    }

    fn add_type(name: &str) -> PatchRequest {
        PatchRequest {
            edit: vec![Edit::Add {
                parent: "model:theseus".to_string(),
                kind: "type".to_string(),
                name: name.to_string(),
                attrs: [("shape".to_string(), "foreign:String".to_string())].into(),
            }],
            write: false,
        }
    }

    fn query_node(node: &str) -> QueryRequest {
        QueryRequest {
            find: None,
            node: Some(node.to_string()),
            kind: None,
        }
    }

    #[tokio::test]
    async fn an_in_memory_patch_updates_subsequent_queries() {
        let service = session();
        service
            .patch(add_type("StatefulProbe"))
            .await
            .expect("the patch applies");

        let query = service
            .query(query_node("type:theseus:StatefulProbe"))
            .await
            .expect("the query runs");
        assert_eq!(query.handles.len(), 1);
        assert_eq!(query.handles[0].name, "StatefulProbe");
    }

    #[tokio::test]
    async fn concurrent_patches_cannot_lose_an_update() {
        let service = Arc::new(session());
        let first = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.patch(add_type("ConcurrentFirst")).await })
        };
        let second = {
            let service = Arc::clone(&service);
            tokio::spawn(async move { service.patch(add_type("ConcurrentSecond")).await })
        };

        first
            .await
            .expect("the first task joins")
            .expect("the first patch applies");
        second
            .await
            .expect("the second task joins")
            .expect("the second patch applies");

        for name in ["ConcurrentFirst", "ConcurrentSecond"] {
            let query = service
                .query(query_node(&format!("type:theseus:{name}")))
                .await
                .expect("the query runs");
            assert_eq!(query.handles.len(), 1, "{name} was lost");
        }
    }

    #[tokio::test]
    async fn rollback_adopts_the_snapshot_model_in_the_stateful_session() {
        let service = StatefulSession::new(
            project(),
            NoopWorkspace,
            ModelCheckpoint::default(),
            theseus_calculator::Calculator,
            StubToolchain,
            true,
        );
        let reference = service
            .snapshot(SnapshotRequest {
                label: "before speculation".to_string(),
            })
            .await
            .expect("the stateful snapshot succeeds");
        service
            .patch(add_type("StatefulAfterSnapshot"))
            .await
            .expect("the speculative patch applies");

        service
            .rollback(SnapshotRef { reference })
            .await
            .expect("the stateful rollback succeeds");

        let state = service.state.lock().await;
        assert!(state.working.type_def("StatefulAfterSnapshot").is_none());
        assert_eq!(state.working, state.persisted);
    }

    #[test]
    fn every_modeled_operation_has_a_serialized_delegation() {
        let mut model = theseus_model();
        model.services.retain(|service| service.name == "Theseus");
        let report = theseus_modeling::coverage(&model, |_| {
            Ok::<_, std::convert::Infallible>(include_str!("stateful.rs").to_string())
        })
        .expect("the stateful service source parses");

        let missing: Vec<&str> = report
            .unimplemented
            .iter()
            .map(|operation| operation.name.as_str())
            .collect();
        assert!(
            missing.is_empty(),
            "StatefulSession must serialize every Theseus operation; missing: {}",
            missing.join(", ")
        );
    }
}
