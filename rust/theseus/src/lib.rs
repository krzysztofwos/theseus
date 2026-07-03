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
/// the same way. The session and the server inbounds share it.
pub struct GatedWorkspace<'a> {
    pub workspace: &'a dyn Workspace,
    pub allow_writes: bool,
}

#[async_trait::async_trait]
impl Workspace for GatedWorkspace<'_> {
    async fn write_file(&self, file: &GeneratedFile) -> anyhow::Result<()> {
        if !self.allow_writes {
            return Err(theseus_modeling::Refused.into());
        }
        self.workspace.write_file(file).await
    }
}

/// A [`Toolchain`] that compile-checks the workspace by running `cargo check`
/// at the repository root. The shared toolchain adapter for the inbound binaries.
/// The check runs as a managed child process, so a server inbound keeps serving
/// while it compiles.
pub struct CargoToolchain;

#[async_trait::async_trait]
impl Toolchain for CargoToolchain {
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

/// The service over its own local adapters: an owned composition root for a
/// long-lived inbound that cannot hold a borrowed [`Ctx`]. Each call runs over
/// a fresh `Ctx` built on the gated workspace, so a standalone value drives the
/// same authored handlers as every other inbound.
pub struct Standalone {
    model: theseus_modeling::Model,
    workspace: FsWorkspace,
    toolchain: CargoToolchain,
    calculator: theseus_calculator::Calculator,
    allow_writes: bool,
}

impl Standalone {
    /// The service over the repository's adapters, writes gated by `allow_writes`.
    pub fn new(allow_writes: bool) -> Self {
        Self {
            model: theseus_model::theseus_model(),
            workspace: FsWorkspace::at_repo_root(),
            toolchain: CargoToolchain,
            calculator: theseus_calculator::Calculator,
            allow_writes,
        }
    }

    /// The workspace port carrying this root's write permission.
    fn gate(&self) -> GatedWorkspace<'_> {
        GatedWorkspace {
            workspace: &self.workspace,
            allow_writes: self.allow_writes,
        }
    }

    /// The composition root one call runs over.
    fn ctx<'a>(&'a self, workspace: &'a GatedWorkspace<'a>) -> Ctx<'a> {
        Ctx {
            model: &self.model,
            workspace,
            calculator: &self.calculator,
            toolchain: &self.toolchain,
        }
    }
}

#[async_trait::async_trait]
impl TheseusService for Standalone {
    async fn model(&self) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).model().await
    }

    async fn verify(&self) -> anyhow::Result<theseus_modeling::VerifyReport> {
        let workspace = self.gate();
        self.ctx(&workspace).verify().await
    }

    async fn generate(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let workspace = self.gate();
        self.ctx(&workspace).generate().await
    }

    async fn query(&self, request: QueryRequest) -> anyhow::Result<theseus_modeling::QueryOutcome> {
        let workspace = self.gate();
        self.ctx(&workspace).query(request).await
    }

    async fn patch(&self, request: PatchRequest) -> anyhow::Result<theseus_modeling::PatchOutcome> {
        let workspace = self.gate();
        self.ctx(&workspace).patch(request).await
    }

    async fn coverage(&self) -> anyhow::Result<theseus_modeling::CoverageReport> {
        let workspace = self.gate();
        self.ctx(&workspace).coverage().await
    }

    async fn implement(&self, request: ImplementRequest) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).implement(request).await
    }

    async fn show(&self, request: ShowRequest) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).show(request).await
    }

    async fn check(&self) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).check().await
    }

    async fn calc(&self, request: CalcRequest) -> anyhow::Result<String> {
        let workspace = self.gate();
        self.ctx(&workspace).calc(request).await
    }

    async fn scaffold(&self) -> anyhow::Result<Vec<GeneratedFile>> {
        let workspace = self.gate();
        self.ctx(&workspace).scaffold().await
    }
}
