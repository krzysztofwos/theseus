//! The wire round trip: the generated client through the generated glue, over
//! a real socket, preserves the Calculator contract.

use theseus_calculator::{CalculatorService, Operands};
use theseus_calculator_grpc::generated::{
    GrpcCalculator, proto::calculator_server::CalculatorServer,
};
use theseus_calculator_grpc_client::GrpcCalculatorClient;

/// Serve the glue on an ephemeral port and return a client against it.
async fn serve<S: CalculatorService + 'static>(service: S) -> GrpcCalculatorClient {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral port binds");
    let addr = listener.local_addr().expect("the bound address reads");
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(CalculatorServer::new(GrpcCalculator(service)))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("the server runs");
    });
    GrpcCalculatorClient::connect(format!("http://{addr}"))
        .await
        .expect("the client connects")
}

#[tokio::test]
async fn the_wire_crossing_preserves_the_contract() {
    let client = serve(theseus_calculator::Calculator).await;
    let sum = client
        .add(Operands { a: 2.0, b: 3.0 })
        .await
        .expect("the addition crosses the wire");
    assert_eq!(sum, "5");
}

#[tokio::test]
async fn the_unimplemented_class_survives_the_crossing() {
    /// A service left entirely on its trait defaults.
    struct Bare;

    #[async_trait::async_trait]
    impl CalculatorService for Bare {}

    let client = serve(Bare).await;
    let error = client
        .add(Operands { a: 2.0, b: 3.0 })
        .await
        .expect_err("the trait default reports");
    assert!(
        error
            .downcast_ref::<theseus_calculator::Unimplemented>()
            .is_some(),
        "the UNIMPLEMENTED status should come back as the typed default: {error}"
    );
}
