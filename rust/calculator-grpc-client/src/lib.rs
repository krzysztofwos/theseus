//! The calculator gRPC client — the Client adapter over the Grpc transport (L2).
//!
//! The generated module implements the
//! [`CalculatorService`](theseus_calculator::CalculatorService) contract over
//! the wire's generated stub. A composition root wires it where the in-process
//! adapter would stand, so a consumer reaches a remote calculator through the
//! same port.

pub mod generated;

pub use generated::GrpcCalculatorClient;
