//! The Theseus self-modeling service (L3).
//!
//! [`generated`] holds the model-rendered contract — the [`TheseusService`] trait,
//! the request types, the outbound port traits, and the composition roots: the
//! borrowed [`Ctx`] and the owned [`Standalone`]. [`service`] is the authored
//! impl. The inbound binaries wire concrete adapters into a root and drive the
//! contract over a transport.

extern crate self as theseus;

use std::path::{Path, PathBuf};

use anyhow::Context;
use thiserror::Error;

mod check_report;
mod generated;
mod implement_result;
mod service;
mod session;
mod stateful;

pub use check_report::CheckReport;
pub use generated::*;
pub use implement_result::ImplementResult;
pub use session::{Session, SessionState};
pub use stateful::StatefulSession;
pub use theseus_workspace::{
    ExpectedFile, ExpectedFileSet, FsMutation, MutationError, MutationFile, PendingMutation,
    WorkspaceMutation,
};

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
    async fn begin_mutation(&self, expected: &ExpectedFileSet) -> anyhow::Result<PendingMutation> {
        Ok(FsMutation::begin_async(self.root.clone(), expected.clone()).await?)
    }
}

/// A [`Checkpoint`] over the repository's git history: a snapshot is a pinned
/// stash commit of the working tree, and a restore points the tree back at one.
/// The shared checkpoint adapter for the inbound binaries. Both operate on
/// tracked files.
pub struct GitCheckpoint {
    root: PathBuf,
}

const SNAPSHOT_REF_PREFIX: &str = "refs/theseus/snapshots";
const MAX_SNAPSHOT_LABEL_BYTES: usize = 256;

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
            Ok(Self(value.to_ascii_lowercase()))
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
    #[error("snapshot label is {length} bytes; the maximum is {maximum}")]
    LabelTooLong { length: usize, maximum: usize },
    #[error("invalid snapshot reference {reference:?}: {source}")]
    InvalidReference {
        reference: String,
        #[source]
        source: InvalidGitObjectId,
    },
    #[error("snapshot reference {reference} is not pinned by Theseus")]
    UnknownSnapshot { reference: String },
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
    #[error(transparent)]
    Mutation(#[from] MutationError),
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

    async fn repository_lease(&self) -> Result<PendingMutation, GitCheckpointError> {
        Ok(FsMutation::begin_async(self.root.clone(), Vec::new()).await?)
    }

    fn validate_label(label: &str) -> Result<(), GitCheckpointError> {
        if label.len() > MAX_SNAPSHOT_LABEL_BYTES {
            return Err(GitCheckpointError::LabelTooLong {
                length: label.len(),
                maximum: MAX_SNAPSHOT_LABEL_BYTES,
            });
        }
        Ok(())
    }

    fn snapshot_ref(object_id: &GitObjectId) -> String {
        format!("{SNAPSHOT_REF_PREFIX}/{}", object_id.as_str())
    }

    async fn pinned_commit(&self, reference: &str) -> Result<GitObjectId, GitCheckpointError> {
        let object_id = GitObjectId::try_from(reference)
            .map_err(|source| GitCheckpointError::invalid_reference(reference, source))?;
        let commit = format!("{}^{{commit}}", Self::snapshot_ref(&object_id));
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", "--verify"])
            .arg(commit)
            .current_dir(&self.root)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git rev-parse snapshot",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::UnknownSnapshot {
                reference: object_id.into_string(),
            });
        }
        let pinned = Self::snapshot_id("git rev-parse snapshot", &output)?;
        if pinned != object_id {
            return Err(GitCheckpointError::UnknownSnapshot {
                reference: object_id.into_string(),
            });
        }
        Ok(object_id)
    }

    async fn pin(&self, object_id: &GitObjectId) -> Result<(), GitCheckpointError> {
        let reference = Self::snapshot_ref(object_id);
        let output = tokio::process::Command::new("git")
            .args(["update-ref"])
            .arg(reference)
            .arg(object_id.as_str())
            .current_dir(&self.root)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git update-ref",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed(
                "git update-ref",
                &output,
            ));
        }
        Ok(())
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
        let lease = self.repository_lease().await?;
        let reference = self.pinned_commit(request).await?;
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
        let diff = String::from_utf8_lossy(&output.stdout).into_owned();
        lease.commit().map_err(GitCheckpointError::from)?;
        Ok(diff)
    }

    async fn snapshot(&self, request: &str) -> anyhow::Result<String> {
        Self::validate_label(request)?;
        let lease = self.repository_lease().await?;
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
        let snapshot = if stash_id.is_empty() {
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
            Self::snapshot_id("git rev-parse HEAD", &head)?
        } else {
            GitObjectId::try_from(stash_id).map_err(|source| {
                GitCheckpointError::invalid_output("git stash create", stash_id, source)
            })?
        };
        self.pin(&snapshot).await?;
        lease.commit().map_err(GitCheckpointError::from)?;
        Ok(snapshot.into_string())
    }

    async fn restore(&self, request: &str) -> anyhow::Result<String> {
        let lease = self.repository_lease().await?;
        let reference = self.pinned_commit(request).await?;
        // Restoration, lease release, and the caller's model adoption form one
        // cancellation-free poll. Git restore is short and owns the repository
        // lease, so blocking here is preferable to disk/model divergence.
        let output = std::process::Command::new("git")
            .args(["restore", "--source"])
            .arg(reference.as_str())
            .args(["--staged", "--worktree", "--", "."])
            .current_dir(&self.root)
            .output()
            .map_err(|source| GitCheckpointError::Launch {
                operation: "git restore",
                source,
            })?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed("git restore", &output).into());
        }
        lease.commit().map_err(GitCheckpointError::from)?;
        Ok(format!(
            "restored the working tree to {}",
            reference.as_str()
        ))
    }
}

