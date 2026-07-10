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
    CargoToolchain, Checkpoint, Ctx, FsWorkspace, GatedCheckpoint, GatedWorkspace, GitCheckpoint,
    ImplementRequest, ListRequest, PatchRequest, QueryRequest, ReadRequest, ShowRequest,
    SnapshotRef, SnapshotRequest, TheseusService, Toolchain, Workspace,
    service::{apply_patch, generate_model, implement_model, persist_model, scaffold_model},
};

/// A long-lived service over one serialized working model.
///
/// The concrete adapters are owned beside the model. Generated write gates wrap
/// the workspace and checkpoint once at construction, preserving the same
/// read-only default as [`crate::Session`].
pub struct StatefulSession<W, C, A, T> {
    state: Mutex<crate::SessionState>,
    workspace: GatedWorkspace<W>,
    checkpoint: GatedCheckpoint<C>,
    calculator: A,
    toolchain: T,
}

impl<W, C, A, T> StatefulSession<W, C, A, T> {
    /// Build a serialized service over the supplied model and adapters.
    pub fn new(
        model: Model,
        workspace: W,
        checkpoint: C,
        calculator: A,
        toolchain: T,
        allow_writes: bool,
    ) -> Self {
        Self {
            state: Mutex::new(crate::SessionState::new(model)),
            workspace: GatedWorkspace {
                workspace,
                allow_writes,
            },
            checkpoint: GatedCheckpoint {
                checkpoint,
                allow_writes,
            },
            calculator,
            toolchain,
        }
    }
}

impl StatefulSession<FsWorkspace, GitCheckpoint, theseus_calculator::Calculator, CargoToolchain> {
    /// Theseus's repository-rooted server composition.
    pub fn at_repo_root(allow_writes: bool) -> Self {
        Self::new(
            theseus_model::theseus_model(),
            FsWorkspace::at_repo_root(),
            GitCheckpoint::at_repo_root(),
            theseus_calculator::Calculator,
            CargoToolchain,
            allow_writes,
        )
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

    async fn lint(&self) -> anyhow::Result<crate::CheckReport> {
        let state = self.state.lock().await;
        self.ctx(&state.working).lint().await
    }

    async fn snapshot(&self, request: SnapshotRequest) -> anyhow::Result<String> {
        let mut state = self.state.lock().await;
        let reference = self.checkpoint.snapshot(&request.label).await?;
        state.record_snapshot(reference.clone());
        Ok(reference)
    }

    async fn rollback(&self, request: SnapshotRef) -> anyhow::Result<String> {
        let mut state = self.state.lock().await;
        let model = state.snapshot_model(&request.reference)?;
        let result = self.checkpoint.restore(&request.reference).await?;
        state.adopt_rollback(model);
        Ok(result)
    }

    async fn diff(&self, request: SnapshotRef) -> anyhow::Result<String> {
        let state = self.state.lock().await;
        self.ctx(&state.working).diff(request).await
    }

    async fn restart(&self) -> anyhow::Result<()> {
        let state = self.state.lock().await;
        self.ctx(&state.working).restart().await
    }

    async fn read(&self, request: ReadRequest) -> anyhow::Result<String> {
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

    struct NoopWorkspace;

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {}

    struct StubCheckpoint;

    #[async_trait::async_trait]
    impl Checkpoint for StubCheckpoint {}

    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {}

    type TestSession = StatefulSession<
        NoopWorkspace,
        StubCheckpoint,
        theseus_calculator::Calculator,
        StubToolchain,
    >;

    fn session() -> TestSession {
        StatefulSession::new(
            theseus_model(),
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
