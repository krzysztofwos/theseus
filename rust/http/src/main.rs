//! The `http-server` binary: Theseus's operations over HTTP — the Http inbound
//! adapter (L4).
//!
//! One route serves every operation, `POST /{operation}` with a JSON body. The
//! generated handlers parse the body into the operation's request, run it against
//! the composition root, and map the outcome onto the status line: 200 a result,
//! 400 a body that does not parse, 404 an unknown operation, 501 an operation
//! with no authored handler, 403 a write the gate refused, 500 any other error.
//! Writes are refused unless the server is launched with `--allow-writes`.

mod generated;

use std::sync::Arc;

use axum::{
    Router,
    extract::{Json, Path, State},
    http::{StatusCode, header::CONTENT_TYPE},
    response::{IntoResponse, Response},
    routing::post,
};
use theseus::{CargoToolchain, Ctx, FsWorkspace, GatedWorkspace};
use theseus_model::theseus_model;
use theseus_modeling::Model;

/// The owned adapters each request's composition root borrows.
struct App {
    model: Model,
    workspace: FsWorkspace,
    toolchain: CargoToolchain,
    calculator: theseus_calculator::Calculator,
    allow_writes: bool,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let allow_writes = std::env::args().skip(1).any(|arg| arg == "--allow-writes");
    let listen = std::env::args()
        .skip(1)
        .find(|arg| arg != "--allow-writes")
        .unwrap_or_else(|| "127.0.0.1:4870".to_string());
    let app = Arc::new(App {
        model: theseus_model(),
        workspace: FsWorkspace::at_repo_root(),
        toolchain: CargoToolchain,
        calculator: theseus_calculator::Calculator,
        allow_writes,
    });
    let router = Router::new()
        .route("/{operation}", post(call))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("listening on http://{listen}");
    axum::serve(listener, router).await?;
    Ok(())
}

/// Run one operation call: build the composition root over the gated workspace,
/// hand the call to the generated handler, and write its reply out. The core is
/// synchronous, so a long operation holds its connection open until it reports.
async fn call(
    State(app): State<Arc<App>>,
    Path(operation): Path<String>,
    body: Option<Json<serde_json::Value>>,
) -> Response {
    let input = body
        .map(|Json(value)| value)
        .unwrap_or_else(|| serde_json::json!({}));
    let workspace = GatedWorkspace {
        workspace: &app.workspace,
        allow_writes: app.allow_writes,
    };
    let ctx = Ctx {
        model: &app.model,
        workspace: &workspace,
        calculator: &app.calculator,
        toolchain: &app.toolchain,
    };
    let reply = generated::handle(&ctx, &operation, &input);
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

    impl TheseusService for Bare {}

    /// A workspace that writes nowhere.
    struct NoopWorkspace;

    impl Workspace for NoopWorkspace {
        fn write_file(&self, _file: &GeneratedFile) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// A toolchain that reports success without running a build.
    struct StubToolchain;

    impl Toolchain for StubToolchain {
        fn check(&self) -> anyhow::Result<String> {
            Ok("the workspace compiles (stub)".to_string())
        }
    }

    #[test]
    fn a_result_is_200() {
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
        let reply = handle(&ctx, "model", &json!({}));
        assert_eq!(reply.status, 200);
        assert!(reply.content_type.starts_with("text/plain"));
    }

    #[test]
    fn an_unknown_operation_is_404() {
        let reply = handle(&Bare, "warp", &json!({}));
        assert_eq!(reply.status, 404);
        assert!(reply.body.contains("unknown operation"));
    }

    #[test]
    fn a_body_that_does_not_parse_is_400() {
        let reply = handle(&Bare, "implement", &json!({}));
        assert_eq!(reply.status, 400);
        assert!(reply.body.contains("method"));
    }

    #[test]
    fn an_unimplemented_operation_is_501() {
        let reply = handle(&Bare, "verify", &json!({}));
        assert_eq!(reply.status, 501);
        assert!(reply.body.contains("unimplemented operation"));
    }

    #[test]
    fn a_refused_write_is_403() {
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
        );
        assert_eq!(reply.status, 403);
        assert!(reply.body.contains("not permitted"));
    }
}
