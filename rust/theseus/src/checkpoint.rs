//! Durable Git checkpoints over exact, model-owned working-tree state.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs::{self, OpenOptions},
    future::Future,
    io::{Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    task::{Context, Poll},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use theseus_modeling::{CheckpointProjectDescriptor, ProjectId};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::{
    Checkpoint, CheckpointRestore, CheckpointSnapshot, CheckpointSnapshotRequest,
    CheckpointStateRequest, GitCheckpoint, GitCheckpointError, GitObjectId, MutationError,
    MutationFile, MutationTarget, SNAPSHOT_REF_PREFIX, SnapshotRetention,
    checkpoint_model::SnapshotModelV1, validate_workspace_paths,
};

const LEGACY_SNAPSHOT_MANIFEST_VERSION: u32 = 1;
const SNAPSHOT_MANIFEST_VERSION: u32 = 2;
const MAX_SNAPSHOT_MANIFEST_BYTES: usize = 4 * 1024 * 1024;
const MAX_SNAPSHOT_BLOB_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SNAPSHOT_TOTAL_BYTES: u64 = 256 * 1024 * 1024;
const MAX_PROMOTED_OBJECT_BYTES: u64 = MAX_SNAPSHOT_TOTAL_BYTES + 16 * 1024 * 1024;
const MAX_SNAPSHOT_PATHS: usize = 4_096;
const MAX_SNAPSHOT_REFS: usize = 1_024;
const MAX_GIT_METADATA_BYTES: usize = MAX_SNAPSHOT_MANIFEST_BYTES + 1024 * 1024;
const MAX_DIFF_BYTES: usize = MAX_SNAPSHOT_TOTAL_BYTES as usize;
const MAX_GIT_ERROR_BYTES: usize = 1024 * 1024;
const SNAPSHOT_ORDER_REF_PREFIX: &str = "refs/theseus/snapshot-order";
const PROJECT_REF_PREFIX: &str = "refs/theseus/projects";
const LEGACY_THESEUS_PROJECT_ID: &str = "theseus";
const LEGACY_THESEUS_MODEL_RECORD: &str = "rust/model/src/self_model.rs";
pub(super) const PRIMARY_PROMOTION_DIRECTORY: &str = ".theseus-checkpoint-promote";

static NEXT_TEMP_INDEX: AtomicU64 = AtomicU64::new(0);
static NEXT_SNAPSHOT_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotManifest {
    version: u32,
    #[serde(default)]
    project: Option<CheckpointProjectDescriptor>,
    label: String,
    created_millis: u64,
    sequence: u64,
    nonce: String,
    owned_paths: Vec<String>,
    tracked_paths: Vec<String>,
    file_modes: BTreeMap<String, u32>,
    model: SnapshotModelV1,
}

#[derive(Serialize)]
struct SnapshotManifestRef<'a> {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<&'a CheckpointProjectDescriptor>,
    label: &'a str,
    created_millis: u64,
    sequence: u64,
    nonce: &'a str,
    owned_paths: &'a [String],
    tracked_paths: &'a [String],
    file_modes: &'a BTreeMap<String, u32>,
    model: &'a SnapshotModelV1,
}

struct LoadedSnapshot {
    object_id: GitObjectId,
    tree_id: GitObjectId,
    records: Vec<TreeRecord>,
    manifest: SnapshotManifest,
    snapshot_ref: String,
    order_ref: String,
}

struct PinnedCommit {
    object_id: GitObjectId,
    namespace: SnapshotNamespace,
    snapshot_ref: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SnapshotNamespace {
    Project,
    LegacyTheseus,
}

struct CapturedTree {
    object_id: GitObjectId,
    tracked_paths: Vec<String>,
    file_modes: BTreeMap<String, u32>,
}

#[derive(Clone)]
struct SnapshotRefRecord {
    object_id: GitObjectId,
    sequence: u64,
    snapshot_ref: String,
    order_ref: String,
}

struct ListedRef {
    name: String,
    object_id: GitObjectId,
}

struct TreeRecord {
    path: String,
    object_id: GitObjectId,
    size: u64,
    kind: TreeKind,
}

enum TreeKind {
    Regular { mode: u32 },
    Symlink,
}

struct TemporaryIndex {
    path: PathBuf,
}

struct GitObjectEnvironment {
    directory: PathBuf,
    primary: PathBuf,
    alternates: OsString,
}

struct TemporaryObjectStore {
    environment: GitObjectEnvironment,
    control: PathBuf,
    active: bool,
}

impl TemporaryObjectStore {
    fn new(root: &Path, alternate: PathBuf) -> Result<Self, GitCheckpointError> {
        let alternate = alternate
            .canonicalize()
            .map_err(|source| MutationError::Io {
                operation: "canonicalizing Git object directory",
                path: alternate,
                source,
            })?;
        cleanup_primary_promotion_directory(&alternate)?;
        let alternates =
            std::env::join_paths(std::iter::once(alternate.as_os_str())).map_err(|_| {
                GitCheckpointError::InvalidManifest {
                    message: format!(
                        "Git object directory {:?} cannot be represented as an alternate",
                        alternate
                    ),
                }
            })?;
        let sequence = NEXT_TEMP_INDEX.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let control = root.join(".theseus");
        let directory = control.join(format!(
            "checkpoint-objects-{}-{timestamp}-{sequence}",
            std::process::id(),
        ));
        fs::create_dir(&directory).map_err(|source| MutationError::Io {
            operation: "creating checkpoint object directory",
            path: directory.clone(),
            source,
        })?;
        let store = Self {
            environment: GitObjectEnvironment {
                directory,
                primary: alternate,
                alternates,
            },
            control,
            active: true,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                &store.environment.directory,
                fs::Permissions::from_mode(0o700),
            )
            .map_err(|source| MutationError::Io {
                operation: "securing checkpoint object directory",
                path: store.environment.directory.clone(),
                source,
            })?;
        }
        fs::File::open(&store.control)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| MutationError::Io {
                operation: "syncing checkpoint control directory",
                path: store.control.clone(),
                source,
            })?;
        Ok(store)
    }

    fn cleanup(mut self) -> Result<(), GitCheckpointError> {
        fs::remove_dir_all(&self.environment.directory).map_err(|source| MutationError::Io {
            operation: "removing checkpoint object directory",
            path: self.environment.directory.clone(),
            source,
        })?;
        fs::File::open(&self.control)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| MutationError::Io {
                operation: "syncing checkpoint control directory",
                path: self.control.clone(),
                source,
            })?;
        self.active = false;
        Ok(())
    }
}

impl Drop for TemporaryObjectStore {
    fn drop(&mut self) {
        if !self.active {
            return;
        }
        let _ = fs::remove_dir_all(&self.environment.directory);
        let _ = fs::File::open(&self.control).and_then(|directory| directory.sync_all());
    }
}

impl TemporaryIndex {
    fn new(root: &Path) -> Self {
        let sequence = NEXT_TEMP_INDEX.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = root.join(".theseus").join(format!(
            "checkpoint-index-{}-{timestamp}-{sequence}",
            std::process::id(),
        ));
        Self { path }
    }
}

impl Drop for TemporaryIndex {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let mut lock = self.path.as_os_str().to_os_string();
        lock.push(".lock");
        let _ = fs::remove_file(PathBuf::from(lock));
    }
}

/// Dropping a filesystem worker joins it, so request cancellation cannot leave
/// repository reads running after the lease is released.
struct JoiningWorker<T> {
    receiver: tokio::sync::oneshot::Receiver<T>,
    thread: Option<thread::JoinHandle<()>>,
}

struct ManagedGitChild {
    child: tokio::process::Child,
    reaped: bool,
}

impl ManagedGitChild {
    fn new(child: tokio::process::Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }

    async fn kill_and_reap(&mut self) {
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
        self.reaped = true;
    }

    fn mark_reaped(&mut self) {
        self.reaped = true;
    }
}

impl Drop for ManagedGitChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.child.start_kill();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }
}

impl<T: Send + 'static> JoiningWorker<T> {
    fn spawn(work: impl FnOnce() -> T + Send + 'static) -> Result<Self, GitCheckpointError> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let thread = thread::Builder::new()
            .name("theseus-checkpoint-fs".to_string())
            .spawn(move || {
                let _ = sender.send(work());
            })
            .map_err(|source| GitCheckpointError::Launch {
                operation: "starting checkpoint filesystem worker",
                source,
            })?;
        Ok(Self {
            receiver,
            thread: Some(thread),
        })
    }
}

impl<T> JoiningWorker<T> {
    fn join(&mut self) -> Result<(), GitCheckpointError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| GitCheckpointError::WorkerPanicked)
    }
}

impl<T> Unpin for JoiningWorker<T> {}

impl<T> Future for JoiningWorker<T> {
    type Output = Result<T, GitCheckpointError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(output)) => match self.join() {
                Ok(()) => Poll::Ready(Ok(output)),
                Err(error) => Poll::Ready(Err(error)),
            },
            Poll::Ready(Err(_)) => {
                let error = self
                    .join()
                    .err()
                    .unwrap_or(GitCheckpointError::WorkerPanicked);
                Poll::Ready(Err(error))
            }
        }
    }
}

impl<T> Drop for JoiningWorker<T> {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

impl GitCheckpoint {
    fn legacy_snapshot_order_ref(sequence: u64, object_id: &GitObjectId) -> String {
        format!(
            "{SNAPSHOT_ORDER_REF_PREFIX}/{sequence:020}-{}",
            object_id.as_str()
        )
    }

    fn project_snapshot_ref_prefix(&self) -> String {
        project_snapshot_ref_prefix(self.project.descriptor().project_id())
    }

    fn project_snapshot_order_ref_prefix(&self) -> String {
        project_snapshot_order_ref_prefix(self.project.descriptor().project_id())
    }

