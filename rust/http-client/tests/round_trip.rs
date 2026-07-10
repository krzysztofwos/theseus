//! The wire round trip: the generated client through the generated server, over
//! a real socket, preserves the contract — results and error classes alike.

use std::sync::Arc;

use theseus::{ImplementRequest, QueryRequest, Standalone, TheseusService};
use theseus_http_client::HttpTheseusClient;

struct FailedCheck;

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
