//! The wire round trip: the generated client through the generated server, over
//! a real socket, preserves the contract — results and error classes alike.

use std::sync::Arc;

use theseus::{
    Checkpoint, CheckpointRestore, CheckpointSnapshot, CheckpointSnapshotRequest,
    CheckpointStateRequest, ImplementRequest, PatchRequest, QueryRequest, SnapshotRef,
    SnapshotRequest, SnapshotRetention, Standalone, StatefulSession, TheseusService, Toolchain,
    Workspace,
};
use theseus_http_client::HttpTheseusClient;
use theseus_modeling::Edit;

struct FailedCheck;

struct CheckpointEcho;

struct NoopWorkspace;

#[async_trait::async_trait]
impl Workspace for NoopWorkspace {
    async fn context(&self) -> anyhow::Result<theseus::ProjectContext> {
        Ok(theseus::theseus_project()?)
    }
}

struct NoopToolchain;

#[async_trait::async_trait]
impl Toolchain for NoopToolchain {
    async fn context(&self) -> anyhow::Result<theseus::ProjectContext> {
        Ok(theseus::theseus_project()?)
    }
}

#[derive(Default)]
struct ModelCheckpoint(std::sync::Mutex<Option<theseus_modeling::Model>>);

#[async_trait::async_trait]
impl Checkpoint for ModelCheckpoint {
    async fn context(&self) -> anyhow::Result<theseus::ProjectContext> {
        Ok(theseus::theseus_project()?)
    }

    async fn snapshot(
        &self,
        request: &CheckpointSnapshotRequest,
    ) -> anyhow::Result<CheckpointSnapshot> {
        *self.0.lock().expect("the checkpoint mutex is available") = Some(request.model.clone());
        Ok(CheckpointSnapshot {
            reference: "http-stateful-snapshot".to_string(),
        })
    }

    async fn restore(
        &self,
        _request: &CheckpointStateRequest,
    ) -> anyhow::Result<CheckpointRestore> {
        let model = self
            .0
            .lock()
            .expect("the checkpoint mutex is available")
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no snapshot"))?;
        Ok(CheckpointRestore {
            detail: "restored http-stateful-snapshot".to_string(),
            model,
        })
    }
}

#[async_trait::async_trait]
impl TheseusService for CheckpointEcho {
    async fn release(&self, request: SnapshotRef) -> anyhow::Result<String> {
        Ok(format!("released {}", request.reference))
    }

    async fn prune(&self, request: SnapshotRetention) -> anyhow::Result<String> {
        Ok(format!("kept {}", request.keep))
    }

    async fn diff(&self, request: SnapshotRef) -> anyhow::Result<String> {
        Ok(format!("diffed {}", request.reference))
    }
}

#[async_trait::async_trait]
impl TheseusService for FailedCheck {
    async fn check(&self) -> anyhow::Result<theseus::CheckReport> {
        Ok(theseus::CheckReport::failure("compile failure over HTTP"))
    }

    async fn implement(
        &self,
        _request: ImplementRequest,
    ) -> anyhow::Result<theseus::ImplementResult> {
        Ok(theseus::ImplementResult {
            applied: false,
            path: "rust/service.rs".to_string(),
            detail: "compile gate rolled the edit back".to_string(),
            check: theseus::CheckReport::failure("implement failure over HTTP"),
        })
    }
}

/// Serve a router on an ephemeral port and return a client against it.
async fn serve<S: TheseusService + 'static>(service: S) -> HttpTheseusClient {
    let router = theseus_http::router(Arc::new(service));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port binds");
    let addr = listener.local_addr().expect("the bound address reads");
    tokio::spawn(async move {
        axum::serve(listener, router)
            .await
            .expect("the server runs");
    });
    HttpTheseusClient::new(format!("http://{addr}"))
}