    fn project_snapshot_ref(&self, object_id: &GitObjectId) -> String {
        format!(
            "{}/{}",
            self.project_snapshot_ref_prefix(),
            object_id.as_str()
        )
    }

    fn project_snapshot_order_ref(&self, sequence: u64, object_id: &GitObjectId) -> String {
        format!(
            "{}/{sequence:020}-{}",
            self.project_snapshot_order_ref_prefix(),
            object_id.as_str()
        )
    }

    fn snapshot_order_ref(
        &self,
        namespace: SnapshotNamespace,
        sequence: u64,
        object_id: &GitObjectId,
    ) -> String {
        match namespace {
            SnapshotNamespace::Project => self.project_snapshot_order_ref(sequence, object_id),
            SnapshotNamespace::LegacyTheseus => {
                Self::legacy_snapshot_order_ref(sequence, object_id)
            }
        }
    }

    fn supports_legacy_theseus_snapshots(&self) -> bool {
        let descriptor = self.project.descriptor();
        descriptor.project_id().as_str() == LEGACY_THESEUS_PROJECT_ID
            && descriptor.model_record().path() == LEGACY_THESEUS_MODEL_RECORD
            && theseus_model::project_layout()
                .map(|layout| descriptor == layout.checkpoint_descriptor())
                .unwrap_or(false)
    }

    fn validate_request_project(
        &self,
        project: &CheckpointProjectDescriptor,
    ) -> Result<(), GitCheckpointError> {
        let expected = self.project.descriptor();
        if project != &expected {
            return Err(GitCheckpointError::ProjectMismatch {
                expected: expected.project_id().clone(),
                actual: project.project_id().clone(),
            });
        }
        Ok(())
    }

    fn validate_snapshot_request(
        &self,
        request: &CheckpointSnapshotRequest,
    ) -> Result<(), GitCheckpointError> {
        Self::validate_label(&request.label)?;
        self.validate_request_project(&request.project)?;
        validate_workspace_paths(&request.owned_paths)?;
        let expected = self
            .project
            .owned_paths(&request.model)
            .map_err(|source| GitCheckpointError::InvalidModel { source })?;
        if expected != request.owned_paths {
            return Err(GitCheckpointError::OwnershipMismatch);
        }
        let expected_projection = self
            .project
            .expected_files(&request.model)
            .map_err(|source| GitCheckpointError::InvalidModel { source })?;
        if expected_projection != request.expected {
            return Err(GitCheckpointError::ProjectionMismatch);
        }
        let _ = encode_manifest(&request.model)?;
        Ok(())
    }

    fn validate_state_request(
        &self,
        request: &CheckpointStateRequest,
    ) -> Result<(), GitCheckpointError> {
        self.validate_request_project(&request.project)?;
        validate_workspace_paths(&request.owned_paths)?;
        let expected = self
            .project
            .owned_paths(&request.model)
            .map_err(|source| GitCheckpointError::InvalidModel { source })?;
        if expected != request.owned_paths {
            return Err(GitCheckpointError::OwnershipMismatch);
        }
        let expected_projection = self
            .project
            .expected_files(&request.model)
            .map_err(|source| GitCheckpointError::InvalidModel { source })?;
        if expected_projection != request.expected {
            return Err(GitCheckpointError::ProjectionMismatch);
        }
        Ok(())
    }

    async fn validate_repository_root(&self) -> Result<(), GitCheckpointError> {
        let output = self
            .git_output(
                "git rev-parse repository root",
                &["rev-parse", "--show-toplevel"],
                None,
            )
            .await?;
        let actual =
            String::from_utf8(output.stdout).map_err(|_| GitCheckpointError::NonUtf8Path)?;
        let actual = PathBuf::from(actual.trim_end_matches(['\r', '\n']));
        if actual.as_os_str().is_empty() {
            return Err(GitCheckpointError::InvalidManifest {
                message: "Git returned an empty repository root".to_string(),
            });
        }
        let configured = self.project.root().to_path_buf();
        let canonical_configured = configured.clone();
        let canonical_actual = actual.clone();
        let (canonical_configured, canonical_actual) = JoiningWorker::spawn(move || {
            let configured =
                canonical_configured
                    .canonicalize()
                    .map_err(|source| MutationError::Io {
                        operation: "canonicalizing configured project root",
                        path: canonical_configured,
                        source,
                    })?;
            let actual = canonical_actual
                .canonicalize()
                .map_err(|source| MutationError::Io {
                    operation: "canonicalizing Git repository root",
                    path: canonical_actual,
                    source,
                })?;
            Ok::<_, MutationError>((configured, actual))
        })?
        .await??;
        if canonical_configured != canonical_actual {
            return Err(GitCheckpointError::RepositoryRootMismatch { configured, actual });
        }
        Ok(())
    }

    async fn cleanup_temporary_indices(&self) -> Result<(), GitCheckpointError> {
        let root = self.project.root().to_path_buf();
        JoiningWorker::spawn(move || cleanup_temporary_indices(&root))?.await?
    }

    async fn current_targets(
        &self,
        paths: Vec<String>,
    ) -> Result<BTreeMap<String, MutationTarget>, GitCheckpointError> {
        ensure_path_limit(paths.len())?;
        let root = self.project.root().to_path_buf();
        JoiningWorker::spawn(move || read_current_targets(&root, paths))?.await?
    }

    async fn temporary_object_store(&self) -> Result<TemporaryObjectStore, GitCheckpointError> {
        let path = self.object_directory().await?;
        let root = self.project.root().to_path_buf();
        JoiningWorker::spawn(move || TemporaryObjectStore::new(&root, path))?.await?
    }

    async fn object_directory(&self) -> Result<PathBuf, GitCheckpointError> {
        let output = self
            .git_output(
                "git rev-parse object directory",
                &[
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-path",
                    "objects",
                ],
                None,
            )
            .await?;
        let path = String::from_utf8(output.stdout).map_err(|_| GitCheckpointError::NonUtf8Path)?;
        let path = PathBuf::from(path.trim_end_matches(['\r', '\n']));
        if path.as_os_str().is_empty() {
            return Err(GitCheckpointError::InvalidManifest {
                message: "Git returned an empty object directory".to_string(),
            });
        }
        Ok(path)
    }

    async fn object_database_lease(&self) -> Result<crate::PendingMutation, GitCheckpointError> {
        let object_directory = self.object_directory().await?;
        Ok(crate::FsMutation::begin_async(object_directory, Vec::new()).await?)
    }

    async fn head(&self) -> Result<GitObjectId, GitCheckpointError> {
        let output = self
            .git_output("git rev-parse HEAD", &["rev-parse", "HEAD"], None)
            .await?;
        Self::snapshot_id("git rev-parse HEAD", &output)
    }

    async fn pinned_commit(&self, reference: &str) -> Result<PinnedCommit, GitCheckpointError> {
        let object_id = GitObjectId::try_from(reference)
            .map_err(|source| GitCheckpointError::invalid_reference(reference, source))?;
        let project_ref = self.project_snapshot_ref(&object_id);
        let (namespace, snapshot_ref) =
            if self.has_exact_pinned_ref(&project_ref, &object_id).await? {
                (SnapshotNamespace::Project, project_ref)
            } else if self.supports_legacy_theseus_snapshots() {
                let legacy_ref = Self::snapshot_ref(&object_id);
                if self.has_exact_pinned_ref(&legacy_ref, &object_id).await? {
                    (SnapshotNamespace::LegacyTheseus, legacy_ref)
                } else {
                    return Err(GitCheckpointError::UnknownSnapshot {
                        reference: object_id.into_string(),
                    });
                }
            } else {
                return Err(GitCheckpointError::UnknownSnapshot {
                    reference: object_id.into_string(),
                });
            };
        if self
            .git_text("git cat-file type", &["cat-file", "-t", object_id.as_str()])
            .await?
            != "commit"
        {
            return Err(GitCheckpointError::UnknownSnapshot {
                reference: object_id.into_string(),
            });
        }
        Ok(PinnedCommit {
            object_id,
            namespace,
            snapshot_ref,
        })
    }

    async fn capture_tree_in(
        &self,
        owned_paths: &[String],
        objects: Option<&GitObjectEnvironment>,
    ) -> Result<CapturedTree, GitCheckpointError> {
        validate_workspace_paths(owned_paths)?;
        ensure_path_limit(owned_paths.len())?;
        let head = self.head().await?;
        let head_paths = self.tree_paths(&head).await?;
        let tracked = self.tracked_paths().await?;

        let mut tracked_paths: BTreeSet<String> = head_paths.into_iter().collect();
        tracked_paths.extend(tracked);
        ensure_path_limit(tracked_paths.len())?;
        let mut candidates = tracked_paths.clone();
        candidates.extend(owned_paths.iter().cloned());
        ensure_path_limit(candidates.len())?;
        let candidates: Vec<String> = candidates.into_iter().collect();
        validate_workspace_paths(&candidates)?;
        let mut current_targets = self.current_targets(candidates.clone()).await?;

        let index = TemporaryIndex::new(self.project.root());
        self.git_output_limited_in(
            "git read-tree",
            &["read-tree", "--empty"],
            Some(&index.path),
            objects,
            MAX_GIT_METADATA_BYTES,
        )
        .await?;
        let mut file_modes = BTreeMap::new();
        let mut index_entries = Vec::new();
        for path in candidates {
            let Some(target) = current_targets.remove(&path) else {
                continue;
            };
            let (git_mode, contents) = match target {
                MutationTarget::Regular { contents, mode } => {
                    let mode = mode.unwrap_or(0o644);
                    file_modes.insert(path.clone(), mode);
                    let git_mode = if mode & 0o111 == 0 {
                        "100644"
                    } else {
                        "100755"
                    };
                    (git_mode, contents)
                }
                MutationTarget::Symlink { target } => ("120000", target),
                MutationTarget::Absent => unreachable!("current targets are present"),
            };
            let output = self
                .git_input_in(
                    "git hash-object",
                    &["hash-object", "-w", "--no-filters", "--stdin"],
                    None,
                    objects,
                    &contents,
                )
                .await?;
            let object_id = Self::snapshot_id("git hash-object", &output)?;
            index_entries
                .extend_from_slice(format!("{git_mode} {}\t{path}", object_id.as_str()).as_bytes());
            index_entries.push(0);
        }
        self.git_input_in(
            "git update-index",
            &["update-index", "-z", "--index-info"],
            Some(&index.path),
            objects,
            &index_entries,
        )
        .await?;
        let output = self
            .git_output_limited_in(
                "git write-tree",
                &["write-tree"],
                Some(&index.path),
                objects,
                MAX_GIT_METADATA_BYTES,
            )
            .await?;
        Ok(CapturedTree {
            object_id: Self::snapshot_id("git write-tree", &output)?,
            tracked_paths: tracked_paths.into_iter().collect(),
            file_modes,
        })
    }

