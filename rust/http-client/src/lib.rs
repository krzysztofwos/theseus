//! The Theseus HTTP client — the Client adapter over the Http transport (L5).
//!
//! The generated module implements the [`TheseusService`](theseus::TheseusService)
//! contract over the wire: each call posts its request as a JSON body, and the
//! reply's status maps back onto the contract's error classes — 501 the typed
//! unimplemented default, 403 the typed refusal. A composition root wires it
//! where an in-process adapter would stand.

pub mod generated;

pub use generated::HttpTheseusClient;