#[tokio::test]
async fn the_wire_crossing_preserves_the_contract() {
    let client = serve(Standalone::new(false).expect("Theseus project context is valid")).await;

    // A read crosses typed: the client returns the contract's value.
    let outcome = client
        .query(QueryRequest {
            find: None,
            node: None,
            kind: Some("operation".to_string()),
        })
        .await
        .expect("the query crosses the wire");
    assert!(
        outcome.handles.iter().any(|handle| handle.name == "verify"),
        "the handles carry the operations"
    );

    // The refusal class survives the crossing: the 403 the server mapped comes
    // back as the typed gate error.
    let error = client
        .implement(ImplementRequest {
            method: "verify".to_string(),
            body: "todo!()".to_string(),
            port: None,
            adapter: None,
        })
        .await
        .expect_err("the gate refuses");
    assert!(
        error.downcast_ref::<theseus::Refused>().is_some(),
        "the refusal should come back typed: {error}"
    );

    let error = client
        .diff(SnapshotRef {
            reference: "not-a-snapshot".to_string(),
        })
        .await
        .expect_err("the checkpoint diff gate refuses");
    assert!(
        error.downcast_ref::<theseus::Refused>().is_some(),
        "the diff refusal should come back typed: {error}"
    );
}

#[tokio::test]
async fn the_unimplemented_class_survives_the_crossing() {
    /// A service left entirely on its trait defaults.
    struct Bare;

    #[async_trait::async_trait]
    impl TheseusService for Bare {}

    let client = serve(Bare).await;
    let error = client
        .verify()
        .await
        .expect_err("the trait default reports");
    assert!(
        error.downcast_ref::<theseus::Unimplemented>().is_some(),
        "the 501 should come back as the typed default: {error}"
    );
}

#[tokio::test]
async fn a_failed_check_report_crosses_as_a_result() {
    let report = serve(FailedCheck)
        .await
        .check()
        .await
        .expect("a completed failing check is still a typed result");
    assert!(!report.ok);
    assert_eq!(report.detail, "compile failure over HTTP");
}

#[tokio::test]
async fn a_structured_implement_result_crosses_the_wire() {
    let result = serve(FailedCheck)
        .await
        .implement(ImplementRequest {
            method: "verify".to_string(),
            body: "todo!()".to_string(),
            port: None,
            adapter: None,
        })
        .await
        .expect("the structured implement result crosses HTTP");
    assert!(!result.applied);
    assert_eq!(result.path, "rust/service.rs");
    assert_eq!(result.detail, "compile gate rolled the edit back");
    assert!(!result.check.ok);
    assert_eq!(result.check.detail, "implement failure over HTTP");
}

#[tokio::test]
async fn checkpoint_lifecycle_requests_cross_http() {
    let client = serve(CheckpointEcho).await;

    let diff = client
        .diff(SnapshotRef {
            reference: "snapshot-a".to_string(),
        })
        .await
        .expect("the diff reference crosses HTTP");
    assert_eq!(diff, "diffed snapshot-a");

    let pruned = client
        .prune(SnapshotRetention { keep: 7 })
        .await
        .expect("the retention limit crosses HTTP");
    assert_eq!(pruned, "kept 7");

    let released = client
        .release(SnapshotRef {
            reference: "snapshot-b".to_string(),
        })
        .await
        .expect("the release reference crosses HTTP");
    assert_eq!(released, "released snapshot-b");
}

#[tokio::test]
async fn rollback_adopts_the_snapshot_model_across_http() {
    let one_shot = Standalone::new(false).expect("Theseus project context is valid");
    let service = StatefulSession::new(
        one_shot.project,
        NoopWorkspace,
        ModelCheckpoint::default(),
        one_shot.calculator,
        NoopToolchain,
        true,
    );
    let client = serve(service).await;
    let reference = client
        .snapshot(SnapshotRequest {
            label: "before HTTP speculation".to_string(),
        })
        .await
        .expect("the snapshot crosses HTTP");
    client
        .patch(PatchRequest {
            edit: vec![Edit::Add {
                parent: "model:theseus".to_string(),
                kind: "type".to_string(),
                name: "HttpAfterSnapshot".to_string(),
                attrs: [("shape".to_string(), "foreign:String".to_string())].into(),
            }],
            write: false,
        })
        .await
        .expect("the speculative patch crosses HTTP");
    let speculative = client
        .query(QueryRequest {
            find: Some("HttpAfterSnapshot".to_string()),
            node: None,
            kind: Some("type".to_string()),
        })
        .await
        .expect("the speculative model is queried over HTTP");
    assert_eq!(speculative.handles.len(), 1);

    client
        .rollback(SnapshotRef { reference })
        .await
        .expect("the rollback crosses HTTP");
    let restored = client
        .query(QueryRequest {
            find: Some("HttpAfterSnapshot".to_string()),
            node: None,
            kind: Some("type".to_string()),
        })
        .await
        .expect("the restored model is queried over HTTP");
    assert!(restored.handles.is_empty());
}
