//! The `calculator-grpc` binary: the Calculator service over gRPC — the Grpc
//! inbound adapter (L2).
//!
//! The generated glue implements the wire's server trait over the service
//! contract and maps each outcome onto a gRPC status: OK a result, UNIMPLEMENTED
//! an operation with no authored handler, PERMISSION_DENIED a refused write,
//! INTERNAL any other error. The typed wire eliminates the malformed-request
//! class an HTTP body carries — a request that decodes is a request that parses.

use theseus_calculator_grpc::generated::{
    GrpcCalculator, proto::calculator_server::CalculatorServer,
};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let mut address = None;
    for arg in std::env::args().skip(1) {
        if arg.starts_with("--") {
            anyhow::bail!("unknown flag `{arg}`; usage: calculator-grpc [address]");
        }
        address = Some(arg);
    }
    let listen = address.unwrap_or_else(|| "127.0.0.1:4872".to_string());
    let addr = listen.parse()?;
    eprintln!("listening on grpc://{listen}");
    tonic::transport::Server::builder()
        .add_service(CalculatorServer::new(GrpcCalculator(
            theseus_calculator::Calculator,
        )))
        .serve(addr)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use theseus_calculator_grpc::generated::{
        GrpcCalculator, proto, proto::calculator_server::Calculator as _,
    };

    #[tokio::test]
    async fn a_result_maps_to_ok() {
        let glue = GrpcCalculator(theseus_calculator::Calculator);
        let reply = glue
            .add(tonic::Request::new(proto::Operands { a: 2.0, b: 3.0 }))
            .await
            .expect("the operation runs");
        assert_eq!(reply.into_inner().value, "5");
    }

    #[tokio::test]
    async fn an_unimplemented_operation_maps_to_unimplemented() {
        /// A service left entirely on its trait defaults.
        struct Bare;

        #[async_trait::async_trait]
        impl theseus_calculator::CalculatorService for Bare {}

        let glue = GrpcCalculator(Bare);
        let status = glue
            .add(tonic::Request::new(proto::Operands { a: 2.0, b: 3.0 }))
            .await
            .expect_err("the trait default reports");
        assert_eq!(status.code(), tonic::Code::Unimplemented);
    }
}
