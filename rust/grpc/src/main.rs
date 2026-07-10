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

use theseus::StatefulSession;
use theseus_grpc::generated::{GrpcTheseus, proto::theseus_server::TheseusServer};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut allow_writes = false;
    let mut address = None;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--allow-writes" => allow_writes = true,
            flag if flag.starts_with("--") => {
                anyhow::bail!(
                    "unknown flag `{flag}`; usage: grpc-server [--allow-writes] [address]"
                )
            }
            _ => address = Some(arg),
        }
    }
    let listen = address.unwrap_or_else(|| "127.0.0.1:4873".to_string());
    let addr = listen.parse()?;
    eprintln!("listening on grpc://{listen}");
    tonic::transport::Server::builder()
        .add_service(TheseusServer::new(GrpcTheseus(
            StatefulSession::at_repo_root(allow_writes)?,
        )))
        .serve(addr)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use theseus::StatefulSession;
    use theseus_grpc::generated::{GrpcTheseus, proto, proto::theseus_server::Theseus as _};

    #[tokio::test]
    async fn a_query_rides_back_as_json() {
        let glue = GrpcTheseus(StatefulSession::at_repo_root(false).unwrap());
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
        let glue = GrpcTheseus(StatefulSession::at_repo_root(false).unwrap());
        let edit = proto::Edit {
            verb: Some(proto::edit::Verb::Add(proto::edit::Add {
                parent: Some("model:theseus".to_string()),
                kind: Some("type".to_string()),
                name: Some("Probe".to_string()),
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

        let query = glue
            .query(tonic::Request::new(proto::QueryRequest {
                find: None,
                node: Some("type:theseus:Probe".to_string()),
                kind: None,
            }))
            .await
            .expect("the subsequent query runs")
            .into_inner()
            .json;
        assert!(query.contains("type:theseus:Probe"), "{query}");
    }

    #[tokio::test]
    async fn an_edit_without_a_verb_is_invalid() {
        let glue = GrpcTheseus(StatefulSession::at_repo_root(false).unwrap());
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
        let glue = GrpcTheseus(StatefulSession::at_repo_root(false).unwrap());
        let status = glue
            .implement(tonic::Request::new(proto::ImplementRequest {
                method: Some("verify".to_string()),
                body: Some("todo!()".to_string()),
                port: None,
                adapter: None,
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