/// A [`Toolchain`] that compile-checks the workspace by running `cargo check`
/// at the repository root. The shared toolchain adapter for the inbound binaries.
/// The check runs as a managed child process, so a server inbound keeps serving
/// while it compiles.
pub struct CargoToolchain;

#[async_trait::async_trait]
impl Toolchain for CargoToolchain {
    async fn lint(&self) -> anyhow::Result<CheckReport> {
        run_cargo_under_lease(
            &[
                "clippy",
                "--workspace",
                "--quiet",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
            "cargo clippy --workspace --locked -- -D warnings",
            "clippy: no warnings or errors",
            "clippy: clean (with notes)",
            "clippy: warnings or errors found",
        )
        .await
    }

    async fn test(&self) -> anyhow::Result<CheckReport> {
        run_cargo_under_lease(
            &["test", "--workspace", "--quiet", "--locked"],
            "cargo test --workspace --locked",
            "the tests pass",
            "the tests pass, with warnings",
            "tests failed",
        )
        .await
    }

    async fn check(&self) -> anyhow::Result<CheckReport> {
        run_cargo_under_lease(
            &["check", "--workspace", "--quiet", "--locked"],
            "cargo check --workspace --locked",
            "the workspace compiles",
            "the workspace compiles, with warnings",
            "check failed",
        )
        .await
    }

    async fn check_mutation(&self) -> anyhow::Result<CheckReport> {
        run_cargo(
            &["check", "--workspace", "--quiet"],
            "cargo check --workspace",
            "the workspace compiles",
            "the workspace compiles, with warnings",
            "check failed",
        )
        .await
    }
}

async fn run_cargo_under_lease(
    args: &[&str],
    operation: &'static str,
    success: &'static str,
    success_with_notes: &'static str,
    failure: &'static str,
) -> anyhow::Result<CheckReport> {
    let lease = FsMutation::begin_async(workspace_root(), Vec::new()).await?;
    let report = run_cargo(args, operation, success, success_with_notes, failure).await?;
    lease.commit()?;
    Ok(report)
}

async fn run_cargo(
    args: &[&str],
    operation: &'static str,
    success: &'static str,
    success_with_notes: &'static str,
    failure: &'static str,
) -> anyhow::Result<CheckReport> {
    let output = tokio::process::Command::new("cargo")
        .args(args)
        .current_dir(workspace_root())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| format!("running `{operation}`"))?;
    Ok(report_from_output(
        &output,
        success,
        success_with_notes,
        failure,
    ))
}

fn report_from_output(
    output: &std::process::Output,
    success: &str,
    success_with_notes: &str,
    failure: &str,
) -> CheckReport {
    report_from_streams(
        output.status.success(),
        &output.stdout,
        &output.stderr,
        success,
        success_with_notes,
        failure,
    )
}

