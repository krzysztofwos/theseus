//! The Theseus HTTP service surface: the generated operation handlers and a
//! router over any implementation of the contract, reusable by the server
//! binary and by tests that compose a server in-process.

pub mod generated;

use std::sync::Arc;

use axum::{
    Router,
    extract::{Json, Path, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::post,
};
use theseus::TheseusService;

/// The HTTP surface over an implementation of the contract: one route serves
/// every operation, `POST /{operation}` with a JSON body, replied with the
/// generated handlers' structural status map.
pub fn router<S: TheseusService + 'static>(service: Arc<S>) -> Router {
    Router::new()
        .route("/{operation}", post(call::<S>))
        .with_state(service)
}

/// Run one operation call through the generated handler and write its reply out.
async fn call<S: TheseusService + 'static>(
    State(service): State<Arc<S>>,
    Path(operation): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let input = body
        .map(|Json(value)| value)
        .unwrap_or_else(|| serde_json::json!({}));
    let reply = generated::handle(service.as_ref(), &operation, &input).await;
    let status = StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, [(CONTENT_TYPE, reply.content_type)], reply.body).into_response()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use theseus::{GatedWorkspace, TheseusService, Toolchain, Workspace};
    use theseus_model::theseus_model;
    use theseus_modeling::GeneratedFile;

    use crate::generated::handle;

    /// A service left entirely on its trait defaults, so every call reports the
    /// typed unimplemented error.
    struct Bare;

    #[async_trait::async_trait]
    impl TheseusService for Bare {}

    /// A workspace that writes nowhere.
    struct NoopWorkspace;

    #[async_trait::async_trait]
    impl Workspace for NoopWorkspace {
        async fn write_file(&self, _file: &GeneratedFile) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A toolchain that reports success without running a build.
    struct StubToolchain;

    #[async_trait::async_trait]
    impl Toolchain for StubToolchain {
        async fn check(&self) -> anyhow::Result<String> {
            Ok("the workspace compiles (stub)".to_string())
        }
    }

    #[tokio::test]
    async fn a_result_is_200() {
        let model = theseus_model();
        let workspace = GatedWorkspace {
            workspace: &NoopWorkspace,
            allow_writes: false,
        };
        let ctx = theseus::Ctx {
            model: &model,
            workspace: &workspace,
            calculator: &theseus_calculator::Calculator,
            toolchain: &StubToolchain,
        };
        let reply = handle(&ctx, "model", &json!({})).await;
        assert_eq!(reply.status, 200);
        assert!(reply.content_type.starts_with("text/plain"));
    }

    #[tokio::test]
    async fn an_unknown_operation_is_404() {
        let reply = handle(&Bare, "warp", &json!({})).await;
        assert_eq!(reply.status, 404);
        assert!(reply.body.contains("unknown operation"));
    }

    #[tokio::test]
    async fn a_body_that_does_not_parse_is_400() {
        let reply = handle(&Bare, "implement", &json!({})).await;
        assert_eq!(reply.status, 400);
        assert!(reply.body.contains("method"));
    }

    #[tokio::test]
    async fn an_unimplemented_operation_is_501() {
        let reply = handle(&Bare, "verify", &json!({})).await;
        assert_eq!(reply.status, 501);
        assert!(reply.body.contains("unimplemented operation"));
    }

    #[tokio::test]
    async fn a_refused_write_is_403() {
        let model = theseus_model();
        let workspace = GatedWorkspace {
            workspace: &NoopWorkspace,
            allow_writes: false,
        };
        let ctx = theseus::Ctx {
            model: &model,
            workspace: &workspace,
            calculator: &theseus_calculator::Calculator,
            toolchain: &StubToolchain,
        };
        let reply = handle(
            &ctx,
            "implement",
            &json!({ "method": "verify", "body": "todo!()" }),
        )
        .await;
        assert_eq!(reply.status, 403);
        assert!(reply.body.contains("not permitted"));
    }
}
