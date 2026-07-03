//! The Theseus self-modeling service (L3).
//!
//! [`generated`] holds the model-rendered contract — the [`TheseusService`] trait,
//! the request types, the outbound port traits, and the composition roots: the
//! borrowed [`Ctx`] and the owned [`Standalone`]. [`service`] is the authored
//! impl. The inbound binaries wire concrete adapters into a root and drive the
//! contract over a transport.

use std::path::{Path, PathBuf};

use anyhow::Context;
use theseus_modeling::GeneratedFile;

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

#[async_trait::async_trait]
impl Workspace for FsWorkspace {
    async fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        let path = self.root.join(&file.path);
        // Scaffolding a new crate writes into a directory that does not exist yet.
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &file.contents).await?;
        Ok(())
    }
}

/// A workspace port carrying a write permission. A permitted write passes
/// through to the wrapped port and a refused one reports the contract's
/// [`Refused`], so every operation that reaches disk through the port is gated
/// the same way. It wraps an owned adapter inside a [`Standalone`] and a
/// borrowed one inside a session, the same gate either way.
pub struct GatedWorkspace<W> {
    pub workspace: W,
    pub allow_writes: bool,
}

#[async_trait::async_trait]
impl<W: Workspace> Workspace for GatedWorkspace<W> {
    async fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        if !self.allow_writes {
            return Err(Refused.into());
        }
        self.workspace.write_file(file).await
    }
}

/// A borrowed adapter serves the port its target serves, so a wrapper generic
/// over the port holds a borrow as readily as an owned adapter.
#[async_trait::async_trait]
impl<T: Workspace + ?Sized> Workspace for &T {
    async fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        (**self).write_file(file).await
    }
}

/// A [`Toolchain`] that compile-checks the workspace by running `cargo check`
/// at the repository root. The shared toolchain adapter for the inbound binaries.
/// The check runs as a managed child process, so a server inbound keeps serving
/// while it compiles.
pub struct CargoToolchain;

#[async_trait::async_trait]
impl Toolchain for CargoToolchain {
    async fn test(&self) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("cargo")
            .args(["test", "--workspace", "--quiet"])
            .current_dir(workspace_root())
            .kill_on_drop(true)
            .output()
            .await
            .context("running `cargo test --workspace`")?;
        // With `--quiet` the diagnostic stream carries warnings and errors only.
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        let diagnostics = diagnostics.trim();
        Ok(if output.status.success() {
            if diagnostics.is_empty() {
                "the tests pass".to_string()
            } else {
                format!("the tests pass, with warnings:\n{}", head(diagnostics))
            }
        } else {
            format!("tests failed:\n{}", head(diagnostics))
        })
    }

    async fn check(&self) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("cargo")
            .args(["check", "--workspace", "--quiet"])
            .current_dir(workspace_root())
            .kill_on_drop(true)
            .output()
            .await
            .context("running `cargo check --workspace`")?;
        // With `--quiet` the diagnostic stream carries warnings and errors only.
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        let diagnostics = diagnostics.trim();
        Ok(if output.status.success() {
            if diagnostics.is_empty() {
                "the workspace compiles".to_string()
            } else {
                format!(
                    "the workspace compiles, with warnings:\n{}",
                    head(diagnostics)
                )
            }
        } else {
            format!("check failed:\n{}", head(diagnostics))
        })
    }
}

/// The head of a diagnostic stream, capped so the report stays readable as a
/// tool result. The first diagnostics carry the signal, so the cap keeps the
/// head and counts what it drops.
pub fn head(diagnostics: &str) -> String {
    const CAP: usize = 8_000;
    match diagnostics.char_indices().nth(CAP) {
        None => diagnostics.to_string(),
        Some((byte, _)) => format!(
            "{}\n… truncated ({} more bytes)",
            &diagnostics[..byte],
            diagnostics.len() - byte
        ),
    }
}

/// The repository's own composition for the owned root: the local adapters,
/// writes gated by `allow_writes`.
impl Standalone<GatedWorkspace<FsWorkspace>, theseus_calculator::Calculator, CargoToolchain> {
    pub fn new(allow_writes: bool) -> Self {
        Self {
            model: theseus_model::theseus_model(),
            workspace: GatedWorkspace {
                workspace: FsWorkspace::at_repo_root(),
                allow_writes,
            },
            calculator: theseus_calculator::Calculator,
            toolchain: CargoToolchain,
        }
    }
}