fn report_from_streams(
    ok: bool,
    stdout: &[u8],
    stderr: &[u8],
    success: &str,
    success_with_notes: &str,
    failure: &str,
) -> CheckReport {
    let stderr = String::from_utf8_lossy(stderr);
    let stderr = stderr.trim();
    if ok {
        return CheckReport::success(if stderr.is_empty() {
            success.to_string()
        } else {
            format!("{success_with_notes}:\n{}", head(stderr))
        });
    }

    let stdout = String::from_utf8_lossy(stdout);
    let diagnostics = [stderr, stdout.trim()]
        .into_iter()
        .filter(|stream| !stream.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    CheckReport::failure(if diagnostics.is_empty() {
        failure.to_string()
    } else {
        format!("{failure}:\n{}", head(&diagnostics))
    })
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

#[cfg(test)]
mod check_report_tests {
    use super::report_from_streams;

    #[test]
    fn a_completed_command_reports_its_status_structurally() {
        let success = report_from_streams(
            true,
            b"ignored normal output",
            b"",
            "success",
            "success with notes",
            "failure",
        );
        assert!(success.ok);
        assert_eq!(success.detail, "success");

        let failure = report_from_streams(
            false,
            b"test assertion failed on stdout",
            b"compiler diagnostic on stderr",
            "success",
            "success with notes",
            "failure",
        );
        assert!(!failure.ok);
        assert!(failure.detail.contains("compiler diagnostic on stderr"));
        assert!(failure.detail.contains("test assertion failed on stdout"));
    }

    #[test]
    fn successful_diagnostics_are_preserved_as_notes() {
        let report = report_from_streams(
            true,
            b"",
            b"a warning",
            "success",
            "success with notes",
            "failure",
        );
        assert!(report.ok);
        assert_eq!(report.detail, "success with notes:\na warning");
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

    use super::{
        Checkpoint, FsMutation, GitCheckpoint, GitCheckpointError, GitObjectId,
        MAX_SNAPSHOT_LABEL_BYTES, SNAPSHOT_REF_PREFIX,
    };

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

    fn git_stdout(root: &Path, args: &[&str]) -> String {
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
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
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
        assert_eq!(
            git_stdout(
                repository.directory.path(),
                &[
                    "show-ref",
                    "--verify",
                    "--hash",
                    &format!("{SNAPSHOT_REF_PREFIX}/{snapshot}"),
                ],
            ),
            snapshot
        );

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
    async fn an_existing_but_unpinned_commit_is_rejected() {
        let repository = TestRepository::new();
        let unpinned = git_stdout(repository.directory.path(), &["rev-parse", "HEAD"]);

        let error = repository
            .checkpoint
            .diff(&unpinned)
            .await
            .expect_err("an unpinned commit is not accepted");

        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownSnapshot { reference }) if reference == &unpinned
        ));
    }

    #[tokio::test]
    async fn pinned_snapshots_survive_reflog_expiry_and_garbage_collection() {
        let repository = TestRepository::new();
        fs::write(repository.path("tracked.txt"), "snapshot\n")
            .expect("the snapshot content is written");
        let snapshot = repository
            .checkpoint
            .snapshot("durable")
            .await
            .expect("the working tree is snapshotted");

        fs::write(repository.path("tracked.txt"), "after\n").expect("the file is changed");
        git(
            repository.directory.path(),
            &["reflog", "expire", "--expire=now", "--all"],
        );
        git(repository.directory.path(), &["gc", "--prune=now"]);

        let diff = repository
            .checkpoint
            .diff(&snapshot)
            .await
            .expect("the pinned snapshot remains readable");
        assert!(diff.contains("-snapshot"), "{diff}");
        assert!(diff.contains("+after"), "{diff}");
        repository
            .checkpoint
            .restore(&snapshot)
            .await
            .expect("the pinned snapshot remains restorable");
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt")).expect("the file is readable"),
            "snapshot\n"
        );
    }

    #[tokio::test]
    async fn snapshot_labels_are_bounded() {
        let repository = TestRepository::new();
        let error = repository
            .checkpoint
            .snapshot(&"x".repeat(MAX_SNAPSHOT_LABEL_BYTES + 1))
            .await
            .expect_err("an oversized label is refused");

        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::LabelTooLong { length, maximum })
                if *length == MAX_SNAPSHOT_LABEL_BYTES + 1
                    && *maximum == MAX_SNAPSHOT_LABEL_BYTES
        ));
    }

    #[tokio::test]
    async fn checkpoints_wait_for_the_repository_mutation_lease() {
        let repository = TestRepository::new();
        let mutation = FsMutation::begin(repository.directory.path(), &[])
            .expect("the mutation lease is acquired");
        let checkpoint = GitCheckpoint {
            root: repository.directory.path().to_path_buf(),
        };
        let snapshot = tokio::spawn(async move { checkpoint.snapshot("leased").await });

        tokio::task::yield_now().await;
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!snapshot.is_finished(), "the checkpoint bypassed the lease");
        mutation.commit().expect("the mutation lease is released");

        snapshot
            .await
            .expect("the checkpoint task completes")
            .expect("the checkpoint succeeds");
    }
}
