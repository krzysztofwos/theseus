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
mod checkpoint;
mod checkpoint_model;
mod generated;
mod implement_result;
mod project;
mod service;
mod session;
mod stateful;

pub use check_report::CheckReport;
pub use checkpoint_model::SnapshotModelError;
pub use generated::*;
pub use implement_result::ImplementResult;
pub use project::{
    ProjectBindingError, ProjectContext, ProjectContextError, ProjectPathError, ProjectRootError,
    theseus_project,
};
pub use session::{Session, SessionState};
pub use stateful::StatefulSession;
pub use theseus_workspace::{
    ExpectedFile, ExpectedFileSet, FsMutation, MutationError, MutationFile, MutationTarget,
    PendingMutation, WorkspaceMutation, validate_workspace_paths,
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
    project: ProjectContext,
}

impl FsWorkspace {
    /// A workspace bound to the same immutable project as its session.
    pub fn for_project(project: &ProjectContext) -> Self {
        Self {
            project: project.clone(),
        }
    }

    /// A workspace rooted at the repository root.
    pub fn at_repo_root() -> Result<Self, ProjectContextError> {
        Ok(Self::for_project(&theseus_project()?))
    }
}

#[async_trait::async_trait]
impl Workspace for FsWorkspace {
    async fn context(&self) -> anyhow::Result<ProjectContext> {
        self.project.validate_root()?;
        Ok(self.project.clone())
    }

    async fn begin_mutation(&self, expected: &ExpectedFileSet) -> anyhow::Result<PendingMutation> {
        self.project.validate_root()?;
        Ok(FsMutation::begin_async(self.project.root().to_path_buf(), expected.clone()).await?)
    }
}

/// A [`Checkpoint`] backed by paired private Git refs that are validated and
/// never retargeted, then deleted together on release. Snapshots store raw
/// tracked and model-owned state; restores publish exact files, symlinks, modes,
/// and tombstones through the workspace WAL.
pub struct GitCheckpoint {
    project: ProjectContext,
}

const SNAPSHOT_REF_PREFIX: &str = "refs/theseus/snapshots";
const MAX_SNAPSHOT_LABEL_BYTES: usize = 256;

/// The internal snapshot plan assembled from a session's persisted model.
#[derive(Clone, Debug)]
pub struct CheckpointSnapshotRequest {
    pub label: String,
    pub project: theseus_modeling::CheckpointProjectDescriptor,
    pub expected: ExpectedFileSet,
    pub owned_paths: Vec<String>,
    pub model: theseus_modeling::Model,
}

/// The internal plan for inspecting or restoring a snapshot from the current
/// persisted model revision.
#[derive(Clone, Debug)]
pub struct CheckpointStateRequest {
    pub reference: String,
    pub project: theseus_modeling::CheckpointProjectDescriptor,
    pub expected: ExpectedFileSet,
    pub owned_paths: Vec<String>,
    pub model: theseus_modeling::Model,
}

/// A newly pinned snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointSnapshot {
    pub reference: String,
}

/// A completed durable restore and the persisted model stored with it.
#[derive(Clone, Debug)]
pub struct CheckpointRestore {
    pub detail: String,
    pub model: theseus_modeling::Model,
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
            Ok(Self(value.to_ascii_lowercase()))
        } else {
            Err(InvalidGitObjectId)
        }
    }
}

/// A checkpoint reference was not a full hexadecimal Git object ID.
#[derive(Debug, Error)]
#[error("expected a full 40- or 64-character hexadecimal Git object ID")]
pub struct InvalidGitObjectId;

