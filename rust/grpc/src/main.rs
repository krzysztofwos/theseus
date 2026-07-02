//! The `grpc-server` binary: Theseus's operations over gRPC — the Grpc inbound
//! adapter (L4).
//!
//! The generated glue implements the wire's server trait over the service
//! contract and maps each outcome onto a gRPC status: OK a result, UNIMPLEMENTED
//! an operation with no authored handler, PERMISSION_DENIED a refused write,
//! INTERNAL any other error. A typed request decodes on the wire — the `Edit`
//! oneof carries a patch verb by verb — and a response the model holds as a
//! foreign type rides as its JSON rendering. Writes are refused unless the
//! server is launched with `--allow-writes`.

mod generated;

use generated::{GrpcTheseus, proto::theseus_server::TheseusServer};
use theseus::{
    CalcRequest, CargoToolchain, Ctx, FsWorkspace, GatedWorkspace, ImplementRequest, PatchRequest,
    QueryRequest, ShowRequest, TheseusService,
};
use theseus_model::theseus_model;
use theseus_modeling::{
    CoverageReport, GeneratedFile, Model, PatchOutcome, QueryOutcome, VerifyReport,
};

/// The owned adapters each call's composition root borrows: the service contract
/// implemented over a per-call `Ctx`, so the long-lived glue drives the same
/// authored handlers as every other inbound.
struct App {
    model: Model,
    workspace: FsWorkspace,
    toolchain: CargoToolchain,
    calculator: theseus_calculator::Calculator,
    allow_writes: bool,
}

impl App {
    fn new(allow_writes: bool) -> Self {
        Self {
            model: theseus_model(),
            workspace: FsWorkspace::at_repo_root(),
            toolchain: CargoToolchain,
            calculator: theseus_calculator::Calculator,
            allow_writes,
        }
    }

    /// The workspace port carrying this server's write permission.
    fn gate(&self) -> GatedWorkspace<'_> {
        GatedWorkspace {
            workspace: &self.workspace,
            allow_writes: self.allow_writes,
        }
    }

    /// The composition root one call runs over.
    fn ctx<'a>(&'a self, workspace: &'a GatedWorkspace<'a>) -> Ctx<'a> {
        Ctx {
            model: &self.model,
            workspace,
            calculator: &self.calculator,
            toolchain: &self.toolchain,
        }
    }
}

#[async_trait::async_trait]
impl TheseusService for App {
    async fn model(&self) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).model().await
    }

    async fn verify(&self) -> anyhow::Result<VerifyReport> {
        let workspace = self.gate();
        self.ctx(&workspace).verify().await
    }

    async fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let workspace = self.gate();
        self.ctx(&workspace).generate().await
    }

    async fn query(&self, request: QueryRequest) -> anyhow::Result<QueryOutcome> {
        let workspace = self.gate();
        self.ctx(&workspace).query(request).await
    }

    async fn patch(&self, request: PatchRequest) -> anyhow::Result<PatchOutcome> {
        let workspace = self.gate();
        self.ctx(&workspace).patch(request).await
    }

    async fn coverage(&self) -> anyhow::Result<CoverageReport> {
        let workspace = self.gate();
        self.ctx(&workspace).coverage().await
    }

    async fn implement(&self, request: ImplementRequest) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).implement(request).await
    }

    async fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).show(request).await
    }

    async fn check(&self) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).check().await
    }

    async fn calc(&self, request: CalcRequest) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).calc(request).await
    }

    async fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let workspace = self.gate();
        self.ctx(&workspace).scaffold().await
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let allow_writes = std::env::args().skip(1).any(|arg| arg == "--allow-writes");
    let listen = std::env::args()
        .skip(1)
        .find(|arg| arg != "--allow-writes")
        .unwrap_or_else(|| "127.0.0.1:4873".to_string());
    let addr = listen.parse()?;
    eprintln!("listening on grpc://{listen}");
    tonic::transport::Server::builder()
        .add_service(TheseusServer::new(GrpcTheseus(App::new(allow_writes))))
        .serve(addr)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::App;
    use crate::generated::{GrpcTheseus, proto, proto::theseus_server::Theseus as _};

    #[tokio::test]
    async fn a_query_rides_back_as_json() {
        let glue = GrpcTheseus(App::new(false));
        let reply = glue
            .query(tonic::Request::new(proto::QueryRequest {
                find: None,
                node: None,
                kind: Some("operation".to_string()),
            }))
            .await
            .expect("the query runs");
        let json = reply.into_inner().json;
        assert!(json.contains("model_hash"), "{json}");
        assert!(json.contains("op:theseus:verify"), "{json}");
    }

    #[tokio::test]
    async fn a_patch_carries_its_edit_as_a_oneof() {
        let glue = GrpcTheseus(App::new(false));
        let edit = proto::Edit {
            verb: Some(proto::edit::Verb::Add(proto::edit::Add {
                parent: "model:theseus".to_string(),
                kind: "type".to_string(),
                name: "Probe".to_string(),
                attrs: [("shape".to_string(), "foreign:String".to_string())].into(),
            })),
        };
        // No write: the edit is applied and reported, and nothing reprojects.
        let reply = glue
            .patch(tonic::Request::new(proto::PatchRequest {
                edit: vec![edit],
                write: false,
            }))
            .await
            .expect("the patch applies");
        let json = reply.into_inner().json;
        assert!(json.contains(r#""ok":true"#), "{json}");
        assert!(json.contains("Probe"), "{json}");
    }

    #[tokio::test]
    async fn an_edit_without_a_verb_is_invalid() {
        let glue = GrpcTheseus(App::new(false));
        let status = glue
            .patch(tonic::Request::new(proto::PatchRequest {
                edit: vec![proto::Edit { verb: None }],
                write: false,
            }))
            .await
            .expect_err("an empty edit is refused");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[tokio::test]
    async fn a_refused_write_maps_to_permission_denied() {
        let glue = GrpcTheseus(App::new(false));
        let status = glue
            .implement(tonic::Request::new(proto::ImplementRequest {
                method: "verify".to_string(),
                body: "todo!()".to_string(),
            }))
            .await
            .expect_err("the gate refuses");
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[tokio::test]
    async fn an_unimplemented_operation_maps_to_unimplemented() {
        /// A service left entirely on its trait defaults.
        struct Bare;

        #[async_trait::async_trait]
        impl theseus::TheseusService for Bare {}

        let glue = GrpcTheseus(Bare);
        let status = glue
            .verify(tonic::Request::new(proto::Empty {}))
            .await
            .expect_err("the trait default reports");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }
}