    async fn create_snapshot_commit(
        &self,
        tree: &GitObjectId,
        manifest: &(impl Serialize + ?Sized),
        objects: Option<&GitObjectEnvironment>,
    ) -> Result<GitObjectId, GitCheckpointError> {
        let encoded = encode_manifest(manifest)?;
        let head = self.head().await?;
        let output = self
            .git_input_in(
                "git commit-tree",
                &["commit-tree", tree.as_str(), "-p", head.as_str(), "-F", "-"],
                None,
                objects,
                &encoded,
            )
            .await?;
        Self::snapshot_id("git commit-tree", &output)
    }

    async fn load_snapshot(&self, reference: &str) -> Result<LoadedSnapshot, GitCheckpointError> {
        let PinnedCommit {
            object_id,
            namespace,
            snapshot_ref,
        } = self.pinned_commit(reference).await?;
        let size = self
            .git_text("git cat-file size", &["cat-file", "-s", object_id.as_str()])
            .await?
            .parse::<usize>()
            .map_err(|_| GitCheckpointError::InvalidManifest {
                message: "Git returned a non-numeric commit size".to_string(),
            })?;
        if size > MAX_SNAPSHOT_MANIFEST_BYTES + 16 * 1024 {
            return Err(GitCheckpointError::ManifestTooLarge {
                length: size,
                maximum: MAX_SNAPSHOT_MANIFEST_BYTES,
            });
        }
        let output = self
            .git_output(
                "git cat-file commit",
                &["cat-file", "commit", object_id.as_str()],
                None,
            )
            .await?;
        let separator = output
            .stdout
            .windows(2)
            .position(|window| window == b"\n\n")
            .ok_or_else(|| GitCheckpointError::InvalidManifest {
                message: "commit has no manifest body".to_string(),
            })?;
        let headers = std::str::from_utf8(&output.stdout[..separator]).map_err(|_| {
            GitCheckpointError::InvalidManifest {
                message: "commit headers are not UTF-8".to_string(),
            }
        })?;
        let tree = headers
            .lines()
            .find_map(|line| line.strip_prefix("tree "))
            .ok_or_else(|| GitCheckpointError::InvalidManifest {
                message: "commit has no tree".to_string(),
            })?;
        let tree_id = GitObjectId::try_from(tree).map_err(|source| {
            GitCheckpointError::invalid_output("git cat-file commit", tree, source)
        })?;
        let body = &output.stdout[separator + 2..];
        if body.len() > MAX_SNAPSHOT_MANIFEST_BYTES {
            return Err(GitCheckpointError::ManifestTooLarge {
                length: body.len(),
                maximum: MAX_SNAPSHOT_MANIFEST_BYTES,
            });
        }
        let manifest: SnapshotManifest = serde_json::from_slice(body)
            .map_err(|source| GitCheckpointError::ParseManifest { source })?;
        self.validate_manifest(&manifest, namespace)?;
        let order_ref = self.snapshot_order_ref(namespace, manifest.sequence, &object_id);
        self.validate_order_ref(&object_id, &order_ref).await?;
        let records = self.tree_records(&tree_id).await?;
        self.validate_manifest_tree(&manifest, &records)?;
        Ok(LoadedSnapshot {
            object_id,
            tree_id,
            records,
            manifest,
            snapshot_ref,
            order_ref,
        })
    }

