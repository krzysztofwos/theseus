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
use thiserror::Error;

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

/// A [`Checkpoint`] over the repository's git history: a snapshot is a stash
/// commit of the working tree, and a restore points the tree back at one. The
/// shared checkpoint adapter for the inbound binaries. Both operate on tracked
/// files.
pub struct GitCheckpoint {
    root: PathBuf,
}

/// A full Git object ID. Snapshot references deliberately do not accept
/// symbolic or abbreviated revisions, so Git never interprets caller input as
/// command-line syntax.
#[derive(Clone, Debug, Eq, PartialEq)]
struct GitObjectId(String);

impl GitObjectId {
    fn as_str(&self) -> &str {
        &self.0
    }

    fn into_string(self) -> String {
        self.0
    }
}

impl TryFrom<&str> for GitObjectId {
    type Error = InvalidGitObjectId;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let is_full_length = matches!(value.len(), 40 | 64);
        if is_full_length && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidGitObjectId)
        }
    }
}

#[derive(Debug, Error)]
#[error("expected a full 40- or 64-character hexadecimal Git object ID")]
struct InvalidGitObjectId;

#[derive(Debug, Error)]
enum GitCheckpointError {
    #[error("invalid snapshot reference {reference:?}: {source}")]
    InvalidReference {
        reference: String,
        #[source]
        source: InvalidGitObjectId,
    },
    #[error("snapshot reference {reference} does not name an existing commit")]
    UnknownCommit { reference: String },
    #[error("Git returned an invalid object ID from `{operation}`: {output:?}: {source}")]
    InvalidOutput {
        operation: &'static str,
        output: String,
        #[source]
        source: InvalidGitObjectId,
    },
    #[error("could not run `{operation}`")]
    Launch {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("`{operation}` failed: {message}")]
    CommandFailed {
        operation: &'static str,
        message: String,
    },
}

impl GitCheckpointError {
    fn invalid_reference(reference: &str, source: InvalidGitObjectId) -> Self {
        Self::InvalidReference {
            reference: reference.to_owned(),
            source,
        }
    }

    fn invalid_output(operation: &'static str, output: &str, source: InvalidGitObjectId) -> Self {
        Self::InvalidOutput {
            operation,
            output: output.to_owned(),
            source,
        }
    }

    fn command_failed(operation: &'static str, output: &std::process::Output) -> Self {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        let message = if stderr.is_empty() {
            output.status.to_string()
        } else {
            stderr.to_owned()
        };
        Self::CommandFailed { operation, message }
    }
}

impl GitCheckpoint {
    /// A checkpoint rooted at the repository root.
    pub fn at_repo_root() -> Self {
        Self {
            root: workspace_root(),
        }
    }

    async fn commit(&self, reference: &str) -> Result<GitObjectId, GitCheckpointError> {
        let object_id = GitObjectId::try_from(reference)
            .map_err(|source| GitCheckpointError::invalid_reference(reference, source))?;
        let commit = format!("{}^{{commit}}", object_id.as_str());
        let output = tokio::process::Command::new("git")
            .args(["cat-file", "-e", &commit])
            .current_dir(&self.root)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git cat-file",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::UnknownCommit {
                reference: object_id.into_string(),
            });
        }
        Ok(object_id)
    }

    fn snapshot_id(
        operation: &'static str,
        output: &std::process::Output,
    ) -> Result<GitObjectId, GitCheckpointError> {
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed(operation, output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let snapshot_id = stdout.trim();
        GitObjectId::try_from(snapshot_id)
            .map_err(|source| GitCheckpointError::invalid_output(operation, snapshot_id, source))
    }
}

#[async_trait::async_trait]
impl Checkpoint for GitCheckpoint {
    async fn diff(&self, request: &str) -> anyhow::Result<String> {
        let reference = self.commit(request).await?;
        let output = tokio::process::Command::new("git")
            .args(["diff", "--no-ext-diff", "--no-textconv"])
            .arg(reference.as_str())
            .args(["--", "."])
            .current_dir(&self.root)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git diff",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed("git diff", &output).into());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    async fn snapshot(&self, request: &str) -> anyhow::Result<String> {
        // The stash commit is unreferenced: it lives for the gc grace period,
        // which covers a session's checkpoint-and-rollback horizon.
        // Prefix the caller's label so it is always one positional message and
        // can never be parsed as an option by `git stash create`.
        let message = format!("Theseus checkpoint: {request}");
        let output = tokio::process::Command::new("git")
            .args(["stash", "create"])
            .arg(message)
            .current_dir(&self.root)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git stash create",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed("git stash create", &output).into());
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stash_id = stdout.trim();
        if stash_id.is_empty() {
            // A clean tree snapshots HEAD.
            let head = tokio::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&self.root)
                .kill_on_drop(true)
                .output()
                .await
                .map_err(|source| GitCheckpointError::Launch {
                    operation: "git rev-parse HEAD",
                    source,
                })?;
            Ok(Self::snapshot_id("git rev-parse HEAD", &head)?.into_string())
        } else {
            Ok(GitObjectId::try_from(stash_id)
                .map_err(|source| {
                    GitCheckpointError::invalid_output("git stash create", stash_id, source)
                })?
                .into_string())
        }
    }

    async fn restore(&self, request: &str) -> anyhow::Result<String> {
        let reference = self.commit(request).await?;
        let output = tokio::process::Command::new("git")
            .args(["restore", "--source"])
            .arg(reference.as_str())
            .args(["--staged", "--worktree", "--", "."])
            .current_dir(&self.root)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git restore",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed("git restore", &output).into());
        }
        Ok(format!(
            "restored the working tree to {}",
            reference.as_str()
        ))
    }
}

