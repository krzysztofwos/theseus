//! The wire round trip: the generated client through the generated glue, over
//! a real socket, preserves the contract — the `Edit` oneof crosses typed, and
//! the error classes come back as the types the server mapped.

use theseus::{ImplementRequest, PatchRequest, QueryRequest, Standalone, TheseusService};
use theseus_grpc::generated::{GrpcTheseus, proto::theseus_server::TheseusServer};
use theseus_grpc_client::GrpcTheseusClient;
use theseus_modeling::Edit;

/// Serve the glue on an ephemeral port and return a client against it.
async fn serve<S: TheseusService + 'static>(service: S) -> GrpcTheseusClient {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port binds");
    let addr = listener.local_addr().expect("the bound address reads");
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(TheseusServer::new(GrpcTheseus(service)))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("the server runs");
    });
    GrpcTheseusClient::connect(format!("http://{addr}"))
        .await
        .expect("the client connects")
}

#[tokio::test]
async fn the_wire_crossing_preserves_the_contract() {
    let client = serve(Standalone::new(false)).await;

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

    // The Edit oneof crosses verb by verb: a structured patch applies remotely.
    let outcome = client
        .patch(PatchRequest {
            edit: vec![Edit::Add {
                parent: "model:theseus".to_string(),
                kind: "type".to_string(),
                name: "Probe".to_string(),
                attrs: [("shape".to_string(), "foreign:String".to_string())].into(),
            }],
            write: false,
        })
        .await
        .expect("the patch crosses the wire");
    assert!(outcome.ok, "the edit applies: {:?}", outcome.diagnostics);
    assert!(
        outcome.diff.iter().any(|line| line.contains("Probe")),
        "the diff names the new type: {:?}",
        outcome.diff
    );

    // The refusal class survives the crossing.
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
        "the UNIMPLEMENTED status should come back as the typed default: {error}"
    );
}