    fn validate_manifest(
        &self,
        manifest: &SnapshotManifest,
        namespace: SnapshotNamespace,
    ) -> Result<(), GitCheckpointError> {
        Self::validate_label(&manifest.label)?;
        validate_workspace_paths(&manifest.owned_paths)?;
        validate_workspace_paths(&manifest.tracked_paths)?;
        let declared_paths: BTreeSet<&str> = manifest
            .owned_paths
            .iter()
            .chain(&manifest.tracked_paths)
            .map(String::as_str)
            .collect();
        ensure_path_limit(declared_paths.len())?;
        let expected = match manifest.version {
            LEGACY_SNAPSHOT_MANIFEST_VERSION => {
                if namespace != SnapshotNamespace::LegacyTheseus
                    || manifest.project.is_some()
                    || !self.supports_legacy_theseus_snapshots()
                {
                    return Err(GitCheckpointError::InvalidManifest {
                        message:
                            "version-one snapshot is not pinned in the legacy Theseus namespace"
                                .to_string(),
                    });
                }
                manifest.model.owned_paths()?
            }
            SNAPSHOT_MANIFEST_VERSION => {
                if namespace != SnapshotNamespace::Project {
                    return Err(GitCheckpointError::InvalidManifest {
                        message: "version-two snapshot is not pinned in its project namespace"
                            .to_string(),
                    });
                }
                let project = manifest.project.as_ref().ok_or_else(|| {
                    GitCheckpointError::InvalidManifest {
                        message: "version-two snapshot has no project descriptor".to_string(),
                    }
                })?;
                self.validate_request_project(project)?;
                project
                    .owned_paths(&manifest.model.clone().into())
                    .map_err(|source| GitCheckpointError::InvalidModel { source })?
            }
            version => return Err(GitCheckpointError::UnsupportedManifest { version }),
        };
        if expected != manifest.owned_paths {
            return Err(GitCheckpointError::OwnershipMismatch);
        }
        if manifest.nonce.is_empty() {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot nonce is empty".to_string(),
            });
        }
        if manifest.sequence == 0 {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot order sequence is zero".to_string(),
            });
        }
        let mode_paths: Vec<String> = manifest.file_modes.keys().cloned().collect();
        validate_workspace_paths(&mode_paths)?;
        ensure_path_limit(mode_paths.len())?;
        if manifest.file_modes.values().any(|mode| mode & !0o777 != 0) {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot contains an invalid regular-file mode".to_string(),
            });
        }
        Ok(())
    }

    fn validate_manifest_tree(
        &self,
        manifest: &SnapshotManifest,
        records: &[TreeRecord],
    ) -> Result<(), GitCheckpointError> {
        let regular_paths: BTreeSet<&str> = records
            .iter()
            .filter_map(|record| match &record.kind {
                TreeKind::Regular { .. } => Some(record.path.as_str()),
                TreeKind::Symlink => None,
            })
            .collect();
        let mode_paths: BTreeSet<&str> = manifest.file_modes.keys().map(String::as_str).collect();
        if regular_paths != mode_paths {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot file modes do not match its regular files".to_string(),
            });
        }
        for record in records {
            let TreeKind::Regular { mode: git_mode } = &record.kind else {
                continue;
            };
            let stored_mode = manifest.file_modes[&record.path];
            if (git_mode & 0o111 == 0) != (stored_mode & 0o111 == 0) {
                return Err(GitCheckpointError::InvalidManifest {
                    message: format!(
                        "snapshot mode metadata disagrees with Git for {:?}",
                        record.path
                    ),
                });
            }
        }
        let declared: BTreeSet<&str> = manifest
            .tracked_paths
            .iter()
            .chain(&manifest.owned_paths)
            .map(String::as_str)
            .collect();
        if records
            .iter()
            .any(|record| !declared.contains(record.path.as_str()))
        {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot tree contains a path outside its declared inventory".to_string(),
            });
        }
        Ok(())
    }

    async fn tracked_paths(&self) -> Result<Vec<String>, GitCheckpointError> {
        let output = self
            .git_output("git ls-files", &["ls-files", "--stage", "-z"], None)
            .await?;
        let mut paths = Vec::new();
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| GitCheckpointError::InvalidManifest {
                    message: "Git returned a malformed index entry".to_string(),
                })?;
            let header =
                std::str::from_utf8(&record[..tab]).map_err(|_| GitCheckpointError::NonUtf8Path)?;
            let mut fields = header.split_ascii_whitespace();
            let mode = fields.next().unwrap_or_default();
            let _object = fields.next().unwrap_or_default();
            let stage = fields.next().unwrap_or_default();
            let path = String::from_utf8(record[tab + 1..].to_vec())
                .map_err(|_| GitCheckpointError::NonUtf8Path)?;
            if stage != "0" || !matches!(mode, "100644" | "100755" | "120000") {
                return Err(GitCheckpointError::UnsupportedTreeEntry {
                    path,
                    mode: mode.to_string(),
                    kind: format!("index stage {stage}"),
                });
            }
            paths.push(path);
            ensure_path_limit(paths.len())?;
        }
        validate_workspace_paths(&paths)?;
        Ok(paths)
    }

    async fn tree_records(
        &self,
        treeish: &GitObjectId,
    ) -> Result<Vec<TreeRecord>, GitCheckpointError> {
        let output = self
            .git_output(
                "git ls-tree",
                &["ls-tree", "-r", "-z", "-l", treeish.as_str()],
                None,
            )
            .await?;
        let mut records = Vec::new();
        let mut total_bytes = 0u64;
        for record in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|record| !record.is_empty())
        {
            let tab = record
                .iter()
                .position(|byte| *byte == b'\t')
                .ok_or_else(|| GitCheckpointError::InvalidManifest {
                    message: "Git returned a malformed tree entry".to_string(),
                })?;
            let header =
                std::str::from_utf8(&record[..tab]).map_err(|_| GitCheckpointError::NonUtf8Path)?;
            let mut fields = header.split_ascii_whitespace();
            let mode = fields.next().unwrap_or_default();
            let kind = fields.next().unwrap_or_default();
            let object = fields.next().unwrap_or_default();
            let size = fields
                .next()
                .unwrap_or_default()
                .parse::<u64>()
                .map_err(|_| GitCheckpointError::InvalidManifest {
                    message: "Git returned a non-numeric blob size".to_string(),
                })?;
            let path = String::from_utf8(record[tab + 1..].to_vec())
                .map_err(|_| GitCheckpointError::NonUtf8Path)?;
            let entry_kind = match (mode, kind) {
                ("100644", "blob") => TreeKind::Regular { mode: 0o644 },
                ("100755", "blob") => TreeKind::Regular { mode: 0o755 },
                ("120000", "blob") => TreeKind::Symlink,
                _ => {
                    return Err(GitCheckpointError::UnsupportedTreeEntry {
                        path,
                        mode: mode.to_string(),
                        kind: kind.to_string(),
                    });
                }
            };
            if size > MAX_SNAPSHOT_BLOB_BYTES {
                return Err(GitCheckpointError::BlobTooLarge {
                    path,
                    length: size,
                    maximum: MAX_SNAPSHOT_BLOB_BYTES,
                });
            }
            total_bytes = total_bytes.saturating_add(size);
            if total_bytes > MAX_SNAPSHOT_TOTAL_BYTES {
                return Err(GitCheckpointError::SnapshotTooLarge {
                    length: total_bytes,
                    maximum: MAX_SNAPSHOT_TOTAL_BYTES,
                });
            }
            let object_id = GitObjectId::try_from(object).map_err(|source| {
                GitCheckpointError::invalid_output("git ls-tree", object, source)
            })?;
            records.push(TreeRecord {
                path,
                object_id,
                size,
                kind: entry_kind,
            });
            ensure_path_limit(records.len())?;
        }
        validate_workspace_paths(
            &records
                .iter()
                .map(|record| record.path.clone())
                .collect::<Vec<_>>(),
        )?;
        Ok(records)
    }

    async fn tree_paths(&self, treeish: &GitObjectId) -> Result<Vec<String>, GitCheckpointError> {
        let output = self
            .git_output(
                "git ls-tree names",
                &["ls-tree", "-r", "-z", "--name-only", treeish.as_str()],
                None,
            )
            .await?;
        let mut paths = Vec::new();
        for path in output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
        {
            paths.push(
                String::from_utf8(path.to_vec()).map_err(|_| GitCheckpointError::NonUtf8Path)?,
            );
            ensure_path_limit(paths.len())?;
        }
        validate_workspace_paths(&paths)?;
        Ok(paths)
    }

    async fn tree_targets(
        &self,
        snapshot: &LoadedSnapshot,
    ) -> Result<BTreeMap<String, MutationTarget>, GitCheckpointError> {
        let mut targets = BTreeMap::new();
        for record in &snapshot.records {
            let output = self
                .git_output_limited(
                    "git cat-file blob",
                    &["cat-file", "blob", record.object_id.as_str()],
                    None,
                    record.size as usize,
                )
                .await?;
            if output.stdout.len() as u64 != record.size {
                return Err(GitCheckpointError::BlobLength {
                    path: record.path.clone(),
                    expected: record.size,
                    actual: output.stdout.len(),
                });
            }
            let target = match &record.kind {
                TreeKind::Regular { .. } => MutationTarget::Regular {
                    contents: output.stdout,
                    mode: Some(snapshot.manifest.file_modes[&record.path]),
                },
                TreeKind::Symlink => MutationTarget::Symlink {
                    target: output.stdout,
                },
            };
            targets.insert(record.path.clone(), target);
        }
        Ok(targets)
    }

    async fn listed_commit_refs(&self, prefix: &str) -> Result<Vec<ListedRef>, GitCheckpointError> {
        let output = self
            .git_output(
                "git for-each-ref",
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)%00%(symref)%00%(objecttype)",
                    prefix,
                ],
                None,
            )
            .await?;
        let text = String::from_utf8(output.stdout).map_err(|_| GitCheckpointError::NonUtf8Path)?;
        let mut refs = Vec::new();
        for line in text.lines().filter(|line| !line.is_empty()) {
            let mut fields = line.split('\0');
            let name = fields.next().unwrap_or_default();
            let object = fields.next().unwrap_or_default();
            let symbolic = fields.next().unwrap_or_default();
            let kind = fields.next().unwrap_or_default();
            if fields.next().is_some() || name.is_empty() || object.is_empty() || kind != "commit" {
                return Err(GitCheckpointError::InvalidManifest {
                    message: "Git returned a malformed checkpoint ref".to_string(),
                });
            }
            let object_id = GitObjectId::try_from(object).map_err(|source| {
                GitCheckpointError::invalid_output("git for-each-ref", object, source)
            })?;
            if !symbolic.is_empty() {
                return Err(GitCheckpointError::SymbolicSnapshot {
                    reference: object_id.into_string(),
                });
            }
            refs.push(ListedRef {
                name: name.to_string(),
                object_id,
            });
        }
        Ok(refs)
    }

    async fn has_exact_pinned_ref(
        &self,
        reference: &str,
        object_id: &GitObjectId,
    ) -> Result<bool, GitCheckpointError> {
        let refs = self.listed_commit_refs(reference).await?;
        if refs.is_empty() {
            return Ok(false);
        }
        if refs.len() != 1
            || refs[0].name != reference
            || refs[0].object_id.as_str() != object_id.as_str()
        {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot ref does not uniquely name its target".to_string(),
            });
        }
        Ok(true)
    }

    async fn validate_order_ref(
        &self,
        object_id: &GitObjectId,
        expected: &str,
    ) -> Result<(), GitCheckpointError> {
        let refs = self.listed_commit_refs(expected).await?;
        if refs.len() != 1
            || refs[0].name != expected
            || refs[0].object_id.as_str() != object_id.as_str()
        {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot order metadata does not match its manifest".to_string(),
            });
        }
        Ok(())
    }

    async fn discover_namespace_refs(
        &self,
        snapshot_prefix: &str,
        order_prefix: &str,
    ) -> Result<Vec<SnapshotRefRecord>, GitCheckpointError> {
        let mut snapshots = BTreeMap::new();
        for listed in self.listed_commit_refs(snapshot_prefix).await? {
            let suffix = listed
                .name
                .strip_prefix(&format!("{snapshot_prefix}/"))
                .ok_or_else(|| GitCheckpointError::InvalidManifest {
                    message: "snapshot ref escaped its namespace".to_string(),
                })?;
            if listed.object_id.as_str() != suffix
                || snapshots
                    .insert(listed.object_id.as_str().to_string(), listed)
                    .is_some()
            {
                return Err(GitCheckpointError::InvalidManifest {
                    message: "snapshot ref does not uniquely name its target".to_string(),
                });
            }
        }

        let mut retained = Vec::with_capacity(snapshots.len());
        let mut sequences = BTreeSet::new();
        for listed in self.listed_commit_refs(order_prefix).await? {
            let suffix = listed
                .name
                .strip_prefix(&format!("{order_prefix}/"))
                .ok_or_else(|| GitCheckpointError::InvalidManifest {
                    message: "snapshot order ref escaped its namespace".to_string(),
                })?;
            if suffix.len() < 22 {
                return Err(GitCheckpointError::InvalidManifest {
                    message: "snapshot order ref is malformed".to_string(),
                });
            }
            let (sequence, object) = suffix.split_at(20);
            let object =
                object
                    .strip_prefix('-')
                    .ok_or_else(|| GitCheckpointError::InvalidManifest {
                        message: "snapshot order ref is malformed".to_string(),
                    })?;
            let sequence =
                sequence
                    .parse::<u64>()
                    .map_err(|_| GitCheckpointError::InvalidManifest {
                        message: "snapshot order sequence is malformed".to_string(),
                    })?;
            if sequence == 0 || !sequences.insert(sequence) || listed.object_id.as_str() != object {
                return Err(GitCheckpointError::InvalidManifest {
                    message: "snapshot order ref has an invalid or duplicate sequence".to_string(),
                });
            }
            let snapshot =
                snapshots
                    .remove(object)
                    .ok_or_else(|| GitCheckpointError::InvalidManifest {
                        message: "snapshot order ref has no snapshot ref".to_string(),
                    })?;
            if snapshot.object_id != listed.object_id {
                return Err(GitCheckpointError::InvalidManifest {
                    message: "snapshot ref pair disagrees on its target".to_string(),
                });
            }
            retained.push(SnapshotRefRecord {
                object_id: listed.object_id,
                sequence,
                snapshot_ref: snapshot.name,
                order_ref: listed.name,
            });
        }
        if !snapshots.is_empty() {
            return Err(GitCheckpointError::InvalidManifest {
                message: "snapshot ref has no order ref".to_string(),
            });
        }
        Ok(retained)
    }

    async fn discover_retained_refs(&self) -> Result<Vec<SnapshotRefRecord>, GitCheckpointError> {
        let snapshot_prefix = self.project_snapshot_ref_prefix();
        let order_prefix = self.project_snapshot_order_ref_prefix();
        let mut retained = self
            .discover_namespace_refs(&snapshot_prefix, &order_prefix)
            .await?;
        if self.supports_legacy_theseus_snapshots() {
            retained.extend(
                self.discover_namespace_refs(SNAPSHOT_REF_PREFIX, SNAPSHOT_ORDER_REF_PREFIX)
                    .await?,
            );
        }
        Ok(retained)
    }

    async fn retained_refs(&self) -> Result<Vec<SnapshotRefRecord>, GitCheckpointError> {
        let retained = self.discover_retained_refs().await?;
        if retained.len() > MAX_SNAPSHOT_REFS {
            return Err(GitCheckpointError::TooManySnapshots {
                length: retained.len(),
                maximum: MAX_SNAPSHOT_REFS,
            });
        }
        Ok(retained)
    }

    async fn restore_plan(
        &self,
        snapshot: &LoadedSnapshot,
        current_owned: &[String],
    ) -> Result<Vec<MutationFile>, GitCheckpointError> {
        let mut desired = self.tree_targets(snapshot).await?;
        let mut paths: BTreeSet<String> = desired.keys().cloned().collect();
        paths.extend(self.tracked_paths().await?);
        paths.extend(snapshot.manifest.tracked_paths.iter().cloned());
        paths.extend(snapshot.manifest.owned_paths.iter().cloned());
        paths.extend(current_owned.iter().cloned());
        ensure_path_limit(paths.len())?;
        let paths: Vec<String> = paths.into_iter().collect();
        validate_workspace_paths(&paths)?;
        let mut current_targets = self.current_targets(paths.clone()).await?;

        let mut changes = Vec::new();
        for path in paths {
            let target = desired.remove(&path).unwrap_or(MutationTarget::Absent);
            let current = current_targets.remove(&path);
            let unchanged = matches!((&current, &target), (None, MutationTarget::Absent))
                || current.as_ref() == Some(&target);
            if !unchanged {
                changes.push(MutationFile { path, target });
            }
        }
        Ok(changes)
    }

    async fn git_text(
        &self,
        operation: &'static str,
        args: &[&str],
    ) -> Result<String, GitCheckpointError> {
        let output = self.git_output(operation, args, None).await?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn git_output(
        &self,
        operation: &'static str,
        args: &[&str],
        index: Option<&Path>,
    ) -> Result<std::process::Output, GitCheckpointError> {
        self.git_output_limited(operation, args, index, MAX_GIT_METADATA_BYTES)
            .await
    }

    async fn git_output_limited(
        &self,
        operation: &'static str,
        args: &[&str],
        index: Option<&Path>,
        maximum: usize,
    ) -> Result<std::process::Output, GitCheckpointError> {
        self.git_output_limited_in(operation, args, index, None, maximum)
            .await
    }

    async fn git_output_limited_in(
        &self,
        operation: &'static str,
        args: &[&str],
        index: Option<&Path>,
        objects: Option<&GitObjectEnvironment>,
        maximum: usize,
    ) -> Result<std::process::Output, GitCheckpointError> {
        let mut command = tokio::process::Command::new("git");
        configure_tokio_git(&mut command);
        command
            .args(args)
            .current_dir(self.project.root())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        if let Some(objects) = objects {
            command
                .env("GIT_OBJECT_DIRECTORY", &objects.directory)
                .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects.alternates);
        }
        let child = command
            .spawn()
            .map_err(|source| GitCheckpointError::Launch { operation, source })?;
        let output = bounded_git_output(child, operation, None, maximum).await?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed(operation, &output));
        }
        Ok(output)
    }

    async fn git_input(
        &self,
        operation: &'static str,
        args: &[&str],
        index: Option<&Path>,
        input: &[u8],
    ) -> Result<std::process::Output, GitCheckpointError> {
        self.git_input_in(operation, args, index, None, input).await
    }

    async fn git_input_in(
        &self,
        operation: &'static str,
        args: &[&str],
        index: Option<&Path>,
        objects: Option<&GitObjectEnvironment>,
        input: &[u8],
    ) -> Result<std::process::Output, GitCheckpointError> {
        let mut command = tokio::process::Command::new("git");
        configure_tokio_git(&mut command);
        command
            .args(args)
            .current_dir(self.project.root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env("GIT_AUTHOR_NAME", "Theseus")
            .env("GIT_AUTHOR_EMAIL", "theseus@localhost")
            .env("GIT_COMMITTER_NAME", "Theseus")
            .env("GIT_COMMITTER_EMAIL", "theseus@localhost");
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        if let Some(objects) = objects {
            command
                .env("GIT_OBJECT_DIRECTORY", &objects.directory)
                .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects.alternates);
        }
        let child = command
            .spawn()
            .map_err(|source| GitCheckpointError::Launch { operation, source })?;
        let output =
            bounded_git_output(child, operation, Some(input), MAX_GIT_METADATA_BYTES).await?;
        if !output.status.success() {
            return Err(GitCheckpointError::command_failed(operation, &output));
        }
        Ok(output)
    }

    async fn update_refs(
        &self,
        create: &[(String, GitObjectId)],
        delete: &[(String, GitObjectId)],
    ) -> Result<(), GitCheckpointError> {
        let mut input = String::from("start\n");
        for (name, object_id) in create {
            input.push_str(&format!("create {} {}\n", name, object_id.as_str()));
        }
        for (name, object_id) in delete {
            input.push_str(&format!("delete {} {}\n", name, object_id.as_str()));
        }
        input.push_str("prepare\ncommit\n");

        self.git_input(
            "git update-ref",
            &["update-ref", "--no-deref", "--stdin"],
            None,
            input.as_bytes(),
        )
        .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Checkpoint for GitCheckpoint {
    async fn context(&self) -> anyhow::Result<crate::ProjectContext> {
        self.project.validate_root()?;
        Ok(self.project.clone())
    }

    async fn snapshot(
        &self,
        request: &CheckpointSnapshotRequest,
    ) -> anyhow::Result<CheckpointSnapshot> {
        self.project.validate_root()?;
        self.validate_snapshot_request(request)?;
        self.validate_repository_root().await?;
        let lease = self.repository_lease(request.expected.clone()).await?;
        let object_lease = self.object_database_lease().await?;
        let retained = self.retained_refs().await?;
        ensure_snapshot_capacity(retained.len())?;
        let sequence = retained
            .iter()
            .map(|record| record.sequence)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| GitCheckpointError::InvalidManifest {
                message: "snapshot order sequence is exhausted".to_string(),
            })?;
        let snapshot_model = SnapshotModelV1::from(&request.model);
        if request
            .project
            .owned_paths(&snapshot_model.clone().into())
            .map_err(|source| GitCheckpointError::InvalidModel { source })?
            != request.owned_paths
        {
            return Err(GitCheckpointError::OwnershipMismatch.into());
        }
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| GitCheckpointError::InvalidClock)?;
        let created_millis = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
        let nonce = format!(
            "{}-{}-{}",
            std::process::id(),
            elapsed.as_nanos(),
            NEXT_SNAPSHOT_NONCE.fetch_add(1, Ordering::Relaxed)
        );
        let empty_modes = BTreeMap::new();
        let preflight = SnapshotManifestRef {
            version: SNAPSHOT_MANIFEST_VERSION,
            project: Some(&request.project),
            label: &request.label,
            created_millis,
            sequence,
            nonce: &nonce,
            owned_paths: &request.owned_paths,
            tracked_paths: &[],
            file_modes: &empty_modes,
            model: &snapshot_model,
        };
        let _ = encode_manifest(&preflight)?;

        self.cleanup_temporary_indices().await?;
        let objects = self.temporary_object_store().await?;
        let tree = self
            .capture_tree_in(&request.owned_paths, Some(&objects.environment))
            .await?;
        let manifest = SnapshotManifestRef {
            version: SNAPSHOT_MANIFEST_VERSION,
            project: Some(&request.project),
            label: &request.label,
            created_millis,
            sequence,
            nonce: &nonce,
            owned_paths: &request.owned_paths,
            tracked_paths: &tree.tracked_paths,
            file_modes: &tree.file_modes,
            model: &snapshot_model,
        };
        let snapshot = self
            .create_snapshot_commit(&tree.object_id, &manifest, Some(&objects.environment))
            .await?;
        #[cfg(test)]
        pause_after_snapshot_quarantine(self.project.root());

        let source = objects.environment.directory.clone();
        let primary = objects.environment.primary.clone();
        JoiningWorker::spawn(move || promote_loose_objects(&source, &primary))?.await??;

        let create = [
            (self.project_snapshot_ref(&snapshot), snapshot.clone()),
            (
                self.project_snapshot_order_ref(sequence, &snapshot),
                snapshot.clone(),
            ),
        ];
        JoiningWorker::spawn(move || objects.cleanup())?.await??;
        self.update_refs(&create, &[]).await?;
        object_lease.commit().map_err(GitCheckpointError::from)?;
        lease.commit().map_err(GitCheckpointError::from)?;
        Ok(CheckpointSnapshot {
            reference: snapshot.into_string(),
        })
    }

    async fn restore(&self, request: &CheckpointStateRequest) -> anyhow::Result<CheckpointRestore> {
        self.project.validate_root()?;
        self.validate_state_request(request)?;
        self.validate_repository_root().await?;
        let mut mutation = self.repository_lease(request.expected.clone()).await?;
        let object_lease = self.object_database_lease().await?;
        let snapshot = self.load_snapshot(&request.reference).await?;
        let changes = self.restore_plan(&snapshot, &request.owned_paths).await?;
        mutation.apply(&changes).await?;
        #[cfg(test)]
        pause_after_restore_apply(self.project.root());
        mutation.commit().map_err(GitCheckpointError::from)?;
        object_lease.commit().map_err(GitCheckpointError::from)?;
        Ok(CheckpointRestore {
            detail: format!(
                "restored the working tree to {}",
                snapshot.object_id.as_str()
            ),
            model: snapshot.manifest.model.into(),
        })
    }

    async fn diff(&self, request: &CheckpointStateRequest) -> anyhow::Result<String> {
        self.project.validate_root()?;
        self.validate_state_request(request)?;
        self.validate_repository_root().await?;
        let lease = self.repository_lease(request.expected.clone()).await?;
        let object_lease = self.object_database_lease().await?;
        let snapshot = self.load_snapshot(&request.reference).await?;
        self.cleanup_temporary_indices().await?;
        let objects = self.temporary_object_store().await?;
        let current = self
            .capture_tree_in(&request.owned_paths, Some(&objects.environment))
            .await?;
        #[cfg(test)]
        pause_after_diff_capture(self.project.root());
        let output = self
            .git_output_limited_in(
                "git diff",
                &[
                    "diff",
                    "--binary",
                    "--no-ext-diff",
                    "--no-textconv",
                    snapshot.tree_id.as_str(),
                    current.object_id.as_str(),
                    "--",
                    ".",
                ],
                None,
                Some(&objects.environment),
                MAX_DIFF_BYTES,
            )
            .await;
        JoiningWorker::spawn(move || objects.cleanup())?.await??;
        let output = output?;
        object_lease.commit().map_err(GitCheckpointError::from)?;
        lease.commit().map_err(GitCheckpointError::from)?;
        let snapshot_modes = snapshot.manifest.file_modes;
        let current_modes = current.file_modes;
        let diff = JoiningWorker::spawn(move || {
            let mut diff = encode_diff(output.stdout)?;
            append_mode_diff(&mut diff, &snapshot_modes, &current_modes)?;
            Ok::<_, GitCheckpointError>(diff)
        })?
        .await??;
        Ok(diff)
    }

    async fn release(&self, request: &str) -> anyhow::Result<String> {
        self.project.validate_root()?;
        self.validate_repository_root().await?;
        let lease = self.repository_lease(Vec::new()).await?;
        let object_lease = self.object_database_lease().await?;
        let snapshot = self.load_snapshot(request).await?;
        let delete = [
            (snapshot.snapshot_ref, snapshot.object_id.clone()),
            (snapshot.order_ref, snapshot.object_id.clone()),
        ];
        self.update_refs(&[], &delete).await?;
        object_lease.commit().map_err(GitCheckpointError::from)?;
        lease.commit().map_err(GitCheckpointError::from)?;
        Ok(format!("released snapshot {}", snapshot.object_id.as_str()))
    }

    async fn prune(&self, request: &SnapshotRetention) -> anyhow::Result<String> {
        self.project.validate_root()?;
        self.validate_repository_root().await?;
        let lease = self.repository_lease(Vec::new()).await?;
        let object_lease = self.object_database_lease().await?;
        let mut retained = self.discover_retained_refs().await?;
        retained.sort_by(|left, right| {
            left.sequence
                .cmp(&right.sequence)
                .then_with(|| left.object_id.as_str().cmp(right.object_id.as_str()))
        });
        let release_count = retained.len().saturating_sub(request.keep as usize);
        let released: Vec<SnapshotRefRecord> = retained.into_iter().take(release_count).collect();
        if !released.is_empty() {
            let mut delete = Vec::with_capacity(released.len() * 2);
            for record in &released {
                delete.push((record.snapshot_ref.clone(), record.object_id.clone()));
                delete.push((record.order_ref.clone(), record.object_id.clone()));
            }
            self.update_refs(&[], &delete).await?;
        }
        object_lease.commit().map_err(GitCheckpointError::from)?;
        lease.commit().map_err(GitCheckpointError::from)?;
        Ok(format!("released {} older snapshots", released.len()))
    }
}