/// A structured checkpoint refusal or I/O failure.
#[derive(Debug, Error)]
pub enum GitCheckpointError {
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
    #[error("snapshot reference {reference} is symbolic")]
    SymbolicSnapshot { reference: String },
    #[error("snapshot manifest is {length} bytes; the maximum is {maximum}")]
    ManifestTooLarge { length: usize, maximum: usize },
    #[error("snapshot manifest has unsupported version {version}")]
    UnsupportedManifest { version: u32 },
    #[error("snapshot manifest is invalid: {message}")]
    InvalidManifest { message: String },
    #[error("snapshot ownership does not match its persisted model")]
    OwnershipMismatch,
    #[error("snapshot expected projection does not match its persisted model")]
    ProjectionMismatch,
    #[error("checkpoint project {actual} does not match configured project {expected}")]
    ProjectMismatch {
        expected: theseus_modeling::ProjectId,
        actual: theseus_modeling::ProjectId,
    },
    #[error("configured project root {configured:?} does not equal Git repository root {actual:?}")]
    RepositoryRootMismatch {
        configured: PathBuf,
        actual: PathBuf,
    },
    #[error("snapshot model cannot be projected")]
    InvalidModel {
        #[source]
        source: theseus_modeling::ProjectLayoutError,
    },
    #[error("snapshot model is invalid")]
    SnapshotModel {
        #[from]
        #[source]
        source: checkpoint_model::SnapshotModelError,
    },
    #[error("Git returned a non-UTF-8 workspace path")]
    NonUtf8Path,
    #[error("Git tree entry {path:?} has unsupported mode {mode} and type {kind}")]
    UnsupportedTreeEntry {
        path: String,
        mode: String,
        kind: String,
    },
    #[error("Git blob for {path:?} is {length} bytes; the maximum is {maximum}")]
    BlobTooLarge {
        path: String,
        length: u64,
        maximum: u64,
    },
    #[error("snapshot contents total {length} bytes; the maximum is {maximum}")]
    SnapshotTooLarge { length: u64, maximum: u64 },
    #[error("snapshot declares {length} paths; the maximum is {maximum}")]
    TooManyPaths { length: usize, maximum: usize },
    #[error("repository retains {length} snapshots; the maximum is {maximum}")]
    TooManySnapshots { length: usize, maximum: usize },
    #[error("Git blob for {path:?} reported {expected} bytes but returned {actual}")]
    BlobLength {
        path: String,
        expected: u64,
        actual: usize,
    },
    #[error("system clock is before the Unix epoch")]
    InvalidClock,
    #[error("writing to `{operation}`")]
    CommandInput {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("reading from `{operation}`")]
    CommandOutput {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("`{operation}` {stream} exceeded {maximum} bytes")]
    CommandOutputTooLarge {
        operation: &'static str,
        stream: &'static str,
        maximum: usize,
    },
    #[error("checkpoint filesystem worker panicked")]
    WorkerPanicked,
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
    #[error("serializing snapshot manifest")]
    SerializeManifest {
        #[source]
        source: serde_json::Error,
    },
    #[error("parsing snapshot manifest")]
    ParseManifest {
        #[source]
        source: serde_json::Error,
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
    /// A checkpoint bound to one immutable project root and layout.
    pub fn for_project(project: ProjectContext) -> Self {
        Self { project }
    }

    /// A checkpoint rooted at the repository root.
    pub fn at_repo_root() -> Result<Self, ProjectContextError> {
        Ok(Self::for_project(theseus_project()?))
    }

    async fn repository_lease(
        &self,
        expected: ExpectedFileSet,
    ) -> Result<PendingMutation, GitCheckpointError> {
        Ok(FsMutation::begin_async(self.project.root().to_path_buf(), expected).await?)
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

/// A [`Toolchain`] that compile-checks the workspace by running `cargo check`
/// at the repository root. The shared toolchain adapter for the inbound binaries.
/// The check runs as a managed child process, so a server inbound keeps serving
/// while it compiles.
pub struct CargoToolchain {
    project: ProjectContext,
}

impl CargoToolchain {
    /// A Cargo adapter bound to the same immutable project as its session.
    pub fn for_project(project: &ProjectContext) -> Self {
        Self {
            project: project.clone(),
        }
    }

    /// A Cargo adapter rooted at Theseus's repository root.
    pub fn at_repo_root() -> Result<Self, ProjectContextError> {
        Ok(Self::for_project(&theseus_project()?))
    }
}

#[async_trait::async_trait]
impl Toolchain for CargoToolchain {
    async fn context(&self) -> anyhow::Result<ProjectContext> {
        self.project.validate_root()?;
        Ok(self.project.clone())
    }

    async fn lint(&self) -> anyhow::Result<CheckReport> {
        self.project.validate_root()?;
        run_cargo_under_lease(
            self.project.root(),
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
        self.project.validate_root()?;
        run_cargo_under_lease(
            self.project.root(),
            &["test", "--workspace", "--quiet", "--locked"],
            "cargo test --workspace --locked",
            "the tests pass",
            "the tests pass, with warnings",
            "tests failed",
        )
        .await
    }

    async fn check(&self) -> anyhow::Result<CheckReport> {
        self.project.validate_root()?;
        run_cargo_under_lease(
            self.project.root(),
            &["check", "--workspace", "--quiet", "--locked"],
            "cargo check --workspace --locked",
            "the workspace compiles",
            "the workspace compiles, with warnings",
            "check failed",
        )
        .await
    }

    async fn check_mutation(&self) -> anyhow::Result<CheckReport> {
        self.project.validate_root()?;
        run_cargo(
            self.project.root(),
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
    root: &Path,
    args: &[&str],
    operation: &'static str,
    success: &'static str,
    success_with_notes: &'static str,
    failure: &'static str,
) -> anyhow::Result<CheckReport> {
    let lease = FsMutation::begin_async(root.to_path_buf(), Vec::new()).await?;
    let report = run_cargo(root, args, operation, success, success_with_notes, failure).await?;
    lease.commit()?;
    Ok(report)
}

async fn run_cargo(
    root: &Path,
    args: &[&str],
    operation: &'static str,
    success: &'static str,
    success_with_notes: &'static str,
    failure: &'static str,
) -> anyhow::Result<CheckReport> {
    let output = tokio::process::Command::new("cargo")
        .args(args)
        .current_dir(root)
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
        ProjectContext,
        GatedWorkspace<FsWorkspace>,
        GatedCheckpoint<GitCheckpoint>,
        theseus_calculator::Calculator,
        CargoToolchain,
    >
{
    pub fn new(allow_writes: bool) -> Result<Self, ProjectContextError> {
        let project = theseus_project()?;
        Ok(Self {
            model: project.initial_model().clone(),
            project: project.clone(),
            workspace: GatedWorkspace {
                workspace: FsWorkspace::for_project(&project),
                allow_writes,
            },
            checkpoint: GatedCheckpoint {
                checkpoint: GitCheckpoint::for_project(project.clone()),
                allow_writes,
            },
            calculator: theseus_calculator::Calculator,
            toolchain: CargoToolchain::for_project(&project),
        })
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
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
    };

    use theseus_model::theseus_model;
    use theseus_modeling::{ModelRecord, ProjectId, RustWorkspaceLayout};

    use super::{
        Checkpoint, CheckpointSnapshotRequest, CheckpointStateRequest, FsMutation, GitCheckpoint,
        GitCheckpointError, GitObjectId, MAX_SNAPSHOT_LABEL_BYTES, ProjectContext,
        SNAPSHOT_REF_PREFIX, checkpoint::PRIMARY_PROMOTION_DIRECTORY,
    };

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    const PROJECT_SNAPSHOT_REF_PREFIX: &str = "refs/theseus/projects/theseus/snapshots";

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
            fs::write(
                directory.path().join(".gitignore"),
                "Cargo.lock\nunrelated.ignored\n",
            )
            .expect("the ignore rules are written");
            git(
                directory.path(),
                &["add", "--", "tracked.txt", ".gitignore"],
            );
            git(directory.path(), &["commit", "--quiet", "-m", "initial"]);
            let checkpoint = GitCheckpoint::for_project(project_context(directory.path()));
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

    fn git_input_stdout(root: &Path, args: &[&str], input: &[u8]) -> String {
        use std::io::Write as _;

        let mut child = Command::new("git")
            .args(args)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("git runs");
        child
            .stdin
            .take()
            .expect("git stdin is piped")
            .write_all(input)
            .expect("git input is written");
        let output = child.wait_with_output().expect("git completes");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn snapshot_manifest(root: &Path, reference: &str) -> serde_json::Value {
        let commit = git_stdout(root, &["cat-file", "commit", reference]);
        let (_, manifest) = commit
            .split_once("\n\n")
            .expect("the snapshot commit has a manifest");
        serde_json::from_str(manifest).expect("the snapshot manifest is JSON")
    }

    fn loose_object_count(root: &Path) -> u64 {
        git_stdout(root, &["count-objects", "-v"])
            .lines()
            .find_map(|line| line.strip_prefix("count: "))
            .expect("Git reports its loose object count")
            .parse()
            .expect("the loose object count is numeric")
    }

    fn project_context(root: &Path) -> ProjectContext {
        ProjectContext::new(
            root,
            theseus_model(),
            theseus_model::project_layout().expect("the Theseus layout is valid"),
        )
        .expect("the project context is valid")
    }

    fn alternate_project_context(root: &Path) -> ProjectContext {
        let layout = RustWorkspaceLayout::new(
            ProjectId::new("alternate").expect("the alternate id is valid"),
            ModelRecord::rust_builder("rust/alternate/src/model.rs", "", "theseus_model")
                .expect("the alternate model record is valid"),
        );
        ProjectContext::new(root, theseus_model(), layout)
            .expect("the alternate project context is valid")
    }

    fn snapshot_request_for(
        project: &ProjectContext,
        label: impl Into<String>,
    ) -> CheckpointSnapshotRequest {
        let model = project.initial_model().clone();
        CheckpointSnapshotRequest {
            label: label.into(),
            project: project.descriptor(),
            expected: project
                .expected_files(&model)
                .expect("checkpoint expectations render"),
            owned_paths: project
                .owned_paths(&model)
                .expect("checkpoint paths render"),
            model,
        }
    }

    fn state_request_for(
        project: &ProjectContext,
        reference: impl Into<String>,
    ) -> CheckpointStateRequest {
        let model = project.initial_model().clone();
        CheckpointStateRequest {
            reference: reference.into(),
            project: project.descriptor(),
            expected: project
                .expected_files(&model)
                .expect("checkpoint expectations render"),
            owned_paths: project
                .owned_paths(&model)
                .expect("checkpoint paths render"),
            model,
        }
    }

    fn snapshot_request(root: &Path, label: impl Into<String>) -> CheckpointSnapshotRequest {
        let project = project_context(root);
        snapshot_request_for(&project, label)
    }

    fn state_request(root: &Path, reference: impl Into<String>) -> CheckpointStateRequest {
        let project = project_context(root);
        state_request_for(&project, reference)
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
            .diff(&state_request(
                repository.directory.path(),
                format!("--output={}", output_path.display()),
            ))
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
    async fn nested_project_roots_are_rejected_before_checkpoint_writes() {
        let repository = TestRepository::new();
        let nested = repository.path("nested-project");
        fs::create_dir(&nested).expect("the nested project root is created");
        let project = project_context(&nested);
        let checkpoint = GitCheckpoint::for_project(project.clone());

        let error = checkpoint
            .snapshot(&snapshot_request_for(&project, "nested"))
            .await
            .expect_err("a nested project cannot checkpoint its outer repository");
        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::RepositoryRootMismatch { configured, actual })
                if configured == project.root() && actual == repository.directory.path()
        ));
        assert!(
            !nested.join(".theseus").exists(),
            "root validation must precede lease creation"
        );
        assert!(
            git_stdout(
                repository.directory.path(),
                &["for-each-ref", "refs/theseus/projects"]
            )
            .is_empty()
        );
    }

    #[tokio::test]
    async fn snapshot_ids_round_trip_through_diff_and_restore() {
        let repository = TestRepository::new();
        fs::write(repository.path("tracked.txt"), "snapshot\n")
            .expect("the snapshot content is written");
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "round trip"))
            .await
            .expect("the working tree is snapshotted")
            .reference;
        GitObjectId::try_from(snapshot.as_str()).expect("snapshot returns a full object ID");
        assert_eq!(
            git_stdout(
                repository.directory.path(),
                &[
                    "show-ref",
                    "--verify",
                    "--hash",
                    &format!("{PROJECT_SNAPSHOT_REF_PREFIX}/{snapshot}"),
                ],
            ),
            snapshot
        );
        let manifest = snapshot_manifest(repository.directory.path(), &snapshot);
        assert_eq!(manifest["version"], 2);
        assert_eq!(manifest["project"]["project_id"], "theseus");
        assert_eq!(manifest["project"]["version"], 1);
        assert!(
            git_stdout(
                repository.directory.path(),
                &["for-each-ref", SNAPSHOT_REF_PREFIX]
            )
            .is_empty(),
            "new snapshots must not use the legacy global namespace"
        );

        fs::write(repository.path("tracked.txt"), "after\n").expect("the later content is written");
        let diff = repository
            .checkpoint
            .diff(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the snapshot can be diffed");
        assert!(diff.contains("-snapshot"), "{diff}");
        assert!(diff.contains("+after"), "{diff}");

        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the snapshot can be restored");
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt"))
                .expect("the restored file is readable"),
            "snapshot\n"
        );
    }

    #[tokio::test]
    async fn project_namespaces_cannot_inspect_release_or_prune_each_other() {
        let repository = TestRepository::new();
        let theseus = project_context(repository.directory.path());
        let alternate = alternate_project_context(repository.directory.path());
        let theseus_checkpoint = GitCheckpoint::for_project(theseus.clone());
        let alternate_checkpoint = GitCheckpoint::for_project(alternate.clone());

        let mismatch = alternate_checkpoint
            .snapshot(&snapshot_request_for(&theseus, "mismatched"))
            .await
            .expect_err("an adapter cannot accept another project's plan");
        assert!(matches!(
            mismatch.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::ProjectMismatch { expected, actual })
                if expected.as_str() == "alternate" && actual.as_str() == "theseus"
        ));

        let theseus_snapshot = theseus_checkpoint
            .snapshot(&snapshot_request_for(&theseus, "theseus"))
            .await
            .expect("the Theseus snapshot succeeds")
            .reference;
        let alternate_snapshot = alternate_checkpoint
            .snapshot(&snapshot_request_for(&alternate, "alternate"))
            .await
            .expect("the alternate snapshot succeeds")
            .reference;

        let error = alternate_checkpoint
            .diff(&state_request_for(&alternate, &theseus_snapshot))
            .await
            .expect_err("another project cannot inspect the Theseus snapshot");
        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownSnapshot { .. })
        ));
        let error = alternate_checkpoint
            .restore(&state_request_for(&alternate, &theseus_snapshot))
            .await
            .expect_err("another project cannot restore the Theseus snapshot");
        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownSnapshot { .. })
        ));
        let error = alternate_checkpoint
            .release(&theseus_snapshot)
            .await
            .expect_err("another project cannot release the Theseus snapshot");
        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownSnapshot { .. })
        ));

        alternate_checkpoint
            .prune(&super::SnapshotRetention { keep: 0 })
            .await
            .expect("alternate pruning succeeds");
        assert_eq!(
            git_stdout(
                repository.directory.path(),
                &[
                    "show-ref",
                    "--verify",
                    "--hash",
                    &format!("{PROJECT_SNAPSHOT_REF_PREFIX}/{theseus_snapshot}"),
                ],
            ),
            theseus_snapshot
        );
        assert!(
            git_stdout(
                repository.directory.path(),
                &["for-each-ref", "refs/theseus/projects/alternate"]
            )
            .is_empty(),
            "alternate pruning must remove only alternate refs"
        );
        theseus_checkpoint
            .diff(&state_request_for(&theseus, &theseus_snapshot))
            .await
            .expect("the Theseus snapshot remains pinned");
        assert_ne!(theseus_snapshot, alternate_snapshot);
    }

    #[tokio::test]
    async fn legacy_version_one_theseus_snapshots_remain_restorable_and_releasable() {
        let repository = TestRepository::new();
        fs::write(repository.path("tracked.txt"), "legacy snapshot\n")
            .expect("the legacy state is written");
        let version_two = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "source"))
            .await
            .expect("the source snapshot succeeds")
            .reference;
        let root = repository.directory.path();
        let tree = git_stdout(root, &["rev-parse", &format!("{version_two}^{{tree}}")]);
        let parent = git_stdout(root, &["rev-parse", "HEAD"]);
        let mut manifest = snapshot_manifest(root, &version_two);
        manifest["version"] = serde_json::json!(1);
        manifest
            .as_object_mut()
            .expect("the manifest is an object")
            .remove("project");
        manifest["label"] = serde_json::json!("legacy");
        manifest["nonce"] = serde_json::json!("legacy-fixture");
        let sequence = manifest["sequence"]
            .as_u64()
            .expect("the sequence is numeric");
        let encoded = serde_json::to_vec(&manifest).expect("the legacy manifest serializes");
        let legacy = git_input_stdout(
            root,
            &["commit-tree", &tree, "-p", &parent, "-F", "-"],
            &encoded,
        );
        let snapshot_ref = format!("{SNAPSHOT_REF_PREFIX}/{legacy}");
        let order_ref = format!("refs/theseus/snapshot-order/{sequence:020}-{legacy}");
        git(root, &["update-ref", &snapshot_ref, &legacy]);
        git(root, &["update-ref", &order_ref, &legacy]);

        fs::write(repository.path("tracked.txt"), "after legacy\n")
            .expect("the post-snapshot state is written");
        let restored = repository
            .checkpoint
            .restore(&state_request(root, &legacy))
            .await
            .expect("the version-one snapshot restores");
        assert_eq!(restored.model, theseus_model());
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt")).unwrap(),
            "legacy snapshot\n"
        );

        repository
            .checkpoint
            .release(&legacy)
            .await
            .expect("the version-one snapshot releases");
        assert!(git_stdout(root, &["for-each-ref", &snapshot_ref]).is_empty());
        assert!(git_stdout(root, &["for-each-ref", &order_ref]).is_empty());
    }

    #[tokio::test]
    async fn diff_object_stores_are_temporary_and_stale_stores_are_recovered() {
        let repository = TestRepository::new();
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "temporary"))
            .await
            .expect("the snapshot succeeds")
            .reference;
        fs::write(repository.path("tracked.txt"), "changed\n")
            .expect("the tracked file is changed");
        let before = loose_object_count(repository.directory.path());
        let marker = repository.path("checkpoint-diff-paused");
        let mut child = Command::new(std::env::current_exe().expect("the test binary has a path"))
            .args([
                "--exact",
                "git_checkpoint_tests::checkpoint_diff_process_helper",
                "--nocapture",
            ])
            .env(
                "THESEUS_CHECKPOINT_DIFF_PAUSE_ROOT",
                repository.directory.path(),
            )
            .env("THESEUS_CHECKPOINT_REFERENCE", &snapshot)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the diff helper starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the diff helper did not reach its object-store pause"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "the diff helper exited before its object-store pause"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            fs::read_dir(repository.path(".theseus"))
                .unwrap()
                .any(|entry| entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("checkpoint-objects-"))
        );
        child.kill().expect("the paused diff is killed");
        child.wait().expect("the killed diff is reaped");
        fs::remove_file(&marker).expect("the diff marker is removed");

        let diff = repository
            .checkpoint
            .diff(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the temporary comparison succeeds");

        assert!(diff.contains("+changed"), "{diff}");
        assert_eq!(loose_object_count(repository.directory.path()), before);
        assert!(
            fs::read_dir(repository.path(".theseus"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("checkpoint-objects-"))
        );
    }

    #[tokio::test]
    async fn killed_snapshot_quarantines_are_recovered_without_main_object_leaks() {
        let repository = TestRepository::new();
        fs::write(repository.path("tracked.txt"), "candidate\n")
            .expect("the candidate state is written");
        let before = loose_object_count(repository.directory.path());
        let marker = repository.path("checkpoint-snapshot-paused");
        let mut child = Command::new(std::env::current_exe().expect("the test binary has a path"))
            .args([
                "--exact",
                "git_checkpoint_tests::checkpoint_snapshot_process_helper",
                "--nocapture",
            ])
            .env(
                "THESEUS_CHECKPOINT_SNAPSHOT_PAUSE_ROOT",
                repository.directory.path(),
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the snapshot helper starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the snapshot helper did not reach quarantine"
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "the snapshot helper exited before its quarantine pause"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(loose_object_count(repository.directory.path()), before);
        child.kill().expect("the paused snapshot is killed");
        child.wait().expect("the killed snapshot is reaped");
        fs::remove_file(&marker).expect("the snapshot marker is removed");
        assert_eq!(loose_object_count(repository.directory.path()), before);

        repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "after recovery",
            ))
            .await
            .expect("the next snapshot removes the stale quarantine");
        assert!(
            fs::read_dir(repository.path(".theseus"))
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("checkpoint-objects-"))
        );
    }

    #[tokio::test]
    async fn checkpoint_snapshot_process_helper() {
        let Ok(root) = std::env::var("THESEUS_CHECKPOINT_SNAPSHOT_PAUSE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        GitCheckpoint::for_project(project_context(&root))
            .snapshot(&snapshot_request(&root, "quarantined"))
            .await
            .expect("the helper snapshot completes only when not killed");
    }

    #[tokio::test]
    async fn checkpoint_diff_process_helper() {
        let Ok(root) = std::env::var("THESEUS_CHECKPOINT_DIFF_PAUSE_ROOT") else {
            return;
        };
        let reference = std::env::var("THESEUS_CHECKPOINT_REFERENCE")
            .expect("the helper receives a snapshot reference");
        let root = PathBuf::from(root);
        let checkpoint = GitCheckpoint::for_project(project_context(&root));
        checkpoint
            .diff(&state_request(&root, reference))
            .await
            .expect("the helper diff completes only when not killed");
    }

    #[tokio::test]
    async fn an_existing_but_unpinned_commit_is_rejected() {
        let repository = TestRepository::new();
        let unpinned = git_stdout(repository.directory.path(), &["rev-parse", "HEAD"]);

        let error = repository
            .checkpoint
            .diff(&state_request(repository.directory.path(), &unpinned))
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
            .snapshot(&snapshot_request(repository.directory.path(), "durable"))
            .await
            .expect("the working tree is snapshotted")
            .reference;

        fs::write(repository.path("tracked.txt"), "after\n").expect("the file is changed");
        git(
            repository.directory.path(),
            &["reflog", "expire", "--expire=now", "--all"],
        );
        git(repository.directory.path(), &["gc", "--prune=now"]);

        let diff = repository
            .checkpoint
            .diff(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the pinned snapshot remains readable");
        assert!(diff.contains("-snapshot"), "{diff}");
        assert!(diff.contains("+after"), "{diff}");
        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the pinned snapshot remains restorable");
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt")).expect("the file is readable"),
            "snapshot\n"
        );
    }

    #[tokio::test]
    async fn owned_untracked_state_is_exact_and_unrelated_files_are_preserved() {
        let repository = TestRepository::new();
        fs::write(repository.path("Cargo.lock"), "snapshot lock\n")
            .expect("the ignored owned file is written");
        fs::write(repository.path("unrelated.ignored"), "before\n")
            .expect("the unrelated ignored file is written");
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "owned state",
            ))
            .await
            .expect("owned untracked state is captured")
            .reference;

        fs::write(repository.path("Cargo.lock"), "after lock\n")
            .expect("the owned file is changed");
        let created = repository.path("rust/calculator/src/service.rs");
        fs::create_dir_all(created.parent().unwrap()).expect("owned parents are created");
        fs::write(&created, "created after snapshot\n").expect("the owned file is created");
        fs::write(repository.path("unrelated.ignored"), "after unrelated\n")
            .expect("the unrelated file is changed");

        let diff = repository
            .checkpoint
            .diff(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the exact checkpoint diff renders");
        assert!(diff.contains("Cargo.lock"), "{diff}");
        assert!(diff.contains("rust/calculator/src/service.rs"), "{diff}");
        assert!(!diff.contains("unrelated.ignored"), "{diff}");

        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("the exact owned state restores");
        assert_eq!(
            fs::read_to_string(repository.path("Cargo.lock")).unwrap(),
            "snapshot lock\n"
        );
        assert!(!created.exists());
        assert_eq!(
            fs::read_to_string(repository.path("unrelated.ignored")).unwrap(),
            "after unrelated\n"
        );
    }

    #[tokio::test]
    async fn blobs_larger_than_git_metadata_restore_exactly() {
        let repository = TestRepository::new();
        let contents = vec![0xa5; 6 * 1024 * 1024];
        fs::write(repository.path("Cargo.lock"), &contents).expect("the large blob is written");
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "large blob"))
            .await
            .expect("the large blob is captured")
            .reference;

        fs::write(repository.path("Cargo.lock"), b"changed").expect("the large blob is replaced");
        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), snapshot))
            .await
            .expect("the large blob restores");

        assert_eq!(fs::read(repository.path("Cargo.lock")).unwrap(), contents);
    }

    #[tokio::test]
    async fn snapshot_capture_bypasses_git_clean_filters() {
        let repository = TestRepository::new();
        git(
            repository.directory.path(),
            &["config", "filter.checkpoint.clean", "tee filter-ran"],
        );
        git(
            repository.directory.path(),
            &["config", "filter.checkpoint.smudge", "cat"],
        );
        fs::write(
            repository.path(".gitattributes"),
            "filtered filter=checkpoint\n",
        )
        .expect("the attributes are written");
        fs::write(repository.path("filtered"), "worktree bytes\n")
            .expect("the filtered file is written");
        git(
            repository.directory.path(),
            &["add", "--", ".gitattributes", "filtered"],
        );
        git(
            repository.directory.path(),
            &["commit", "--quiet", "-m", "filtered fixture"],
        );
        fs::remove_file(repository.path("filter-ran")).expect("the filter marker is cleared");

        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "raw filtered bytes",
            ))
            .await
            .expect("raw capture succeeds")
            .reference;
        assert!(
            !repository.path("filter-ran").exists(),
            "checkpoint capture executed a configured clean filter"
        );
        fs::write(repository.path("filtered"), "changed\n").expect("the worktree file is changed");
        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), snapshot))
            .await
            .expect("the raw filtered bytes restore");
        assert_eq!(
            fs::read_to_string(repository.path("filtered")).unwrap(),
            "worktree bytes\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn snapshot_rejects_hardlinked_workspace_files() {
        let repository = TestRepository::new();
        fs::hard_link(
            repository.path("tracked.txt"),
            repository.path("tracked-alias.txt"),
        )
        .expect("the tracked file is hardlinked");

        let error = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "hardlink"))
            .await
            .expect_err("a hardlinked workspace file is refused");

        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::Mutation(
                super::MutationError::UnsafeTarget { reason, .. }
            )) if *reason == "target has multiple hard links"
        ));
        assert!(
            git_stdout(
                repository.directory.path(),
                &["for-each-ref", PROJECT_SNAPSHOT_REF_PREFIX]
            )
            .is_empty()
        );
    }

    #[test]
    fn inherited_git_repository_overrides_cannot_redirect_a_snapshot() {
        let repository = TestRepository::new();
        let decoy = TestRepository::new();
        let redirected_index = decoy.path("redirected-index");
        let redirected_trace = decoy.path("redirected-trace");
        let output = Command::new(std::env::current_exe().expect("the test binary has a path"))
            .args([
                "--exact",
                "git_checkpoint_tests::checkpoint_environment_process_helper",
                "--nocapture",
            ])
            .env("THESEUS_CHECKPOINT_ENV_ROOT", repository.directory.path())
            .env("GIT_DIR", decoy.path(".git"))
            .env("GIT_WORK_TREE", decoy.directory.path())
            .env("GIT_INDEX_FILE", &redirected_index)
            .env("GIT_OBJECT_DIRECTORY", decoy.path(".git/objects"))
            .env("GIT_TRACE", &redirected_trace)
            .output()
            .expect("the checkpoint helper runs");
        assert!(
            output.status.success(),
            "checkpoint helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !git_stdout(
                repository.directory.path(),
                &["for-each-ref", PROJECT_SNAPSHOT_REF_PREFIX]
            )
            .is_empty(),
            "the intended repository did not receive the snapshot ref"
        );
        assert!(
            git_stdout(
                decoy.directory.path(),
                &["for-each-ref", PROJECT_SNAPSHOT_REF_PREFIX]
            )
            .is_empty(),
            "an inherited Git override redirected the snapshot ref"
        );
        assert!(!redirected_index.exists());
        assert!(!redirected_trace.exists());
    }

    #[tokio::test]
    async fn checkpoint_environment_process_helper() {
        let Ok(root) = std::env::var("THESEUS_CHECKPOINT_ENV_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        GitCheckpoint::for_project(project_context(&root))
            .snapshot(&snapshot_request(&root, "environment"))
            .await
            .expect("the scrubbed checkpoint succeeds");
    }

    #[tokio::test]
    async fn tracked_tombstones_are_restored_without_touching_the_index() {
        let repository = TestRepository::new();
        fs::write(repository.path("removed"), "tracked\n").expect("the fixture is written");
        git(repository.directory.path(), &["add", "--", "removed"]);
        git(
            repository.directory.path(),
            &["commit", "--quiet", "-m", "tracked fixture"],
        );
        git(
            repository.directory.path(),
            &["rm", "--quiet", "--", "removed"],
        );
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "tracked deletion",
            ))
            .await
            .expect("the tracked tombstone is captured")
            .reference;
        let index_before = fs::read(repository.path(".git/index")).expect("the index is readable");
        fs::write(repository.path("removed"), "recreated as untracked\n")
            .expect("the deleted path is recreated");

        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), snapshot))
            .await
            .expect("the tracked tombstone restores");
        assert!(!repository.path("removed").exists());
        assert_eq!(
            fs::read(repository.path(".git/index")).unwrap(),
            index_before
        );
    }

    #[tokio::test]
    async fn historical_blob_limits_do_not_reject_small_current_state() {
        let repository = TestRepository::new();
        let historical = fs::File::create(repository.path("historical-large"))
            .expect("the historical file is created");
        historical
            .set_len(64 * 1024 * 1024 + 1)
            .expect("the historical file is made larger than the snapshot blob limit");
        drop(historical);
        git(
            repository.directory.path(),
            &["add", "--", "historical-large"],
        );
        git(
            repository.directory.path(),
            &["commit", "--quiet", "-m", "large historical blob"],
        );
        fs::write(repository.path("historical-large"), b"current")
            .expect("the current state is small");

        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "small current state",
            ))
            .await
            .expect("historical blob size does not constrain current capture")
            .reference;
        fs::write(repository.path("historical-large"), b"after")
            .expect("the current state changes");
        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), snapshot))
            .await
            .expect("the small current state restores");
        assert_eq!(
            fs::read(repository.path("historical-large")).unwrap(),
            b"current"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn binary_modes_and_symlinks_restore_without_touching_the_index() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let repository = TestRepository::new();
        fs::write(repository.path("binary"), b"snapshot\0\xff")
            .expect("the binary file is written");
        fs::set_permissions(repository.path("binary"), fs::Permissions::from_mode(0o755))
            .expect("the executable mode is set");
        fs::write(repository.path("secret"), b"private").expect("the private file is written");
        fs::set_permissions(repository.path("secret"), fs::Permissions::from_mode(0o600))
            .expect("the private mode is set");
        symlink("binary", repository.path("binary-link")).expect("the link is created");
        git(
            repository.directory.path(),
            &["add", "--", "binary", "binary-link", "secret"],
        );
        git(
            repository.directory.path(),
            &["commit", "--quiet", "-m", "binary fixture"],
        );
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "exact entries",
            ))
            .await
            .expect("the exact entries are captured")
            .reference;
        let index_before = fs::read(repository.path(".git/index")).expect("the index is readable");

        fs::write(repository.path("binary"), b"changed").expect("the binary file is changed");
        fs::set_permissions(repository.path("binary"), fs::Permissions::from_mode(0o644))
            .expect("the executable mode is removed");
        fs::set_permissions(repository.path("secret"), fs::Permissions::from_mode(0o644))
            .expect("the private mode is widened");
        fs::remove_file(repository.path("binary-link")).expect("the link is removed");
        fs::write(repository.path("binary-link"), b"regular")
            .expect("the link path becomes a regular file");

        repository
            .checkpoint
            .restore(&state_request(repository.directory.path(), &snapshot))
            .await
            .expect("binary state restores through the WAL");
        assert_eq!(
            fs::read(repository.path("binary")).unwrap(),
            b"snapshot\0\xff"
        );
        assert_eq!(
            fs::metadata(repository.path("binary"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::read_link(repository.path("binary-link")).unwrap(),
            Path::new("binary")
        );
        assert_eq!(
            fs::metadata(repository.path("secret"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::read(repository.path(".git/index")).unwrap(),
            index_before,
            "checkpoint restore must preserve the user's staging area"
        );
    }

    #[tokio::test]
    async fn clean_snapshots_are_distinct_and_restore_their_persisted_model() {
        let repository = TestRepository::new();
        let first = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "same state"))
            .await
            .expect("the first snapshot succeeds");
        let second = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "same state"))
            .await
            .expect("the second snapshot succeeds");
        assert_ne!(first.reference, second.reference);

        let restored = repository
            .checkpoint
            .restore(&state_request(
                repository.directory.path(),
                &first.reference,
            ))
            .await
            .expect("the first snapshot restores");
        assert_eq!(restored.model, theseus_model());
    }

    #[tokio::test]
    async fn linked_worktrees_share_the_checkpoint_object_lease() {
        let repository = TestRepository::new();
        let worktrees = TempDirectory::new();
        let linked_root = worktrees.path().join("linked");
        git(
            repository.directory.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "--detach",
                linked_root.to_str().expect("the temporary path is UTF-8"),
            ],
        );

        let object_directory = PathBuf::from(git_stdout(
            repository.directory.path(),
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "objects",
            ],
        ));
        let blocker =
            FsMutation::begin(&object_directory, &[]).expect("the shared object lease is acquired");
        let request = snapshot_request(&linked_root, "linked worktree");
        let checkpoint = GitCheckpoint::for_project(project_context(&linked_root));
        let task = tokio::spawn(async move { checkpoint.snapshot(&request).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !task.is_finished(),
            "a linked worktree must wait for the shared object lease"
        );

        blocker
            .commit()
            .expect("the shared object lease is released");
        task.await
            .expect("the linked snapshot task joins")
            .expect("the linked snapshot succeeds");

        let main_checkpoint =
            GitCheckpoint::for_project(project_context(repository.directory.path()));
        let linked_checkpoint = GitCheckpoint::for_project(project_context(&linked_root));
        let main_request = snapshot_request(repository.directory.path(), "concurrent main");
        let linked_request = snapshot_request(&linked_root, "concurrent linked");
        let (main_snapshot, linked_snapshot) = tokio::join!(
            main_checkpoint.snapshot(&main_request),
            linked_checkpoint.snapshot(&linked_request),
        );
        let main_snapshot = main_snapshot.expect("the main worktree snapshot succeeds");
        let linked_snapshot = linked_snapshot.expect("the linked worktree snapshot succeeds");
        assert_ne!(main_snapshot.reference, linked_snapshot.reference);
        repository
            .checkpoint
            .prune(&super::SnapshotRetention { keep: 2 })
            .await
            .expect("concurrent worktree snapshots retain valid unique ordering");
    }

    #[tokio::test]
    async fn stale_primary_promotion_files_are_recovered() {
        let repository = TestRepository::new();
        let promotion = repository
            .path(".git/objects")
            .join(PRIMARY_PROMOTION_DIRECTORY);
        fs::create_dir(&promotion).expect("the stale promotion directory is created");
        fs::write(promotion.join("interrupted-copy"), b"partial")
            .expect("the stale promotion file is written");

        repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "recover promotion",
            ))
            .await
            .expect("the next snapshot recovers the promotion directory");
        assert!(!promotion.exists());
    }

    #[tokio::test]
    async fn release_and_retention_remove_only_validated_snapshot_refs() {
        let repository = TestRepository::new();
        let first = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "first"))
            .await
            .unwrap();
        let second = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "second"))
            .await
            .unwrap();
        let third = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "third"))
            .await
            .unwrap();
        repository
            .checkpoint
            .prune(&super::SnapshotRetention { keep: 2 })
            .await
            .expect("explicit retention pruning succeeds");

        let expired = repository
            .checkpoint
            .diff(&state_request(
                repository.directory.path(),
                &first.reference,
            ))
            .await
            .expect_err("retention unpins the oldest snapshot");
        assert!(matches!(
            expired.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownSnapshot { .. })
        ));
        repository
            .checkpoint
            .release(&second.reference)
            .await
            .expect("an explicit release succeeds");
        let released = repository
            .checkpoint
            .diff(&state_request(
                repository.directory.path(),
                &second.reference,
            ))
            .await
            .expect_err("the released snapshot is no longer accepted");
        assert!(matches!(
            released.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::UnknownSnapshot { .. })
        ));
        repository
            .checkpoint
            .diff(&state_request(
                repository.directory.path(),
                &third.reference,
            ))
            .await
            .expect("the newest snapshot remains pinned");
    }

    #[tokio::test]
    async fn symbolic_snapshot_refs_cannot_delete_their_referent() {
        let repository = TestRepository::new();
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(repository.directory.path(), "symbolic"))
            .await
            .expect("the snapshot succeeds")
            .reference;
        let branch = git_stdout(repository.directory.path(), &["symbolic-ref", "HEAD"]);
        let head = git_stdout(repository.directory.path(), &["rev-parse", "HEAD"]);
        let snapshot_ref = format!("{PROJECT_SNAPSHOT_REF_PREFIX}/{snapshot}");
        git(
            repository.directory.path(),
            &["symbolic-ref", &snapshot_ref, &branch],
        );

        let error = repository
            .checkpoint
            .release(&snapshot)
            .await
            .expect_err("a symbolic snapshot ref is refused");
        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::SymbolicSnapshot { .. })
        ));
        assert_eq!(
            git_stdout(repository.directory.path(), &["rev-parse", &branch]),
            head
        );
        assert_eq!(
            git_stdout(
                repository.directory.path(),
                &["symbolic-ref", &snapshot_ref]
            ),
            branch
        );
    }

    #[tokio::test]
    async fn stale_snapshot_plans_are_rejected_before_a_ref_is_created() {
        let repository = TestRepository::new();
        let request = snapshot_request(repository.directory.path(), "stale");
        let unexpected = repository.path("rust/model/src/self_model.rs");
        fs::create_dir_all(unexpected.parent().unwrap()).expect("unexpected parents are created");
        fs::write(&unexpected, "not expected by the persisted projection\n")
            .expect("the unexpected generated file is written");

        let error = repository
            .checkpoint
            .snapshot(&request)
            .await
            .expect_err("a stale session cannot bind its model to newer disk state");
        assert!(matches!(
            error.downcast_ref::<GitCheckpointError>(),
            Some(GitCheckpointError::Mutation(
                super::MutationError::StaleWorkspace { .. }
            ))
        ));
        assert!(
            git_stdout(
                repository.directory.path(),
                &["for-each-ref", PROJECT_SNAPSHOT_REF_PREFIX]
            )
            .is_empty()
        );
    }

    #[tokio::test]
    async fn a_killed_applied_restore_is_recovered_by_the_next_lease() {
        let repository = TestRepository::new();
        fs::write(repository.path("tracked.txt"), "snapshot\n")
            .expect("the snapshot state is written");
        let snapshot = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "crash recovery",
            ))
            .await
            .expect("the snapshot succeeds")
            .reference;
        fs::write(repository.path("tracked.txt"), "before restore\n")
            .expect("the later state is written");

        let marker = repository.path("checkpoint-restore-paused");
        let mut child = Command::new(std::env::current_exe().expect("the test binary has a path"))
            .args([
                "--exact",
                "git_checkpoint_tests::checkpoint_restore_process_helper",
                "--nocapture",
            ])
            .env("THESEUS_CHECKPOINT_PAUSE_ROOT", repository.directory.path())
            .env("THESEUS_CHECKPOINT_REFERENCE", &snapshot)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the restore helper starts");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the restore helper did not reach its prepared pause"
            );
            assert!(
                child
                    .try_wait()
                    .expect("the helper status is readable")
                    .is_none(),
                "the restore helper exited before its prepared pause"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt")).unwrap(),
            "snapshot\n"
        );
        child.kill().expect("the paused restore is killed");
        child.wait().expect("the killed restore is reaped");
        fs::remove_file(&marker).expect("the test marker is removed");

        let recovered = FsMutation::begin(repository.directory.path(), &[])
            .expect("the next lease recovers the prepared restore");
        assert_eq!(
            fs::read_to_string(repository.path("tracked.txt")).unwrap(),
            "before restore\n"
        );
        recovered.commit().expect("the recovered lease is released");
    }

    #[tokio::test]
    async fn checkpoint_restore_process_helper() {
        let Ok(root) = std::env::var("THESEUS_CHECKPOINT_PAUSE_ROOT") else {
            return;
        };
        let reference = std::env::var("THESEUS_CHECKPOINT_REFERENCE")
            .expect("the helper receives a snapshot reference");
        let root = PathBuf::from(root);
        let checkpoint = GitCheckpoint::for_project(project_context(&root));
        let request = state_request(&root, reference);
        checkpoint
            .restore(&request)
            .await
            .expect("the helper restore completes only when not killed");
    }

    #[tokio::test]
    async fn snapshot_labels_are_bounded() {
        let repository = TestRepository::new();
        let error = repository
            .checkpoint
            .snapshot(&snapshot_request(
                repository.directory.path(),
                "x".repeat(MAX_SNAPSHOT_LABEL_BYTES + 1),
            ))
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
        let checkpoint = GitCheckpoint::for_project(project_context(repository.directory.path()));
        let request = snapshot_request(repository.directory.path(), "leased");
        let snapshot = tokio::spawn(async move { checkpoint.snapshot(&request).await });

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
