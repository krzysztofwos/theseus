//! The Theseus self-modeling service (L3).
//!
//! [`generated`] holds the model-rendered contract — the [`TheseusService`] trait,
//! the request types, the outbound port traits, and the composition root [`Ctx`].
//! [`service`] is the authored impl. The inbound binaries (`theseus-cli`, and the
//! agent and MCP adapters to come) wire concrete adapters into `Ctx` and drive the
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

/// A workspace port carrying a write permission. A permitted write passes
/// through to the wrapped port and a refused one reports
/// [`Refused`](theseus_modeling::Refused), so every operation that reaches disk
/// through the port is gated the same way. The session and the server inbounds
/// share it.
pub struct GatedWorkspace<'a> {
    pub workspace: &'a dyn Workspace,
    pub allow_writes: bool,
}

impl Workspace for GatedWorkspace<'_> {
    fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        if !self.allow_writes {
            return Err(theseus_modeling::Refused.into());
        }
        self.workspace.write_file(file)
    }
}

/// A [`Toolchain`] that compile-checks the workspace by running `cargo check`
/// at the repository root. The shared toolchain adapter for the inbound binaries.
pub struct CargoToolchain;

impl Toolchain for CargoToolchain {
    fn check(&self) -> anyhow::Result<String> {
        let output = std::process::Command::new("cargo")
            .args(["check", "--workspace", "--quiet"])
            .current_dir(workspace_root())
            .output()
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
fn head(diagnostics: &str) -> String {
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