struct LimitedWriter {
    bytes: Vec<u8>,
    maximum: usize,
    exceeded: bool,
}

impl Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.maximum.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("snapshot manifest exceeds its limit"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn project_snapshot_ref_prefix(project_id: &ProjectId) -> String {
    format!("{PROJECT_REF_PREFIX}/{project_id}/snapshots")
}

fn project_snapshot_order_ref_prefix(project_id: &ProjectId) -> String {
    format!("{PROJECT_REF_PREFIX}/{project_id}/snapshot-order")
}

fn encode_manifest(manifest: &(impl Serialize + ?Sized)) -> Result<Vec<u8>, GitCheckpointError> {
    let mut writer = LimitedWriter {
        bytes: Vec::new(),
        maximum: MAX_SNAPSHOT_MANIFEST_BYTES,
        exceeded: false,
    };
    if let Err(source) = serde_json::to_writer(&mut writer, manifest) {
        if writer.exceeded {
            return Err(GitCheckpointError::ManifestTooLarge {
                length: MAX_SNAPSHOT_MANIFEST_BYTES + 1,
                maximum: MAX_SNAPSHOT_MANIFEST_BYTES,
            });
        }
        return Err(GitCheckpointError::SerializeManifest { source });
    }
    Ok(writer.bytes)
}

fn configure_tokio_git(command: &mut tokio::process::Command) {
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("GIT_") {
            command.env_remove(name);
        }
    }
    command
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .arg("-c")
        .arg("core.fsync=loose-object,reference")
        .arg("-c")
        .arg("core.fsyncMethod=fsync")
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-c")
        .arg("core.fsmonitor=false");
}

