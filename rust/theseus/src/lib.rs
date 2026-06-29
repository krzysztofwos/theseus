//! The Theseus self-modeling service (L3).
//!
//! [`generated`] holds the model-rendered contract — the [`TheseusService`] trait,
//! the request types, the outbound port traits, and the composition root [`Ctx`].
//! [`service`] is the authored impl. The inbound binaries (`theseus-cli`, and the
//! agent and MCP adapters to come) wire concrete adapters into `Ctx` and drive the
//! contract over a transport.

use std::path::{Path, PathBuf};

mod generated;
mod service;
mod session;

pub use generated::*;
pub use session::Session;

/// The repository root, the directory that holds `rust/`, derived from this
/// crate's compile-time location at `<root>/rust/theseus`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives at <root>/rust/theseus")
        .to_path_buf()
}
