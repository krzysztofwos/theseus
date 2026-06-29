//! The Theseus self-modeling service (L3).
//!
//! [`generated`] holds the model-rendered contract — the [`TheseusService`] trait,
//! the request types, the outbound port traits, and the composition root [`Ctx`].
//! [`service`] is the authored impl. The inbound binaries (`theseus-cli`, and the
//! agent and MCP adapters to come) wire concrete adapters into `Ctx` and drive the
//! contract over a transport.

use std::path::{Path, PathBuf};

use theseus_modeling::GeneratedFile;

mod generated;
mod service;
mod session;

pub use generated::*;
pub use session::{Session, tool_catalog};

/// The repository root, the directory that holds `rust/`, derived from this
/// crate's compile-time location at `<root>/rust/theseus`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives at <root>/rust/theseus")
        .to_path_buf()
}

/// A [`Workspace`] that writes generated files relative to a root directory. The
/// shared filesystem adapter for the inbound binaries.
pub struct FsWorkspace {
    root: PathBuf,
}

impl FsWorkspace {
    /// A workspace rooted at the repository root.
    pub fn at_repo_root() -> Self {
        Self {
            root: workspace_root(),
        }
    }
}

impl Workspace for FsWorkspace {
    fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        let path = self.root.join(&file.path);
        // Scaffolding a new crate writes into a directory that does not exist yet.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &file.contents)?;
        Ok(())
    }
}