async fn bounded_git_output(
    child: tokio::process::Child,
    operation: &'static str,
    input: Option<&[u8]>,
    maximum: usize,
) -> Result<std::process::Output, GitCheckpointError> {
    let mut child = ManagedGitChild::new(child);
    let stdin = child.child.stdin.take();
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| GitCheckpointError::InvalidManifest {
            message: format!("{operation} has no standard output pipe"),
        })?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| GitCheckpointError::InvalidManifest {
            message: format!("{operation} has no standard error pipe"),
        })?;
    let write_input = async move {
        if let Some(input) = input {
            let mut stdin = stdin.ok_or_else(|| GitCheckpointError::InvalidManifest {
                message: format!("{operation} has no standard input pipe"),
            })?;
            stdin
                .write_all(input)
                .await
                .map_err(|source| GitCheckpointError::CommandInput { operation, source })?;
            stdin
                .shutdown()
                .await
                .map_err(|source| GitCheckpointError::CommandInput { operation, source })?;
        }
        Ok::<(), GitCheckpointError>(())
    };
    let read_stdout = read_limited_git_stream(stdout, operation, "standard output", maximum);
    let read_stderr =
        read_limited_git_stream(stderr, operation, "standard error", MAX_GIT_ERROR_BYTES);
    let outcome = {
        let wait = async {
            child
                .child
                .wait()
                .await
                .map_err(|source| GitCheckpointError::Launch { operation, source })
        };
        tokio::try_join!(write_input, read_stdout, read_stderr, wait)
    };
    match outcome {
        Ok(((), stdout, stderr, status)) => {
            child.mark_reaped();
            Ok(std::process::Output {
                status,
                stdout,
                stderr,
            })
        }
        Err(error) => {
            child.kill_and_reap().await;
            Err(error)
        }
    }
}

async fn read_limited_git_stream(
    stream: impl AsyncRead + Unpin,
    operation: &'static str,
    stream_name: &'static str,
    maximum: usize,
) -> Result<Vec<u8>, GitCheckpointError> {
    let mut bytes = Vec::new();
    stream
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| GitCheckpointError::CommandOutput { operation, source })?;
    if bytes.len() > maximum {
        return Err(GitCheckpointError::CommandOutputTooLarge {
            operation,
            stream: stream_name,
            maximum,
        });
    }
    Ok(bytes)
}

fn cleanup_temporary_indices(root: &Path) -> Result<(), GitCheckpointError> {
    let control = root.join(".theseus");
    for entry in fs::read_dir(&control).map_err(|source| GitCheckpointError::Launch {
        operation: "reading checkpoint temporary directory",
        source,
    })? {
        let entry = entry.map_err(|source| GitCheckpointError::Launch {
            operation: "reading checkpoint temporary entry",
            source,
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let is_index = name.starts_with("checkpoint-index-");
        let is_object_store = name.starts_with("checkpoint-objects-");
        if !is_index && !is_object_store {
            continue;
        }
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|source| GitCheckpointError::Launch {
                operation: "reading checkpoint temporary metadata",
                source,
            })?;
        if is_index && !metadata.is_file() && !metadata.file_type().is_symlink() {
            return Err(GitCheckpointError::InvalidManifest {
                message: format!(
                    "checkpoint temporary {} is not a file",
                    entry.path().display()
                ),
            });
        }
        if is_object_store && !metadata.is_dir() && !metadata.file_type().is_symlink() {
            return Err(GitCheckpointError::InvalidManifest {
                message: format!(
                    "checkpoint object temporary {} is not a directory",
                    entry.path().display()
                ),
            });
        }
        let removed = if metadata.is_dir() {
            fs::remove_dir_all(entry.path())
        } else {
            fs::remove_file(entry.path())
        };
        removed.map_err(|source| GitCheckpointError::Launch {
            operation: "removing stale checkpoint temporary",
            source,
        })?;
    }
    fs::File::open(&control)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| GitCheckpointError::Launch {
            operation: "syncing checkpoint temporary directory",
            source,
        })
}

fn cleanup_primary_promotion_directory(primary: &Path) -> Result<(), GitCheckpointError> {
    let directory = primary.join(PRIMARY_PROMOTION_DIRECTORY);
    match fs::symlink_metadata(&directory) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(MutationError::Io {
                operation: "reading primary Git promotion directory",
                path: directory,
                source,
            }
            .into());
        }
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(GitCheckpointError::InvalidManifest {
                message: format!(
                    "primary Git promotion path {} is not a directory",
                    directory.display()
                ),
            });
        }
        Ok(_) => {}
    }
    fs::remove_dir_all(&directory).map_err(|source| MutationError::Io {
        operation: "removing stale primary Git promotion directory",
        path: directory,
        source,
    })?;
    sync_directory(primary)
}

