//! The wire round trip: the generated client through the generated server, over
//! a real socket, preserves the contract — results and error classes alike.

use std::sync::Arc;

use theseus::{ImplementRequest, QueryRequest, Standalone, TheseusService};
use theseus_http_client::HttpTheseusClient;

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