#[cfg(test)]
mod git_checkpoint_tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    use super::{Checkpoint, GitCheckpoint, GitCheckpointError, GitObjectId};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TempDirectory(PathBuf);

    impl TempDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let name = format!("theseus-checkpoint-{}-{sequence}", std::process::id());
            let path = std::env::temp_dir().join(name);
            fs::create_dir(&path).expect("a temporary directory is created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TestRepository {
        directory: TempDirectory,
        checkpoint: GitCheckpoint,
    }

    impl TestRepository {
        fn new() -> Self {
            let directory = TempDirectory::new();
            git(directory.path(), &["init", "--quiet"]);
            git(directory.path(), &["config", "user.name", "Theseus Test"]);
            git(
                directory.path(),
                &["config", "user.email", "theseus@example.invalid"],
            );
            fs::write(directory.path().join("tracked.txt"), "base\n")
                .expect("the initial file is written");
            git(directory.path(), &["add", "--", "tracked.txt"]);
            git(directory.path(), &["commit", "--quiet", "-m", "initial"]);
            let checkpoint = GitCheckpoint {
                root: directory.path().to_path_buf(),
            };
            Self {
                directory,
                checkpoint,
            }
        }

        fn path(&self, path: &str) -> std::path::PathBuf {
            self.directory.path().join(path)
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn object_ids_must_be_full_hexadecimal_values() {
        assert!(GitObjectId::try_from("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(GitObjectId::try_from(&"a".repeat(64)[..]).is_ok());
        assert!(GitObjectId::try_from("HEAD").is_err());
        assert!(GitObjectId::try_from("0123456").is_err());
        assert!(GitObjectId::try_from(&format!("{}g", "0".repeat(39))[..]).is_err());
    }

    #[tokio::test]
    async fn diff_rejects_an_option_before_git_can_use_it() {
        let repository = TestRepository::new();
        let output_path = repository.path("must-not-be-overwritten");
        fs::write(&output_path, "sentinel\n").expect("the sentinel is written");

        let error = repository
            .checkpoint
            .diff(&format!("--output={}", output_path.display()))
            .await
            .expect_err("a Git option is not a snapshot reference");

        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::InvalidReference { .. })
        ));
        assert_eq!(
            fs::read_to_string(output_path).expect("the sentinel is readable"),
            "sentinel\n"
        );
    }

    #[tokio::test]
    async fn snapshot_ids_round_trip_through_diff_and_restore() {
        let repository = TestRepository::new();
        fs::write(repository.path("tracked.txt"), "snapshot\n")
            .expect("the snapshot content is written");
        let snapshot = repository
            .checkpoint
            .snapshot("round trip")
            .await
            .expect("the working tree is snapshotted");
        GitObjectId::try_from(snapshot.as_str()).expect("snapshot returns a full object ID");

        fs::write(repository.path("tracked.txt"), "after\n").expect("the later content is written");
        let diff = repository
            .checkpoint
            .diff(&snapshot)
            .await
            .expect("the snapshot can be diffed");
        assert!(diff.contains("-snapshot"), "{diff}");
        assert!(diff.contains("+after"), "{diff}");

        repository
            .checkpoint
            .restore(&snapshot)
            .await
            .expect("the snapshot can be restored");
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt"))
                .expect("the restored file is readable"),
            "snapshot\n"
        );
    }

    #[tokio::test]
    async fn a_full_but_unknown_object_id_is_rejected_as_a_commit() {
        let repository = TestRepository::new();
        let missing = "0000000000000000000000000000000000000000";

        let error = repository
            .checkpoint
            .diff(missing)
            .await
            .expect_err("an unknown object is not accepted");

        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownCommit { reference }) if reference == missing
        ));
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
impl
    Standalone<
        GatedWorkspace<FsWorkspace>,
        GatedCheckpoint<GitCheckpoint>,
        theseus_calculator::Calculator,
        CargoToolchain,
    >
{
    pub fn new(allow_writes: bool) -> Self {
        Self {
            model: theseus_model::theseus_model(),
            workspace: GatedWorkspace {
                workspace: FsWorkspace::at_repo_root(),
                allow_writes,
            },
            checkpoint: GatedCheckpoint {
                checkpoint: GitCheckpoint::at_repo_root(),
                allow_writes,
            },
            calculator: theseus_calculator::Calculator,
            toolchain: CargoToolchain,
        }
    }
}