fn promote_loose_objects(source: &Path, primary: &Path) -> Result<(), GitCheckpointError> {
    let primary_metadata = fs::symlink_metadata(primary).map_err(|source| MutationError::Io {
        operation: "reading primary Git object directory",
        path: primary.to_path_buf(),
        source,
    })?;
    if primary_metadata.file_type().is_symlink() || !primary_metadata.is_dir() {
        return Err(GitCheckpointError::InvalidManifest {
            message: format!(
                "primary Git object path {} is not a directory",
                primary.display()
            ),
        });
    }

    let promotion_directory = primary.join(PRIMARY_PROMOTION_DIRECTORY);
    fs::create_dir(&promotion_directory).map_err(|source| MutationError::Io {
        operation: "creating primary Git promotion directory",
        path: promotion_directory.clone(),
        source,
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&promotion_directory, fs::Permissions::from_mode(0o700)).map_err(
            |source| MutationError::Io {
                operation: "securing primary Git promotion directory",
                path: promotion_directory.clone(),
                source,
            },
        )?;
    }
    sync_directory(primary)?;

    let mut total_bytes = 0u64;
    for shard in fs::read_dir(source).map_err(|error| MutationError::Io {
        operation: "reading quarantined Git objects",
        path: source.to_path_buf(),
        source: error,
    })? {
        let shard = shard.map_err(|error| MutationError::Io {
            operation: "reading quarantined Git object entry",
            path: source.to_path_buf(),
            source: error,
        })?;
        let shard_name = shard.file_name();
        let shard_name = shard_name.to_string_lossy();
        let shard_metadata =
            fs::symlink_metadata(shard.path()).map_err(|source| MutationError::Io {
                operation: "reading quarantined Git object shard",
                path: shard.path(),
                source,
            })?;
        if shard_name.len() != 2
            || !shard_name.bytes().all(|byte| byte.is_ascii_hexdigit())
            || shard_metadata.file_type().is_symlink()
            || !shard_metadata.is_dir()
        {
            return Err(GitCheckpointError::InvalidManifest {
                message: format!("quarantined Git object shard {shard_name:?} is invalid"),
            });
        }

        let destination_shard = primary.join(shard_name.as_ref());
        match fs::symlink_metadata(&destination_shard) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(GitCheckpointError::InvalidManifest {
                    message: format!(
                        "primary Git object shard {} is not a directory",
                        destination_shard.display()
                    ),
                });
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&destination_shard).map_err(|source| MutationError::Io {
                    operation: "creating primary Git object shard",
                    path: destination_shard.clone(),
                    source,
                })?;
                sync_directory(primary)?;
            }
            Err(source) => {
                return Err(MutationError::Io {
                    operation: "reading primary Git object shard",
                    path: destination_shard,
                    source,
                }
                .into());
            }
        }

        for object in fs::read_dir(shard.path()).map_err(|source| MutationError::Io {
            operation: "reading quarantined Git object shard",
            path: shard.path(),
            source,
        })? {
            let object = object.map_err(|source| MutationError::Io {
                operation: "reading quarantined Git object",
                path: shard.path(),
                source,
            })?;
            let name = object.file_name();
            let name = name.to_string_lossy();
            let metadata =
                fs::symlink_metadata(object.path()).map_err(|source| MutationError::Io {
                    operation: "reading quarantined Git object metadata",
                    path: object.path(),
                    source,
                })?;
            if !matches!(name.len(), 38 | 62)
                || !name.bytes().all(|byte| byte.is_ascii_hexdigit())
                || metadata.file_type().is_symlink()
                || !metadata.is_file()
                || has_multiple_links(&metadata)
            {
                return Err(GitCheckpointError::InvalidManifest {
                    message: format!("quarantined Git object {name:?} is invalid"),
                });
            }
            total_bytes = total_bytes.saturating_add(metadata.len());
            if total_bytes > MAX_PROMOTED_OBJECT_BYTES {
                return Err(GitCheckpointError::SnapshotTooLarge {
                    length: total_bytes,
                    maximum: MAX_PROMOTED_OBJECT_BYTES,
                });
            }

            let destination = destination_shard.join(name.as_ref());
            match fs::symlink_metadata(&destination) {
                Ok(existing) if existing.file_type().is_symlink() || !existing.is_file() => {
                    return Err(GitCheckpointError::InvalidManifest {
                        message: format!(
                            "primary Git object {} is not a regular file",
                            destination.display()
                        ),
                    });
                }
                Ok(_) => {
                    fs::remove_file(object.path()).map_err(|source| MutationError::Io {
                        operation: "removing duplicate quarantined Git object",
                        path: object.path(),
                        source,
                    })?;
                }
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                    promote_object_file(&object.path(), &destination, &promotion_directory)?;
                }
                Err(source) => {
                    return Err(MutationError::Io {
                        operation: "reading primary Git object",
                        path: destination,
                        source,
                    }
                    .into());
                }
            }
        }
        sync_directory(&destination_shard)?;
    }
    fs::remove_dir(&promotion_directory).map_err(|source| MutationError::Io {
        operation: "removing primary Git promotion directory",
        path: promotion_directory,
        source,
    })?;
    sync_directory(primary)
}

fn promote_object_file(
    source: &Path,
    destination: &Path,
    promotion_directory: &Path,
) -> Result<(), GitCheckpointError> {
    match fs::rename(source, destination) {
        Ok(()) => return Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {}
        Err(source) => {
            return Err(MutationError::Io {
                operation: "promoting Git object",
                path: destination.to_path_buf(),
                source,
            }
            .into());
        }
    }

    let sequence = NEXT_TEMP_INDEX.fetch_add(1, Ordering::Relaxed);
    let temporary = promotion_directory.join(format!(
        ".theseus-object-{}-{sequence}.tmp",
        std::process::id()
    ));
    let copied = (|| {
        let mut input_options = OpenOptions::new();
        input_options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            input_options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut input = input_options
            .open(source)
            .map_err(|error| MutationError::Io {
                operation: "opening quarantined Git object",
                path: source.to_path_buf(),
                source: error,
            })?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options
                .mode(0o444)
                .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        }
        let mut output = options
            .open(&temporary)
            .map_err(|source| MutationError::Io {
                operation: "creating promoted Git object temporary",
                path: temporary.clone(),
                source,
            })?;
        std::io::copy(&mut input, &mut output).map_err(|source| MutationError::Io {
            operation: "copying promoted Git object",
            path: temporary.clone(),
            source,
        })?;
        output.sync_all().map_err(|source| MutationError::Io {
            operation: "syncing promoted Git object",
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, destination).map_err(|source| MutationError::Io {
            operation: "publishing promoted Git object",
            path: destination.to_path_buf(),
            source,
        })?;
        fs::remove_file(source).map_err(|error| MutationError::Io {
            operation: "removing copied quarantined Git object",
            path: source.to_path_buf(),
            source: error,
        })?;
        Ok::<_, GitCheckpointError>(())
    })();
    if copied.is_err() {
        let _ = fs::remove_file(temporary);
    }
    copied
}

fn sync_directory(path: &Path) -> Result<(), GitCheckpointError> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| {
            MutationError::Io {
                operation: "syncing Git object directory",
                path: path.to_path_buf(),
                source,
            }
            .into()
        })
}

fn ensure_path_limit(length: usize) -> Result<(), GitCheckpointError> {
    if length > MAX_SNAPSHOT_PATHS {
        return Err(GitCheckpointError::TooManyPaths {
            length,
            maximum: MAX_SNAPSHOT_PATHS,
        });
    }
    Ok(())
}

fn ensure_snapshot_capacity(length: usize) -> Result<(), GitCheckpointError> {
    if length >= MAX_SNAPSHOT_REFS {
        return Err(GitCheckpointError::TooManySnapshots {
            length,
            maximum: MAX_SNAPSHOT_REFS,
        });
    }
    Ok(())
}

fn read_current_targets(
    root: &Path,
    paths: Vec<String>,
) -> Result<BTreeMap<String, MutationTarget>, GitCheckpointError> {
    ensure_path_limit(paths.len())?;
    let mut targets = BTreeMap::new();
    let mut total_bytes = 0u64;
    for path in paths {
        let Some(target) = current_target(root, &path)? else {
            continue;
        };
        let length = match &target {
            MutationTarget::Regular { contents, .. } => contents.len() as u64,
            MutationTarget::Symlink { target } => target.len() as u64,
            MutationTarget::Absent => 0,
        };
        total_bytes = total_bytes.saturating_add(length);
        if total_bytes > MAX_SNAPSHOT_TOTAL_BYTES {
            return Err(GitCheckpointError::SnapshotTooLarge {
                length: total_bytes,
                maximum: MAX_SNAPSHOT_TOTAL_BYTES,
            });
        }
        targets.insert(path, target);
    }
    Ok(targets)
}

fn current_target(root: &Path, path: &str) -> Result<Option<MutationTarget>, GitCheckpointError> {
    validate_workspace_paths(&[path.to_string()])?;
    let relative = Path::new(path);
    let mut current = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        let is_target = components.peek().is_none();
        match fs::symlink_metadata(&current) {
            Ok(metadata) if !is_target && metadata.file_type().is_symlink() => {
                return Err(MutationError::UnsafeTarget {
                    path: path.to_string(),
                    reason: "path crosses a symbolic link",
                }
                .into());
            }
            Ok(metadata) if !is_target && !metadata.is_dir() => {
                return Err(MutationError::UnsafeTarget {
                    path: path.to_string(),
                    reason: "a parent component is not a directory",
                }
                .into());
            }
            Ok(metadata) if is_target && metadata.is_file() => {
                return read_regular_target(&current, path).map(Some);
            }
            Ok(metadata) if is_target && metadata.file_type().is_symlink() => {
                let target = fs::read_link(&current).map_err(|source| MutationError::Io {
                    operation: "reading checkpoint symlink",
                    path: current.clone(),
                    source,
                })?;
                #[cfg(unix)]
                let target = {
                    use std::os::unix::ffi::OsStringExt;
                    target.into_os_string().into_vec()
                };
                #[cfg(not(unix))]
                let target = target.to_string_lossy().into_owned().into_bytes();
                if target.len() as u64 > MAX_SNAPSHOT_BLOB_BYTES {
                    return Err(GitCheckpointError::BlobTooLarge {
                        path: path.to_string(),
                        length: target.len() as u64,
                        maximum: MAX_SNAPSHOT_BLOB_BYTES,
                    });
                }
                return Ok(Some(MutationTarget::Symlink { target }));
            }
            Ok(_) if is_target => {
                return Err(MutationError::UnsafeTarget {
                    path: path.to_string(),
                    reason: "target is not a regular file or symbolic link",
                }
                .into());
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(MutationError::Io {
                    operation: "reading checkpoint path",
                    path: current,
                    source,
                }
                .into());
            }
        }
    }
    Ok(None)
}

