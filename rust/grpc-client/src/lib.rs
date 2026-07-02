//! The Theseus gRPC client — the Client adapter over the Grpc transport (L5).
//!
//! The generated module implements the [`TheseusService`](theseus::TheseusService)
//! contract over the wire's generated stub: each call converts its request to
//! the proto message — the `Edit` oneof carries a patch verb by verb — and a
//! status maps back onto the contract's error classes. A composition root wires
//! it where an in-process adapter would stand.

pub mod generated;

pub use generated::GrpcTheseusClient;