fn read_regular_target(path: &Path, display: &str) -> Result<MutationTarget, GitCheckpointError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|source| MutationError::Io {
        operation: "opening checkpoint target",
        path: path.to_path_buf(),
        source,
    })?;
    let before = file.metadata().map_err(|source| MutationError::Io {
        operation: "reading checkpoint target metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if !before.is_file() {
        return Err(MutationError::UnsafeTarget {
            path: display.to_string(),
            reason: "target is not a regular file",
        }
        .into());
    }
    if has_multiple_links(&before) {
        return Err(MutationError::UnsafeTarget {
            path: display.to_string(),
            reason: "target has multiple hard links",
        }
        .into());
    }
    if before.len() > MAX_SNAPSHOT_BLOB_BYTES {
        return Err(GitCheckpointError::BlobTooLarge {
            path: display.to_string(),
            length: before.len(),
            maximum: MAX_SNAPSHOT_BLOB_BYTES,
        });
    }
    let mut contents = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_SNAPSHOT_BLOB_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|source| MutationError::Io {
            operation: "reading checkpoint target",
            path: path.to_path_buf(),
            source,
        })?;
    if contents.len() as u64 > MAX_SNAPSHOT_BLOB_BYTES {
        return Err(GitCheckpointError::BlobTooLarge {
            path: display.to_string(),
            length: contents.len() as u64,
            maximum: MAX_SNAPSHOT_BLOB_BYTES,
        });
    }
    let after = file.metadata().map_err(|source| MutationError::Io {
        operation: "rechecking checkpoint target metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if contents.len() as u64 != before.len() || !same_file_state(&before, &after) {
        return Err(MutationError::StaleWorkspace {
            path: display.to_string(),
        }
        .into());
    }
    Ok(MutationTarget::Regular {
        contents,
        mode: Some(file_mode(&before)),
    })
}

#[cfg(unix)]
fn has_multiple_links(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() > 1
}

#[cfg(not(unix))]
fn has_multiple_links(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mode() == after.mode()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len()
}

fn append_mode_diff(
    diff: &mut String,
    snapshot: &BTreeMap<String, u32>,
    current: &BTreeMap<String, u32>,
) -> Result<(), GitCheckpointError> {
    for (path, before) in snapshot {
        let Some(after) = current.get(path) else {
            continue;
        };
        if before != after {
            let before_path = format!("a/{path}");
            let after_path = format!("b/{path}");
            push_diff(
                diff,
                &format!(
                    "diff --theseus-mode {before_path:?} {after_path:?}\nold mode {before:04o}\nnew mode {after:04o}\n"
                ),
            )?;
        }
    }
    Ok(())
}

fn encode_diff(bytes: Vec<u8>) -> Result<String, GitCheckpointError> {
    let mut output = String::with_capacity(bytes.len().min(MAX_DIFF_BYTES));
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                push_valid_diff(&mut output, valid)?;
                break;
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                let valid = std::str::from_utf8(&remaining[..valid_length]).map_err(|_| {
                    GitCheckpointError::InvalidManifest {
                        message: "UTF-8 validation returned an invalid prefix".to_string(),
                    }
                })?;
                push_valid_diff(&mut output, valid)?;
                let invalid_length = error
                    .error_len()
                    .unwrap_or_else(|| remaining.len() - valid_length);
                for byte in &remaining[valid_length..valid_length + invalid_length] {
                    push_escaped_byte(&mut output, *byte)?;
                }
                remaining = &remaining[valid_length + invalid_length..];
            }
        }
    }
    Ok(output)
}

fn push_valid_diff(output: &mut String, text: &str) -> Result<(), GitCheckpointError> {
    for character in text.chars() {
        if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
            for escaped in character.escape_default() {
                push_diff_char(output, escaped)?;
            }
        } else {
            push_diff_char(output, character)?;
        }
    }
    Ok(())
}

fn push_escaped_byte(output: &mut String, byte: u8) -> Result<(), GitCheckpointError> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for character in [
        '\\',
        'x',
        HEX[(byte >> 4) as usize] as char,
        HEX[(byte & 0x0f) as usize] as char,
    ] {
        push_diff_char(output, character)?;
    }
    Ok(())
}

fn push_diff(output: &mut String, text: &str) -> Result<(), GitCheckpointError> {
    if text.len() > MAX_DIFF_BYTES.saturating_sub(output.len()) {
        return Err(diff_too_large());
    }
    output.push_str(text);
    Ok(())
}

fn push_diff_char(output: &mut String, character: char) -> Result<(), GitCheckpointError> {
    if character.len_utf8() > MAX_DIFF_BYTES.saturating_sub(output.len()) {
        return Err(diff_too_large());
    }
    output.push(character);
    Ok(())
}

fn diff_too_large() -> GitCheckpointError {
    GitCheckpointError::CommandOutputTooLarge {
        operation: "encoding git diff",
        stream: "escaped output",
        maximum: MAX_DIFF_BYTES,
    }
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(test)]
fn pause_after_restore_apply(root: &Path) {
    let Ok(pause_root) = std::env::var("THESEUS_CHECKPOINT_PAUSE_ROOT") else {
        return;
    };
    if Path::new(&pause_root) != root {
        return;
    }
    let marker = root.join("checkpoint-restore-paused");
    fs::write(&marker, b"paused\n").expect("the restore pause marker is written");
    loop {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
fn pause_after_diff_capture(root: &Path) {
    let Ok(pause_root) = std::env::var("THESEUS_CHECKPOINT_DIFF_PAUSE_ROOT") else {
        return;
    };
    if Path::new(&pause_root) != root {
        return;
    }
    let marker = root.join("checkpoint-diff-paused");
    fs::write(&marker, b"paused\n").expect("the diff pause marker is written");
    loop {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
fn pause_after_snapshot_quarantine(root: &Path) {
    let Ok(pause_root) = std::env::var("THESEUS_CHECKPOINT_SNAPSHOT_PAUSE_ROOT") else {
        return;
    };
    if Path::new(&pause_root) != root {
        return;
    }
    let marker = root.join("checkpoint-snapshot-paused");
    fs::write(&marker, b"paused\n").expect("the snapshot pause marker is written");
    loop {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_resource_limits_fail_closed() {
        assert!(ensure_path_limit(MAX_SNAPSHOT_PATHS).is_ok());
        assert!(matches!(
            ensure_path_limit(MAX_SNAPSHOT_PATHS + 1),
            Err(GitCheckpointError::TooManyPaths { .. })
        ));
        assert!(ensure_snapshot_capacity(MAX_SNAPSHOT_REFS - 1).is_ok());
        assert!(matches!(
            ensure_snapshot_capacity(MAX_SNAPSHOT_REFS),
            Err(GitCheckpointError::TooManySnapshots { .. })
        ));
        let oversized = "x".repeat(MAX_SNAPSHOT_MANIFEST_BYTES);
        assert!(matches!(
            encode_manifest(&oversized),
            Err(GitCheckpointError::ManifestTooLarge { .. })
        ));

        let version_one = r#"{
            "version":1,
            "label":"fixture",
            "created_millis":1,
            "sequence":1,
            "nonce":"fixture",
            "owned_paths":[],
            "tracked_paths":[],
            "file_modes":{},
            "model":{
                "name":"fixture",
                "crates":[],
                "types":[],
                "services":[],
                "inbounds":[]
            }
        }"#;
        let manifest: SnapshotManifest =
            serde_json::from_str(version_one).expect("the version-one fixture remains readable");
        let model: theseus_modeling::Model = manifest.model.into();
        assert!(model.clients.is_empty());
    }

    #[test]
    fn diff_encoding_is_bounded_and_line_safe() {
        assert_eq!(
            encode_diff(vec![b'+', 0xff, 0x1b, b'\n']).unwrap(),
            "+\\xff\\u{1b}\n"
        );
        let mut diff = String::new();
        let path = "line\ninjected".to_string();
        append_mode_diff(
            &mut diff,
            &BTreeMap::from([(path.clone(), 0o600)]),
            &BTreeMap::from([(path, 0o644)]),
        )
        .unwrap();
        assert!(diff.contains(r#""a/line\ninjected" "b/line\ninjected""#));
        assert!(!diff.contains("a/line\ninjected b/line"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn canceling_git_output_kills_and_reaps_the_child() {
        let sequence = NEXT_TEMP_INDEX.fetch_add(1, Ordering::Relaxed);
        let pid_file = std::env::temp_dir().join(format!(
            "theseus-checkpoint-child-{}-{sequence}",
            std::process::id()
        ));
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "echo $$ > \"$PID_FILE\"; exec sleep 60"])
            .env("PID_FILE", &pid_file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let child = command.spawn().expect("the child starts");
        let task = tokio::spawn(async move {
            bounded_git_output(child, "cancellation test", None, 1024).await
        });
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while !pid_file.exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the child did not publish its PID"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let pid = fs::read_to_string(&pid_file)
            .expect("the PID file is readable")
            .trim()
            .to_string();

        task.abort();
        assert!(task.await.expect_err("the task is canceled").is_cancelled());
        let alive = std::process::Command::new("kill")
            .args(["-0", &pid])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("kill -0 runs")
            .success();
        let _ = fs::remove_file(pid_file);
        assert!(!alive, "the canceled Git child still exists");
    }
}
