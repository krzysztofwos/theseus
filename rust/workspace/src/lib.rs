//! Crash-recoverable, path-scoped workspace mutations.

mod workspace_path;

use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    future::Future,
    io::{Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
    thread,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::workspace_path::WorkspacePath;

const CONTROL_DIR: &str = ".theseus";
const LOCK_FILE: &str = "repository.lock";
const JOURNAL_DIR: &str = "mutation";
const PREPARING_DIR: &str = "mutation.preparing";
const CLEANUP_DIR: &str = "mutation.cleanup";
const MANIFEST_FILE: &str = "manifest.json";
const STATE_FILE: &str = "state";
const STATE_NEXT_FILE: &str = "state.next";
const PREPARED: &[u8] = b"prepared\n";
const COMMITTED: &[u8] = b"committed\n";
const ROLLED_BACK: &[u8] = b"rolled-back\n";
const JOURNAL_VERSION: u32 = 1;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 64;
const MAX_BACKUP_BYTES: u64 = 256 * 1024 * 1024;

/// The persisted state expected for one model-owned workspace path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExpectedFile {
    pub path: String,
    /// Exact persisted contents, or `None` when the path must not exist.
    pub contents: Option<String>,
}

/// The complete optimistic revision checked after acquiring the repository lease.
pub type ExpectedFileSet = Vec<ExpectedFile>;

/// Validate a complete declared path set using the same normalization,
/// reservation, collision, and overlap rules as a mutation.
pub fn validate_workspace_paths(paths: &[String]) -> Result<(), MutationError> {
    validate_paths(paths.iter().map(String::as_str))
}

/// One path in a declared mutation and its exact desired filesystem state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationFile {
    pub path: String,
    pub target: MutationTarget,
}

/// The state published for a declared mutation path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MutationTarget {
    /// A regular file. `None` preserves an existing regular file's mode and uses
    /// `0644` when the path is new or replaces a symlink.
    Regular {
        contents: Vec<u8>,
        mode: Option<u32>,
    },
    /// A symbolic link whose target is represented as raw Unix path bytes.
    Symlink { target: Vec<u8> },
    /// A tombstone: the path must not exist after publication.
    Absent,
}

impl MutationFile {
    /// A UTF-8 regular-file replacement using the existing/default mode policy.
    pub fn text(path: impl Into<String>, contents: impl Into<String>) -> Self {
        Self::regular(path, contents.into().into_bytes(), None)
    }

    /// An exact regular-file replacement.
    pub fn regular(path: impl Into<String>, contents: Vec<u8>, mode: Option<u32>) -> Self {
        Self {
            path: path.into(),
            target: MutationTarget::Regular { contents, mode },
        }
    }

    /// An exact symbolic-link replacement.
    pub fn symlink(path: impl Into<String>, target: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            target: MutationTarget::Symlink { target },
        }
    }

    /// A deletion tombstone.
    pub fn absent(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            target: MutationTarget::Absent,
        }
    }

    /// UTF-8 contents when this entry is a regular text file.
    pub fn text_contents(&self) -> Option<&str> {
        match &self.target {
            MutationTarget::Regular { contents, .. } => std::str::from_utf8(contents).ok(),
            MutationTarget::Symlink { .. } | MutationTarget::Absent => None,
        }
    }

    /// Whether this entry is a deletion tombstone.
    pub fn is_absent(&self) -> bool {
        matches!(self.target, MutationTarget::Absent)
    }
}

/// A repository mutation held under one process-independent write lease.
#[async_trait]
pub trait WorkspaceMutation: Send + Sync {
    /// Read a workspace-relative file while the repository lease is held.
    async fn read_to_string(&self, path: &str) -> Result<String, MutationError>;

    /// Test a workspace-relative file for existence while the lease is held.
    async fn exists(&self, path: &str) -> Result<bool, MutationError>;

    /// Durably apply the complete declared write set.
    async fn apply(&mut self, files: &[MutationFile]) -> Result<(), MutationError>;

    /// Make an applied write set durable and release its rollback obligation.
    fn commit(self: Box<Self>) -> Result<(), MutationError>;

    /// Restore every declared target and release the repository lease.
    fn rollback(self: Box<Self>) -> Result<(), MutationError>;
}

/// A repository mutation whose lease remains held until commit, rollback, or drop.
pub type PendingMutation = Box<dyn WorkspaceMutation>;

/// The filesystem implementation of a repository mutation.
pub struct FsMutation {
    core: Option<MutationCore>,
}

struct MutationCore {
    root: PathBuf,
    control: PathBuf,
    _lock: File,
    journal_active: bool,
    applied: bool,
}

struct MutationCandidate {
    root: PathBuf,
    control: PathBuf,
    lock: File,
    lock_path: PathBuf,
}

impl FsMutation {
    /// Acquire the canonical repository's lease, recover an interrupted mutation,
    /// and prove that the persisted projection still matches the caller's view.
    pub fn begin(
        root: impl AsRef<Path>,
        expected_files: &[ExpectedFile],
    ) -> Result<PendingMutation, MutationError> {
        Ok(Box::new(Self {
            core: Some(MutationCore::begin(root, expected_files)?),
        }))
    }

    /// Acquire a mutation lease without blocking an async executor thread.
    pub async fn begin_async(
        root: PathBuf,
        expected_files: ExpectedFileSet,
    ) -> Result<PendingMutation, MutationError> {
        ensure_supported_platform()?;
        validate_expected_set(&expected_files)?;
        let candidate = tokio::task::spawn_blocking(move || MutationCandidate::prepare(root))
            .await
            .map_err(|source| MutationError::BlockingTask { source })??;
        loop {
            if candidate.try_lock()? {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let core = ThreadWorker::spawn(move || candidate.finish(&expected_files)).await??;
        Ok(Box::new(Self { core: Some(core) }))
    }

    fn core(&self) -> Result<&MutationCore, MutationError> {
        self.core.as_ref().ok_or(MutationError::MutationInFlight)
    }
}

impl MutationCandidate {
    fn prepare(root: impl AsRef<Path>) -> Result<Self, MutationError> {
        let supplied_root = root.as_ref();
        let root = supplied_root
            .canonicalize()
            .map_err(|source| MutationError::Io {
                operation: "canonicalizing repository root",
                path: supplied_root.to_path_buf(),
                source,
            })?;
        if !root
            .metadata()
            .map_err(|source| MutationError::Io {
                operation: "reading repository root metadata",
                path: root.clone(),
                source,
            })?
            .is_dir()
        {
            return Err(MutationError::RootNotDirectory { root });
        }

        let control = prepare_control_directory(&root)?;
        let lock_path = control.join(LOCK_FILE);
        reject_symlink(&lock_path, "repository lock")?;
        let lock = open_lock_file(&lock_path)?;
        Ok(Self {
            root,
            control,
            lock,
            lock_path,
        })
    }

    fn lock(&self) -> Result<(), MutationError> {
        self.lock.lock().map_err(|source| MutationError::Io {
            operation: "locking repository",
            path: self.lock_path.clone(),
            source,
        })
    }

    fn try_lock(&self) -> Result<bool, MutationError> {
        match self.lock.try_lock() {
            Ok(()) => Ok(true),
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(std::fs::TryLockError::Error(source)) => Err(MutationError::Io {
                operation: "locking repository",
                path: self.lock_path.clone(),
                source,
            }),
        }
    }

    fn finish(self, expected_files: &[ExpectedFile]) -> Result<MutationCore, MutationError> {
        let mut mutation = MutationCore {
            root: self.root,
            control: self.control,
            _lock: self.lock,
            journal_active: false,
            applied: false,
        };
        mutation.recover()?;
        mutation.check_expected(expected_files)?;
        Ok(mutation)
    }
}

impl MutationCore {
    fn begin(
        root: impl AsRef<Path>,
        expected_files: &[ExpectedFile],
    ) -> Result<Self, MutationError> {
        ensure_supported_platform()?;
        validate_expected_set(expected_files)?;
        let candidate = MutationCandidate::prepare(root)?;
        candidate.lock()?;
        candidate.finish(expected_files)
    }

    fn recover(&mut self) -> Result<(), MutationError> {
        self.remove_incomplete_directory(PREPARING_DIR)?;
        self.remove_incomplete_directory(CLEANUP_DIR)?;

        let journal = self.journal_path();
        match fs::symlink_metadata(&journal) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(source) => {
                return Err(MutationError::Io {
                    operation: "reading mutation journal metadata",
                    path: journal,
                    source,
                });
            }
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(MutationError::UnsafeInternalPath { path: journal });
            }
            Ok(_) => {}
        }

        match read_state(&journal)? {
            JournalState::Prepared => {
                self.journal_active = true;
                self.restore_journal()?;
            }
            JournalState::Committed | JournalState::RolledBack => {
                finish_cleanup(&self.control, &journal)?;
            }
        }
        Ok(())
    }

    fn check_expected(&self, expected_files: &[ExpectedFile]) -> Result<(), MutationError> {
        for file in expected_files {
            let relative = parse_path(&file.path)?;
            let path = resolve_target(&self.root, &relative, false, false)?;
            let actual = match fs::read(&path) {
                Ok(actual) => Some(actual),
                Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
                Err(source) => {
                    return Err(MutationError::Io {
                        operation: "reading expected generated file",
                        path,
                        source,
                    });
                }
            };
            if actual.as_deref() != file.contents.as_deref().map(str::as_bytes) {
                return Err(MutationError::StaleWorkspace {
                    path: file.path.clone(),
                });
            }
        }
        Ok(())
    }

    fn apply_files(&mut self, files: &[MutationFile]) -> Result<(), MutationError> {
        if self.journal_active {
            return Err(MutationError::PoisonedMutation);
        }
        if self.applied {
            return Err(MutationError::AlreadyApplied);
        }
        validate_file_set(files)?;
        if files.is_empty() {
            self.applied = true;
            return Ok(());
        }

        let manifest = self.prepare_journal(files)?;
        self.journal_active = true;
        let result = files
            .iter()
            .zip(&manifest.entries)
            .try_for_each(|(file, entry)| {
                let relative = parse_path(&file.path)?;
                match &file.target {
                    MutationTarget::Regular { contents, mode } => {
                        validate_file_mode(*mode, &file.path)?;
                        let target = resolve_target(&self.root, &relative, true, true)?;
                        let temporary = self.root.join(&entry.temporary);
                        let mode = mode.unwrap_or_else(|| {
                            if entry.backup.is_some()
                                && entry.original_kind == OriginalKind::Regular
                            {
                                entry.mode
                            } else {
                                0o644
                            }
                        });
                        write_target(&target, &temporary, contents, mode)
                    }
                    MutationTarget::Symlink { target: link } => {
                        validate_symlink_target(link, &file.path)?;
                        let target = resolve_target(&self.root, &relative, true, true)?;
                        let temporary = self.root.join(&entry.temporary);
                        write_symlink_target(&target, &temporary, link)
                    }
                    MutationTarget::Absent => {
                        let target = resolve_target(&self.root, &relative, false, true)?;
                        remove_declared_target(&target, &file.path)
                    }
                }
            });
        match result {
            Ok(()) => {
                self.applied = true;
                Ok(())
            }
            Err(primary) => match self.restore_manifest(&manifest) {
                Ok(()) => Err(primary),
                Err(rollback) => Err(MutationError::ApplyAndRollback {
                    apply: Box::new(primary),
                    rollback: Box::new(rollback),
                }),
            },
        }
    }

    fn prepare_journal(&self, files: &[MutationFile]) -> Result<JournalManifest, MutationError> {
        let preparing = self.control.join(PREPARING_DIR);
        create_private_directory(&preparing)?;

        let prepared = (|| {
            let mut entries = Vec::with_capacity(files.len());
            let mut created_dirs = Vec::new();
            let mut known_created_dirs = HashSet::new();
            let declared_paths: HashSet<&str> =
                files.iter().map(|file| file.path.as_str()).collect();
            for (index, file) in files.iter().enumerate() {
                let relative = parse_path(&file.path)?;
                if !matches!(&file.target, MutationTarget::Absent) {
                    for directory in missing_parent_directories(&self.root, &relative)? {
                        if known_created_dirs.insert(directory.clone()) {
                            created_dirs.push(directory);
                        }
                    }
                }
                let target = resolve_target(&self.root, &relative, false, true)?;
                let backup = format!("backup-{index:08}");
                let temporary = temporary_path(&relative, index)?;
                if declared_paths.contains(temporary.to_str().unwrap_or_default()) {
                    return Err(MutationError::UnsafeTarget {
                        path: temporary.display().to_string(),
                        reason: "generated target collides with a transaction temporary",
                    });
                }
                let temporary_absolute = self.root.join(&temporary);
                match fs::symlink_metadata(&temporary_absolute) {
                    Ok(_) => {
                        return Err(MutationError::UnsafeTarget {
                            path: temporary.display().to_string(),
                            reason: "reserved transaction temporary already exists",
                        });
                    }
                    Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
                    Err(source) => {
                        return Err(MutationError::Io {
                            operation: "checking generated temporary",
                            path: temporary_absolute,
                            source,
                        });
                    }
                }
                let entry = match read_target_backup(&target, &file.path)? {
                    Some(backup_contents) => {
                        write_private(&preparing.join(&backup), &backup_contents.contents)?;
                        JournalEntry {
                            path: file.path.clone(),
                            backup: Some(backup),
                            mode: backup_contents.mode,
                            original_kind: backup_contents.kind,
                            temporary: temporary.display().to_string(),
                        }
                    }
                    None => JournalEntry {
                        path: file.path.clone(),
                        backup: None,
                        mode: 0,
                        original_kind: OriginalKind::Regular,
                        temporary: temporary.display().to_string(),
                    },
                };
                entries.push(entry);
            }

            let manifest = JournalManifest {
                version: JOURNAL_VERSION,
                entries,
                created_dirs,
            };
            let encoded = serde_json::to_vec(&manifest)
                .map_err(|source| MutationError::SerializeJournal { source })?;
            let manifest_path = preparing.join(MANIFEST_FILE);
            if encoded.len() as u64 > MAX_MANIFEST_BYTES {
                return Err(MutationError::PrivateFileTooLarge {
                    path: manifest_path,
                    length: encoded.len() as u64,
                    maximum: MAX_MANIFEST_BYTES,
                });
            }
            write_private(&manifest_path, &encoded)?;
            write_private(&preparing.join(STATE_FILE), PREPARED)?;
            sync_directory(&preparing)?;
            fs::rename(&preparing, self.journal_path()).map_err(|source| MutationError::Io {
                operation: "publishing mutation journal",
                path: preparing.clone(),
                source,
            })?;
            sync_directory(&self.control)?;
            Ok(manifest)
        })();

        if prepared.is_err() {
            let _ = fs::remove_dir_all(&preparing);
            let _ = sync_directory(&self.control);
        }
        prepared
    }

    fn restore_journal(&mut self) -> Result<(), MutationError> {
        let manifest = read_manifest(&self.journal_path())?;
        self.restore_manifest(&manifest)
    }

    fn restore_manifest(&mut self, manifest: &JournalManifest) -> Result<(), MutationError> {
        let journal = self.journal_path();
        for entry in manifest.entries.iter().rev() {
            let relative = parse_path(&entry.path)?;
            let target = resolve_target(&self.root, &relative, entry.backup.is_some(), true)?;
            let temporary = self.root.join(&entry.temporary);
            remove_temporary(&temporary)?;
            if let Some(backup) = &entry.backup {
                let backup_path = journal.join(backup);
                reject_symlink(&backup_path, "mutation backup")?;
                let contents = read_private(&backup_path, MAX_BACKUP_BYTES)?;
                match entry.original_kind {
                    OriginalKind::Regular => {
                        write_target(&target, &temporary, &contents, entry.mode)?;
                    }
                    OriginalKind::Symlink => {
                        validate_symlink_target(&contents, &entry.path)?;
                        write_symlink_target(&target, &temporary, &contents)?;
                    }
                }
            } else {
                remove_created_target(&target, &entry.path)?;
            }
        }
        remove_created_directories(&self.root, &manifest.created_dirs)?;
        write_state(&journal, ROLLED_BACK)?;
        self.journal_active = false;
        self.applied = false;
        finish_cleanup(&self.control, &journal)
    }

    fn commit_inner(&mut self) -> Result<(), MutationError> {
        if self.journal_active && !self.applied {
            return Err(MutationError::PoisonedMutation);
        }
        if !self.journal_active {
            return Ok(());
        }
        let journal = self.journal_path();
        write_state(&journal, COMMITTED)?;
        self.journal_active = false;
        // The durable marker is the commit point. Recovery can finish cleanup;
        // reporting failure here would leave the caller's model behind its disk.
        let _ = finish_cleanup(&self.control, &journal);
        Ok(())
    }

    fn rollback_inner(&mut self) -> Result<(), MutationError> {
        if self.journal_active {
            self.restore_journal()?;
        }
        Ok(())
    }

    fn remove_incomplete_directory(&self, name: &str) -> Result<(), MutationError> {
        let path = self.control.join(name);
        match fs::symlink_metadata(&path) {
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(MutationError::Io {
                operation: "reading internal cleanup path",
                path,
                source,
            }),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&path).map_err(|source| MutationError::Io {
                    operation: "removing incomplete mutation journal",
                    path: path.clone(),
                    source,
                })?;
                sync_directory(&self.control)
            }
            Ok(_) => {
                fs::remove_file(&path).map_err(|source| MutationError::Io {
                    operation: "removing incomplete mutation blocker",
                    path: path.clone(),
                    source,
                })?;
                sync_directory(&self.control)
            }
        }
    }

    fn journal_path(&self) -> PathBuf {
        self.control.join(JOURNAL_DIR)
    }
}

#[async_trait]
impl WorkspaceMutation for FsMutation {
    async fn read_to_string(&self, path: &str) -> Result<String, MutationError> {
        let root = self.core()?.root.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || read_workspace_file(&root, &path))
            .await
            .map_err(|source| MutationError::BlockingTask { source })?
    }

    async fn exists(&self, path: &str) -> Result<bool, MutationError> {
        let root = self.core()?.root.clone();
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || workspace_file_exists(&root, &path))
            .await
            .map_err(|source| MutationError::BlockingTask { source })?
    }

    async fn apply(&mut self, files: &[MutationFile]) -> Result<(), MutationError> {
        let mut core = self.core.take().ok_or(MutationError::MutationInFlight)?;
        let files = files.to_vec();
        let worker = ThreadWorker::spawn(move || {
            let result = core.apply_files(&files);
            (core, result)
        });
        let (returned, result) = worker.await?;
        self.core = Some(returned);
        result
    }

    fn commit(mut self: Box<Self>) -> Result<(), MutationError> {
        let mut core = self.core.take().ok_or(MutationError::MutationInFlight)?;
        core.commit_inner()
    }

    fn rollback(mut self: Box<Self>) -> Result<(), MutationError> {
        let mut core = self.core.take().ok_or(MutationError::MutationInFlight)?;
        core.rollback_inner()
    }
}

/// Dropping a blocking critical section joins its worker before buffered state
/// is discarded, so cancellation cannot outlive recovery or rollback.
struct ThreadWorker<T> {
    receiver: tokio::sync::oneshot::Receiver<T>,
    thread: Option<thread::JoinHandle<()>>,
}

impl<T: Send + 'static> ThreadWorker<T> {
    fn spawn(work: impl FnOnce() -> T + Send + 'static) -> Self {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let thread = thread::spawn(move || {
            let _ = sender.send(work());
        });
        Self {
            receiver,
            thread: Some(thread),
        }
    }
}

impl<T> ThreadWorker<T> {
    fn join(&mut self) -> Result<(), MutationError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread.join().map_err(|_| MutationError::WorkerPanicked)
    }
}

impl<T> Unpin for ThreadWorker<T> {}

impl<T> Future for ThreadWorker<T> {
    type Output = Result<T, MutationError>;

    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(context) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(output)) => match self.join() {
                Ok(()) => Poll::Ready(Ok(output)),
                Err(error) => Poll::Ready(Err(error)),
            },
            Poll::Ready(Err(_)) => {
                let error = self.join().err().unwrap_or(MutationError::WorkerPanicked);
                Poll::Ready(Err(error))
            }
        }
    }
}

impl<T> Drop for ThreadWorker<T> {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

impl Drop for MutationCore {
    fn drop(&mut self) {
        if self.journal_active {
            let _ = self.rollback_inner();
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalManifest {
    version: u32,
    entries: Vec<JournalEntry>,
    created_dirs: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct JournalEntry {
    path: String,
    backup: Option<String>,
    mode: u32,
    #[serde(default)]
    original_kind: OriginalKind,
    temporary: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum OriginalKind {
    #[default]
    Regular,
    Symlink,
}

struct TargetBackup {
    contents: Vec<u8>,
    mode: u32,
    kind: OriginalKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalState {
    Prepared,
    Committed,
    RolledBack,
}

/// A filesystem mutation that could not be completed safely.
#[derive(Debug, Error)]
pub enum MutationError {
    #[error("recoverable workspace mutations are supported only on Unix")]
    UnsupportedPlatform,
    #[error("repository root {root} is not a directory", root = .root.display())]
    RootNotDirectory { root: PathBuf },
    #[error("generated path {path:?} is unsafe: {reason}")]
    UnsafeTarget { path: String, reason: &'static str },
    #[error("generated path {path:?} is declared more than once")]
    DuplicatePath { path: String },
    #[error("generated paths {ancestor:?} and {descendant:?} overlap")]
    OverlappingPaths {
        ancestor: String,
        descendant: String,
    },
    #[error("generated path {path:?} is stale on disk")]
    StaleWorkspace { path: String },
    #[error("internal mutation path {path} is not a private regular path", path = .path.display())]
    UnsafeInternalPath { path: PathBuf },
    #[error("mutation journal input {path} is {length} bytes; the maximum is {maximum}", path = .path.display())]
    PrivateFileTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },
    #[error("the mutation write set was already applied")]
    AlreadyApplied,
    #[error(
        "the mutation is poisoned by an incomplete rollback and cannot be committed or retried"
    )]
    PoisonedMutation,
    #[error("the mutation is currently executing on a blocking worker")]
    MutationInFlight,
    #[error("mutation journal has unsupported version {version}")]
    UnsupportedJournal { version: u32 },
    #[error("mutation journal state is corrupt")]
    CorruptJournalState,
    #[error("serializing mutation journal")]
    SerializeJournal {
        #[source]
        source: serde_json::Error,
    },
    #[error("parsing mutation journal")]
    ParseJournal {
        #[source]
        source: serde_json::Error,
    },
    #[error("mutation failed: {apply}; its rollback also failed: {rollback}")]
    ApplyAndRollback {
        apply: Box<MutationError>,
        rollback: Box<MutationError>,
    },
    #[error("joining filesystem mutation worker")]
    BlockingTask {
        #[source]
        source: tokio::task::JoinError,
    },
    #[error("the filesystem mutation worker panicked")]
    WorkerPanicked,
    #[error("{operation} at {path}", path = .path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

fn validate_file_set(files: &[MutationFile]) -> Result<(), MutationError> {
    validate_paths(files.iter().map(|file| file.path.as_str()))?;
    for file in files {
        match &file.target {
            MutationTarget::Regular { mode, .. } => validate_file_mode(*mode, &file.path)?,
            MutationTarget::Symlink { target } => validate_symlink_target(target, &file.path)?,
            MutationTarget::Absent => {}
        }
    }
    Ok(())
}

fn validate_expected_set(files: &[ExpectedFile]) -> Result<(), MutationError> {
    validate_paths(files.iter().map(|file| file.path.as_str()))
}

fn validate_paths<'a>(paths: impl Iterator<Item = &'a str>) -> Result<(), MutationError> {
    let paths: Vec<&str> = paths.collect();
    let mut seen = HashSet::with_capacity(paths.len());
    let mut seen_folded = HashSet::with_capacity(paths.len());
    let mut parsed: Vec<(String, WorkspacePath)> = Vec::with_capacity(paths.len());
    let mut parsed_folded: Vec<(String, PathBuf)> = Vec::with_capacity(paths.len());
    for path in paths {
        let relative = parse_path(path)?;
        let folded = path.to_ascii_lowercase();
        if !seen.insert(path) || !seen_folded.insert(folded.clone()) {
            return Err(MutationError::DuplicatePath {
                path: path.to_string(),
            });
        }
        for (other_path, other) in &parsed {
            if relative.as_path().starts_with(other.as_path()) {
                return Err(MutationError::OverlappingPaths {
                    ancestor: other_path.clone(),
                    descendant: path.to_string(),
                });
            }
            if other.as_path().starts_with(relative.as_path()) {
                return Err(MutationError::OverlappingPaths {
                    ancestor: path.to_string(),
                    descendant: other_path.clone(),
                });
            }
        }
        let folded_path = PathBuf::from(&folded);
        for (other_path, other) in &parsed_folded {
            if folded_path.starts_with(other) {
                return Err(MutationError::OverlappingPaths {
                    ancestor: other_path.clone(),
                    descendant: path.to_string(),
                });
            }
            if other.starts_with(&folded_path) {
                return Err(MutationError::OverlappingPaths {
                    ancestor: path.to_string(),
                    descendant: other_path.clone(),
                });
            }
        }
        parsed.push((path.to_string(), relative));
        parsed_folded.push((path.to_string(), folded_path));
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_supported_platform() -> Result<(), MutationError> {
    Ok(())
}

#[cfg(not(unix))]
fn ensure_supported_platform() -> Result<(), MutationError> {
    Err(MutationError::UnsupportedPlatform)
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

fn parse_path(path: &str) -> Result<WorkspacePath, MutationError> {
    let relative = WorkspacePath::try_from(path).map_err(|_| MutationError::UnsafeTarget {
        path: path.to_owned(),
        reason: "path must be normalized and relative to the repository",
    })?;
    if relative.components().next().is_some_and(|component| {
        component
            .to_string_lossy()
            .eq_ignore_ascii_case(CONTROL_DIR)
    }) {
        return Err(MutationError::UnsafeTarget {
            path: path.to_owned(),
            reason: "the .theseus control directory is reserved",
        });
    }
    Ok(relative)
}

fn resolve_target(
    root: &Path,
    relative: &WorkspacePath,
    create_parents: bool,
    allow_target_symlink: bool,
) -> Result<PathBuf, MutationError> {
    let display = relative.as_path().display().to_string();
    let mut current = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        current.push(component);
        let is_target = components.peek().is_none();
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if metadata.file_type().is_symlink() && (!is_target || !allow_target_symlink) =>
            {
                return Err(MutationError::UnsafeTarget {
                    path: display,
                    reason: "path crosses a symbolic link",
                });
            }
            Ok(metadata) if !is_target && !metadata.is_dir() => {
                return Err(MutationError::UnsafeTarget {
                    path: display,
                    reason: "a parent component is not a directory",
                });
            }
            Ok(metadata)
                if is_target
                    && !metadata.is_file()
                    && !(allow_target_symlink && metadata.file_type().is_symlink()) =>
            {
                return Err(MutationError::UnsafeTarget {
                    path: display,
                    reason: "target is not a regular file",
                });
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound && !is_target => {
                if !create_parents {
                    current.extend(components);
                    return Ok(current);
                }
                create_workspace_directory(&current)?;
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MutationError::Io {
                    operation: "resolving generated path",
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(current)
}

fn missing_parent_directories(
    root: &Path,
    relative: &WorkspacePath,
) -> Result<Vec<String>, MutationError> {
    let display = relative.as_path().display().to_string();
    let parent = relative
        .as_path()
        .parent()
        .expect("a workspace path always has a relative parent");
    let mut current = root.to_path_buf();
    let mut relative_current = PathBuf::new();
    let mut missing = false;
    let mut directories = Vec::new();
    for component in parent.components() {
        current.push(component);
        relative_current.push(component);
        if missing {
            directories.push(relative_current.display().to_string());
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(MutationError::UnsafeTarget {
                    path: display,
                    reason: "a parent component is not a real directory",
                });
            }
            Ok(_) => {}
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                missing = true;
                directories.push(relative_current.display().to_string());
            }
            Err(source) => {
                return Err(MutationError::Io {
                    operation: "inspecting generated-file parents",
                    path: current,
                    source,
                });
            }
        }
    }
    Ok(directories)
}

fn temporary_path(relative: &WorkspacePath, index: usize) -> Result<PathBuf, MutationError> {
    let parent = relative.as_path().parent().unwrap_or_else(|| Path::new(""));
    let temporary = parent.join(format!(".theseus-mutation-write-{index:08}.tmp"));
    let display = temporary.display().to_string();
    parse_path(&display)?;
    Ok(temporary)
}

fn read_target_backup(path: &Path, display: &str) -> Result<Option<TargetBackup>, MutationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = fs::read_link(path).map_err(|source| MutationError::Io {
                operation: "reading workspace symlink for backup",
                path: path.to_path_buf(),
                source,
            })?;
            #[cfg(unix)]
            let contents = {
                use std::os::unix::ffi::OsStringExt;
                target.into_os_string().into_vec()
            };
            #[cfg(not(unix))]
            let contents = target.to_string_lossy().into_owned().into_bytes();
            if contents.len() as u64 > MAX_BACKUP_BYTES {
                return Err(MutationError::PrivateFileTooLarge {
                    path: path.to_path_buf(),
                    length: contents.len() as u64,
                    maximum: MAX_BACKUP_BYTES,
                });
            }
            return Ok(Some(TargetBackup {
                contents,
                mode: 0,
                kind: OriginalKind::Symlink,
            }));
        }
        Ok(metadata) if !metadata.is_file() => {
            return Err(MutationError::UnsafeTarget {
                path: display.to_owned(),
                reason: "target is not a regular file or symbolic link",
            });
        }
        Ok(_) => {}
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MutationError::Io {
                operation: "reading workspace target metadata",
                path: path.to_path_buf(),
                source,
            });
        }
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(MutationError::Io {
                operation: "opening generated file for backup",
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let before = file.metadata().map_err(|source| MutationError::Io {
        operation: "reading generated file metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if !before.is_file() {
        return Err(MutationError::UnsafeTarget {
            path: display.to_owned(),
            reason: "target is not a regular file",
        });
    }
    if has_multiple_links(&before) {
        return Err(MutationError::UnsafeTarget {
            path: display.to_owned(),
            reason: "target has multiple hard links",
        });
    }
    if before.len() > MAX_BACKUP_BYTES {
        return Err(MutationError::PrivateFileTooLarge {
            path: path.to_path_buf(),
            length: before.len(),
            maximum: MAX_BACKUP_BYTES,
        });
    }
    let mut contents = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_BACKUP_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|source| MutationError::Io {
            operation: "backing up generated file",
            path: path.to_path_buf(),
            source,
        })?;
    let after = file.metadata().map_err(|source| MutationError::Io {
        operation: "rechecking generated file metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if contents.len() as u64 != before.len() || after.len() != before.len() {
        return Err(MutationError::StaleWorkspace {
            path: display.to_owned(),
        });
    }
    Ok(Some(TargetBackup {
        contents,
        mode: file_mode(&before),
        kind: OriginalKind::Regular,
    }))
}

fn validate_file_mode(mode: Option<u32>, path: &str) -> Result<(), MutationError> {
    if mode.is_some_and(|mode| mode & !0o777 != 0) {
        return Err(MutationError::UnsafeTarget {
            path: path.to_owned(),
            reason: "regular-file mode must contain only Unix permission bits",
        });
    }
    Ok(())
}

fn validate_symlink_target(target: &[u8], path: &str) -> Result<(), MutationError> {
    if target.is_empty() || target.contains(&0) {
        return Err(MutationError::UnsafeTarget {
            path: path.to_owned(),
            reason: "symbolic-link target must be non-empty and contain no NUL byte",
        });
    }
    Ok(())
}

fn read_workspace_file(root: &Path, path: &str) -> Result<String, MutationError> {
    let relative = parse_path(path)?;
    let target = resolve_target(root, &relative, false, false)?;
    fs::read_to_string(&target).map_err(|source| MutationError::Io {
        operation: "reading workspace file",
        path: target,
        source,
    })
}

fn workspace_file_exists(root: &Path, path: &str) -> Result<bool, MutationError> {
    let relative = parse_path(path)?;
    let target = resolve_target(root, &relative, false, false)?;
    match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.is_file() => Ok(true),
        Ok(_) => Err(MutationError::UnsafeTarget {
            path: path.to_owned(),
            reason: "target is not a regular file",
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(MutationError::Io {
            operation: "reading workspace file metadata",
            path: target,
            source,
        }),
    }
}

fn prepare_control_directory(root: &Path) -> Result<PathBuf, MutationError> {
    let control = root.join(CONTROL_DIR);
    match fs::symlink_metadata(&control) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(MutationError::UnsafeInternalPath { path: control });
        }
        Ok(_) => set_directory_mode(&control, 0o700)?,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            create_private_directory(&control)?;
            sync_directory(root)?;
        }
        Err(source) => {
            return Err(MutationError::Io {
                operation: "reading control directory metadata",
                path: control,
                source,
            });
        }
    }
    Ok(control)
}

fn create_private_directory(path: &Path) -> Result<(), MutationError> {
    create_directory(path, 0o700, "creating private mutation directory")
}

fn create_workspace_directory(path: &Path) -> Result<(), MutationError> {
    let parent = path.parent().unwrap_or(path);
    create_directory(path, 0o755, "creating generated-file parent")?;
    sync_directory(parent)
}

fn create_directory(path: &Path, mode: u32, operation: &'static str) -> Result<(), MutationError> {
    #[cfg(unix)]
    let result = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(mode).create(path)
    };
    #[cfg(not(unix))]
    let result = fs::create_dir(path);

    result.map_err(|source| MutationError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    })?;
    set_directory_mode(path, mode)
}

fn open_lock_file(path: &Path) -> Result<File, MutationError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|source| MutationError::Io {
        operation: "opening private mutation file",
        path: path.to_path_buf(),
        source,
    })?;
    validate_private_file(&file, path)?;
    set_open_file_mode(&file, 0o600).map_err(|source| MutationError::Io {
        operation: "setting private mutation file permissions",
        path: path.to_path_buf(),
        source,
    })?;
    Ok(file)
}

fn write_private(path: &Path, contents: &[u8]) -> Result<(), MutationError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|source| MutationError::Io {
        operation: "creating private mutation file",
        path: path.to_path_buf(),
        source,
    })?;
    validate_private_file(&file, path)?;
    set_open_file_mode(&file, 0o600).map_err(|source| MutationError::Io {
        operation: "setting private mutation file permissions",
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(contents)
        .and_then(|()| file.sync_all())
        .map_err(|source| MutationError::Io {
            operation: "writing private mutation file",
            path: path.to_path_buf(),
            source,
        })
}

fn read_private(path: &Path, maximum: u64) -> Result<Vec<u8>, MutationError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(path).map_err(|source| MutationError::Io {
        operation: "opening private mutation file",
        path: path.to_path_buf(),
        source,
    })?;
    let length = validate_private_file(&file, path)?.len();
    if length > maximum {
        return Err(MutationError::PrivateFileTooLarge {
            path: path.to_path_buf(),
            length,
            maximum,
        });
    }
    let mut contents = Vec::with_capacity(length as usize);
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut contents)
        .map_err(|source| MutationError::Io {
            operation: "reading private mutation file",
            path: path.to_path_buf(),
            source,
        })?;
    if contents.len() as u64 > maximum {
        return Err(MutationError::PrivateFileTooLarge {
            path: path.to_path_buf(),
            length: contents.len() as u64,
            maximum,
        });
    }
    Ok(contents)
}

fn validate_private_file(file: &File, path: &Path) -> Result<fs::Metadata, MutationError> {
    let metadata = file.metadata().map_err(|source| MutationError::Io {
        operation: "reading private mutation file metadata",
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.is_file() || has_multiple_links(&metadata) {
        return Err(MutationError::UnsafeInternalPath {
            path: path.to_path_buf(),
        });
    }
    Ok(metadata)
}

fn write_target(
    path: &Path,
    temporary: &Path,
    contents: &[u8],
    mode: u32,
) -> Result<(), MutationError> {
    reject_symlink(temporary, "generated temporary")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let mut file = options
        .open(temporary)
        .map_err(|source| MutationError::Io {
            operation: "opening generated temporary",
            path: temporary.to_path_buf(),
            source,
        })?;
    let staged = file
        .write_all(contents)
        .and_then(|()| set_open_file_mode(&file, mode))
        .and_then(|()| file.sync_all());
    if let Err(source) = staged {
        let _ = fs::remove_file(temporary);
        return Err(MutationError::Io {
            operation: "writing generated temporary",
            path: temporary.to_path_buf(),
            source,
        });
    }
    drop(file);
    if let Err(source) = fs::rename(temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(MutationError::Io {
            operation: "publishing generated target",
            path: path.to_path_buf(),
            source,
        });
    }
    sync_directory(path.parent().unwrap_or(path))
}

#[cfg(unix)]
fn write_symlink_target(path: &Path, temporary: &Path, target: &[u8]) -> Result<(), MutationError> {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};

    reject_symlink(temporary, "generated temporary")?;
    let target = std::ffi::OsString::from_vec(target.to_vec());
    symlink(target, temporary).map_err(|source| MutationError::Io {
        operation: "creating symbolic-link temporary",
        path: temporary.to_path_buf(),
        source,
    })?;
    if let Err(source) = fs::rename(temporary, path) {
        let _ = fs::remove_file(temporary);
        return Err(MutationError::Io {
            operation: "publishing symbolic-link target",
            path: path.to_path_buf(),
            source,
        });
    }
    sync_directory(path.parent().unwrap_or(path))
}

#[cfg(not(unix))]
fn write_symlink_target(
    _path: &Path,
    _temporary: &Path,
    _target: &[u8],
) -> Result<(), MutationError> {
    Err(MutationError::UnsupportedPlatform)
}

fn write_state(journal: &Path, state: &[u8]) -> Result<(), MutationError> {
    let next = journal.join(STATE_NEXT_FILE);
    remove_private_temporary(&next)?;
    write_private(&next, state)?;
    fs::rename(&next, journal.join(STATE_FILE)).map_err(|source| MutationError::Io {
        operation: "publishing mutation journal state",
        path: next,
        source,
    })?;
    sync_directory(journal)
}

fn remove_private_temporary(path: &Path) -> Result<(), MutationError> {
    match fs::symlink_metadata(path) {
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MutationError::Io {
            operation: "reading private mutation temporary metadata",
            path: path.to_path_buf(),
            source,
        }),
        Ok(_) => {
            read_private(path, MAX_STATE_BYTES)?;
            fs::remove_file(path).map_err(|source| MutationError::Io {
                operation: "removing stale private mutation temporary",
                path: path.to_path_buf(),
                source,
            })?;
            sync_directory(path.parent().unwrap_or(path))
        }
    }
}

fn read_state(journal: &Path) -> Result<JournalState, MutationError> {
    let path = journal.join(STATE_FILE);
    let state = read_private(&path, MAX_STATE_BYTES)?;
    match state.as_slice() {
        PREPARED => Ok(JournalState::Prepared),
        COMMITTED => Ok(JournalState::Committed),
        ROLLED_BACK => Ok(JournalState::RolledBack),
        _ => Err(MutationError::CorruptJournalState),
    }
}

fn read_manifest(journal: &Path) -> Result<JournalManifest, MutationError> {
    let path = journal.join(MANIFEST_FILE);
    let encoded = read_private(&path, MAX_MANIFEST_BYTES)?;
    let manifest: JournalManifest = serde_json::from_slice(&encoded)
        .map_err(|source| MutationError::ParseJournal { source })?;
    if manifest.version != JOURNAL_VERSION {
        return Err(MutationError::UnsupportedJournal {
            version: manifest.version,
        });
    }
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &JournalManifest) -> Result<(), MutationError> {
    let mut paths = HashSet::with_capacity(manifest.entries.len());
    let mut backups = HashSet::with_capacity(manifest.entries.len());
    let mut parsed_paths: Vec<(String, WorkspacePath)> = Vec::with_capacity(manifest.entries.len());
    let declared_paths: HashSet<&str> = manifest
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    for (index, entry) in manifest.entries.iter().enumerate() {
        let relative = parse_path(&entry.path)?;
        if !paths.insert(entry.path.as_str()) {
            return Err(MutationError::DuplicatePath {
                path: entry.path.clone(),
            });
        }
        for (other_path, other) in &parsed_paths {
            if relative.as_path().starts_with(other.as_path()) {
                return Err(MutationError::OverlappingPaths {
                    ancestor: other_path.clone(),
                    descendant: entry.path.clone(),
                });
            }
            if other.as_path().starts_with(relative.as_path()) {
                return Err(MutationError::OverlappingPaths {
                    ancestor: entry.path.clone(),
                    descendant: other_path.clone(),
                });
            }
        }
        let expected_temporary = temporary_path(&relative, index)?;
        if Path::new(&entry.temporary) != expected_temporary
            || declared_paths.contains(entry.temporary.as_str())
        {
            return Err(MutationError::CorruptJournalState);
        }
        if let Some(backup) = &entry.backup {
            let expected = format!("backup-{index:08}");
            if backup != &expected || !backups.insert(backup.as_str()) {
                return Err(MutationError::CorruptJournalState);
            }
            match entry.original_kind {
                OriginalKind::Regular => validate_file_mode(Some(entry.mode), &entry.path)?,
                OriginalKind::Symlink if entry.mode == 0 => {}
                OriginalKind::Symlink => return Err(MutationError::CorruptJournalState),
            }
        } else if entry.mode != 0 || entry.original_kind != OriginalKind::Regular {
            return Err(MutationError::CorruptJournalState);
        }
        parsed_paths.push((entry.path.clone(), relative));
    }

    let mut created_dirs = HashSet::with_capacity(manifest.created_dirs.len());
    for directory in &manifest.created_dirs {
        let relative = parse_path(directory)?;
        if !created_dirs.insert(directory.as_str()) {
            return Err(MutationError::CorruptJournalState);
        }
        if !parsed_paths.iter().any(|(_, path)| {
            path.as_path() != relative.as_path() && path.as_path().starts_with(relative.as_path())
        }) {
            return Err(MutationError::CorruptJournalState);
        }
    }
    Ok(())
}

fn finish_cleanup(control: &Path, journal: &Path) -> Result<(), MutationError> {
    let cleanup = control.join(CLEANUP_DIR);
    fs::rename(journal, &cleanup).map_err(|source| MutationError::Io {
        operation: "retiring mutation journal",
        path: journal.to_path_buf(),
        source,
    })?;
    sync_directory(control)?;
    fs::remove_dir_all(&cleanup).map_err(|source| MutationError::Io {
        operation: "removing retired mutation journal",
        path: cleanup,
        source,
    })?;
    sync_directory(control)
}

fn remove_declared_target(target: &Path, display: &str) -> Result<(), MutationError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if !metadata.file_type().is_symlink() && !metadata.is_file() => {
            Err(MutationError::UnsafeTarget {
                path: display.to_owned(),
                reason: "deletion target is not a regular file or symbolic link",
            })
        }
        Ok(_) => {
            fs::remove_file(target).map_err(|source| MutationError::Io {
                operation: "deleting declared workspace file",
                path: target.to_path_buf(),
                source,
            })?;
            sync_directory(target.parent().unwrap_or(target))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MutationError::Io {
            operation: "reading deletion target metadata",
            path: target.to_path_buf(),
            source,
        }),
    }
}

fn remove_created_target(target: &Path, display: &str) -> Result<(), MutationError> {
    match fs::symlink_metadata(target) {
        Ok(metadata) if !metadata.file_type().is_symlink() && !metadata.is_file() => {
            Err(MutationError::UnsafeTarget {
                path: display.to_owned(),
                reason: "rollback target is not a regular file or symbolic link",
            })
        }
        Ok(_) => {
            fs::remove_file(target).map_err(|source| MutationError::Io {
                operation: "removing generated file during rollback",
                path: target.to_path_buf(),
                source,
            })?;
            sync_directory(target.parent().unwrap_or(target))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MutationError::Io {
            operation: "reading rollback target metadata",
            path: target.to_path_buf(),
            source,
        }),
    }
}

fn remove_temporary(path: &Path) -> Result<(), MutationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_symlink() && !metadata.is_file() => {
            Err(MutationError::UnsafeInternalPath {
                path: path.to_path_buf(),
            })
        }
        Ok(_) => {
            fs::remove_file(path).map_err(|source| MutationError::Io {
                operation: "removing generated temporary",
                path: path.to_path_buf(),
                source,
            })?;
            sync_directory(path.parent().unwrap_or(path))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MutationError::Io {
            operation: "reading generated temporary metadata",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn remove_created_directories(root: &Path, directories: &[String]) -> Result<(), MutationError> {
    let mut directories: Vec<_> = directories
        .iter()
        .map(|directory| Ok((directory, parse_path(directory)?)))
        .collect::<Result<_, MutationError>>()?;
    directories.sort_by_key(|(_, path)| std::cmp::Reverse(path.components().count()));
    for (directory, relative) in directories {
        let path = root.join(relative.as_path());
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(MutationError::UnsafeTarget {
                    path: directory.clone(),
                    reason: "created parent was replaced by a non-directory",
                });
            }
            Ok(_) => match fs::remove_dir(&path) {
                Ok(()) => sync_directory(path.parent().unwrap_or(root))?,
                Err(source)
                    if matches!(
                        source.kind(),
                        std::io::ErrorKind::DirectoryNotEmpty | std::io::ErrorKind::NotFound
                    ) => {}
                Err(source) => {
                    return Err(MutationError::Io {
                        operation: "removing transaction-created directory",
                        path,
                        source,
                    });
                }
            },
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(MutationError::Io {
                    operation: "reading transaction-created directory metadata",
                    path,
                    source,
                });
            }
        }
    }
    Ok(())
}

fn reject_symlink(path: &Path, _kind: &'static str) -> Result<(), MutationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(MutationError::UnsafeInternalPath {
                path: path.to_path_buf(),
            })
        }
        Ok(_) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(MutationError::Io {
            operation: "reading path metadata",
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn sync_directory(path: &Path) -> Result<(), MutationError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|source| MutationError::Io {
            operation: "syncing directory",
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(unix)]
fn set_open_file_mode(file: &File, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_open_file_mode(_file: &File, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o7777
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn set_file_mode(path: &Path, mode: u32) -> Result<(), MutationError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|source| {
        MutationError::Io {
            operation: "setting file permissions",
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn set_file_mode(_path: &Path, _mode: u32) -> Result<(), MutationError> {
    Ok(())
}

#[cfg(unix)]
fn set_directory_mode(path: &Path, mode: u32) -> Result<(), MutationError> {
    set_file_mode(path, mode)
}

#[cfg(not(unix))]
fn set_directory_mode(_path: &Path, _mode: u32) -> Result<(), MutationError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Child, Command, Stdio},
        sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
            mpsc,
        },
        time::Duration,
    };

    use super::*;

    static NEXT_REPOSITORY: AtomicU64 = AtomicU64::new(0);

    struct TestRepository {
        root: PathBuf,
    }

    impl TestRepository {
        fn new() -> Self {
            let id = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("theseus-mutation-{}-{id}", std::process::id()));
            fs::create_dir(&root).expect("test repository is created");
            Self { root }
        }

        fn path(&self, relative: &str) -> PathBuf {
            self.root.join(relative)
        }

        fn write(&self, relative: &str, contents: &[u8]) {
            let path = self.path(relative);
            fs::create_dir_all(path.parent().expect("test path has a parent"))
                .expect("test parent is created");
            fs::write(path, contents).expect("test file is written");
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn generated(path: &str, contents: &str) -> MutationFile {
        MutationFile::text(path, contents)
    }

    fn deleted(path: &str) -> MutationFile {
        MutationFile::absent(path)
    }

    fn expected(path: &str, contents: &str) -> ExpectedFile {
        ExpectedFile {
            path: path.to_owned(),
            contents: Some(contents.to_owned()),
        }
    }

    fn expected_absent(path: &str) -> ExpectedFile {
        ExpectedFile {
            path: path.to_owned(),
            contents: None,
        }
    }

    fn spawn_helper(repository: &TestRepository, mode: &str) -> Child {
        Command::new(std::env::current_exe().expect("the test executable has a path"))
            .args(["--exact", "tests::repository_process_helper", "--nocapture"])
            .env("THESEUS_MUTATION_HELPER", mode)
            .env("THESEUS_MUTATION_ROOT", &repository.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the mutation helper starts")
    }

    fn wait_for(path: &Path) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn repository_process_helper() {
        let Ok(mode) = std::env::var("THESEUS_MUTATION_HELPER") else {
            return;
        };
        let root = PathBuf::from(
            std::env::var_os("THESEUS_MUTATION_ROOT").expect("the helper receives a root"),
        );
        match mode.as_str() {
            "holder" => {
                let mutation =
                    FsMutation::begin(&root, &[]).expect("the holder acquires the lease");
                fs::write(root.join("holder-ready"), b"").expect("the holder signals readiness");
                wait_for(&root.join("holder-release"));
                mutation.commit().expect("the holder releases the lease");
            }
            "contender" => {
                let mutation =
                    FsMutation::begin(&root, &[]).expect("the contender acquires the lease");
                fs::write(root.join("contender-acquired"), b"")
                    .expect("the contender signals acquisition");
                mutation.commit().expect("the contender releases the lease");
            }
            "crash-prepared" => {
                let mut mutation =
                    MutationCore::begin(&root, &[]).expect("the writer acquires the lease");
                mutation
                    .apply_files(&[generated("model.rs", "interrupted")])
                    .expect("the writer applies its batch");
                fs::write(root.join("crash-ready"), b"").expect("the writer signals readiness");
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
            other => panic!("unknown mutation helper mode {other}"),
        }
    }

    #[tokio::test]
    async fn rollback_restores_exact_targets_and_preserves_unrelated_files() {
        let repository = TestRepository::new();
        repository.write("src/existing.rs", b"before\n");
        repository.write("notes.txt", b"untracked\n");
        repository.write(".git/index", b"index bytes\0\xff");
        fs::create_dir(repository.path("preexisting-empty"))
            .expect("pre-existing empty directory is created");
        #[cfg(unix)]
        set_file_mode(&repository.path("src/existing.rs"), 0o640).expect("test mode is set");

        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[
                generated("src/existing.rs", "after\n"),
                generated("generated/nested/new.rs", "new\n"),
                generated("preexisting-empty/new/target.rs", "new\n"),
            ])
            .await
            .expect("write set is applied");
        mutation.rollback().expect("write set is rolled back");

        assert_eq!(
            fs::read(repository.path("src/existing.rs")).unwrap(),
            b"before\n"
        );
        assert!(!repository.path("generated/nested/new.rs").exists());
        assert!(!repository.path("generated").exists());
        assert!(repository.path("preexisting-empty").is_dir());
        assert!(!repository.path("preexisting-empty/new").exists());
        assert_eq!(
            fs::read(repository.path("notes.txt")).unwrap(),
            b"untracked\n"
        );
        assert_eq!(
            fs::read(repository.path(".git/index")).unwrap(),
            b"index bytes\0\xff"
        );
        #[cfg(unix)]
        assert_eq!(
            file_mode(&fs::metadata(repository.path("src/existing.rs")).unwrap()),
            0o640
        );
        assert!(!repository.path(".theseus/mutation").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_late_apply_failure_restores_earlier_targets() {
        let repository = TestRepository::new();
        repository.write("first.rs", b"before");
        fs::create_dir(repository.path("read-only")).expect("read-only parent is created");
        set_directory_mode(&repository.path("read-only"), 0o555).expect("read-only mode is set");

        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        let result = mutation
            .apply(&[
                generated("first.rs", "after"),
                generated("read-only/second.rs", "new"),
            ])
            .await;
        set_directory_mode(&repository.path("read-only"), 0o755).expect("cleanup mode is restored");

        assert!(result.is_err());
        assert_eq!(fs::read(repository.path("first.rs")).unwrap(), b"before");
        assert!(!repository.path("read-only/second.rs").exists());
        assert!(!repository.path(".theseus/mutation").exists());
        mutation.commit().expect("the recovered lease is released");
    }

    #[test]
    fn a_poisoned_journal_cannot_be_retried_or_committed() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let files = [generated("model.rs", "partial")];
        let mut core = MutationCore::begin(&repository.root, &[]).expect("lease is acquired");
        core.prepare_journal(&files)
            .expect("the prepared journal is published");
        core.journal_active = true;
        fs::write(repository.path("model.rs"), b"partial").expect("a partial apply is simulated");

        assert!(matches!(
            core.apply_files(&files),
            Err(MutationError::PoisonedMutation)
        ));
        assert!(matches!(
            core.commit_inner(),
            Err(MutationError::PoisonedMutation)
        ));
        drop(core);
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"persisted");
        assert!(!repository.path(".theseus/mutation").exists());
    }

    #[cfg(unix)]
    #[test]
    fn oversized_targets_and_hardlinked_journals_fail_closed() {
        let repository = TestRepository::new();
        let oversized = File::create(repository.path("oversized.rs")).unwrap();
        oversized.set_len(MAX_BACKUP_BYTES + 1).unwrap();
        drop(oversized);
        let mut core = MutationCore::begin(&repository.root, &[]).expect("lease is acquired");
        assert!(matches!(
            core.prepare_journal(&[generated("oversized.rs", "replacement")]),
            Err(MutationError::PrivateFileTooLarge { .. })
        ));
        core.prepare_journal(&[generated("new.rs", "new")])
            .expect("a normal journal is prepared");
        core.journal_active = false;
        drop(core);

        fs::hard_link(
            repository.path(".theseus/mutation/manifest.json"),
            repository.path("manifest-hardlink"),
        )
        .expect("the manifest is hardlinked");
        assert!(matches!(
            FsMutation::begin(&repository.root, &[]),
            Err(MutationError::UnsafeInternalPath { .. })
        ));
    }

    #[tokio::test]
    async fn commit_keeps_the_complete_batch_and_cleans_the_journal() {
        let repository = TestRepository::new();
        repository.write("one.txt", b"one");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[
                generated("one.txt", "changed"),
                generated("two.txt", "created"),
            ])
            .await
            .expect("write set is applied");
        mutation.commit().expect("write set is committed");

        assert_eq!(fs::read(repository.path("one.txt")).unwrap(), b"changed");
        assert_eq!(fs::read(repository.path("two.txt")).unwrap(), b"created");
        assert!(!repository.path(".theseus/mutation").exists());
        assert!(!repository.path(".theseus/mutation.cleanup").exists());
    }

    #[tokio::test]
    async fn deletion_tombstones_commit_and_roll_back_without_creating_parents() {
        let repository = TestRepository::new();
        repository.write("obsolete.rs", b"owned");

        let mut rolled_back = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        rolled_back
            .apply(&[deleted("obsolete.rs")])
            .await
            .expect("deletion is applied");
        assert!(!repository.path("obsolete.rs").exists());
        rolled_back.rollback().expect("deletion is rolled back");
        assert_eq!(fs::read(repository.path("obsolete.rs")).unwrap(), b"owned");

        let mut committed = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        committed
            .apply(&[deleted("obsolete.rs"), deleted("missing/tree/file.rs")])
            .await
            .expect("deletions are applied");
        committed.commit().expect("deletions are committed");
        assert!(!repository.path("obsolete.rs").exists());
        assert!(!repository.path("missing").exists());
    }

    #[tokio::test]
    async fn dropping_an_uncommitted_mutation_rolls_it_back_synchronously() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[generated("model.rs", "working")])
            .await
            .expect("write set is applied");
        drop(mutation);

        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"persisted");
        assert!(!repository.path(".theseus/mutation").exists());
    }

    #[test]
    fn cancelling_an_in_flight_apply_waits_for_its_rollback() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut core = MutationCore::begin(&repository.root, &[]).expect("lease is acquired");
        let started = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_started = Arc::clone(&started);
        let worker_release = Arc::clone(&release);
        let worker = ThreadWorker::spawn(move || {
            worker_started.wait();
            worker_release.wait();
            let result = core.apply_files(&[generated("model.rs", "working")]);
            (core, result)
        });
        started.wait();

        let (dropped_tx, dropped_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(worker);
            dropped_tx.send(()).expect("drop completion is reported");
        });
        assert!(
            dropped_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "cancellation returned while the filesystem worker was still active"
        );

        release.wait();
        dropped_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("cancellation completes after rollback");
        dropper.join().expect("dropper exits");
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"persisted");
        assert!(!repository.path(".theseus/mutation").exists());
    }

    #[tokio::test]
    async fn interrupted_prepared_journal_is_recovered_before_stale_check() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut mutation = MutationCore::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply_files(&[generated("model.rs", "interrupted")])
            .expect("write set is applied");
        mutation.journal_active = false;
        drop(mutation);

        let recovered = FsMutation::begin(&repository.root, &[expected("model.rs", "persisted")])
            .expect("recovery restores the persisted projection");
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"persisted");
        assert!(!repository.path(".theseus/mutation").exists());
        recovered.commit().expect("recovered lease is released");
    }

    #[tokio::test]
    async fn recovery_handles_a_new_target_before_an_existing_backup() {
        let repository = TestRepository::new();
        repository.write("existing.rs", b"persisted");
        let mut mutation = MutationCore::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply_files(&[
                generated("new.rs", "new"),
                generated("existing.rs", "interrupted"),
            ])
            .expect("write set is applied");
        mutation.journal_active = false;
        drop(mutation);

        let recovered =
            FsMutation::begin(&repository.root, &[expected("existing.rs", "persisted")])
                .expect("mixed journal is recovered");
        assert!(!repository.path("new.rs").exists());
        assert_eq!(
            fs::read(repository.path("existing.rs")).unwrap(),
            b"persisted"
        );
        recovered.commit().expect("recovered lease is released");
    }

    #[tokio::test]
    async fn interrupted_committed_journal_is_cleaned_without_rollback() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut mutation = MutationCore::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply_files(&[generated("model.rs", "committed")])
            .expect("write set is applied");
        write_state(&mutation.journal_path(), COMMITTED).expect("commit marker is durable");
        mutation.journal_active = false;
        drop(mutation);

        let recovered = FsMutation::begin(&repository.root, &[expected("model.rs", "committed")])
            .expect("committed cleanup is recovered");
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"committed");
        assert!(!repository.path(".theseus/mutation").exists());
        recovered.commit().expect("recovered lease is released");
    }

    #[tokio::test]
    async fn durable_commit_succeeds_when_cleanup_must_be_recovered() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[generated("model.rs", "committed")])
            .await
            .expect("write set is applied");
        repository.write(".theseus/mutation.cleanup/blocker", b"block cleanup rename");

        mutation
            .commit()
            .expect("durable marker makes commit successful");
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"committed");
        assert!(repository.path(".theseus/mutation").exists());

        let recovered = FsMutation::begin(&repository.root, &[expected("model.rs", "committed")])
            .expect("next lease finishes committed cleanup");
        assert!(!repository.path(".theseus/mutation").exists());
        assert!(!repository.path(".theseus/mutation.cleanup").exists());
        recovered.commit().expect("recovered lease is released");
    }

    #[tokio::test]
    async fn a_committed_journal_recovers_past_a_file_cleanup_blocker() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[generated("model.rs", "committed")])
            .await
            .expect("write set is applied");
        repository.write(".theseus/mutation.cleanup", b"block cleanup rename");
        mutation.commit().expect("the durable commit succeeds");

        let recovered = FsMutation::begin(&repository.root, &[expected("model.rs", "committed")])
            .expect("the file blocker is safely removed");
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"committed");
        assert!(!repository.path(".theseus/mutation").exists());
        assert!(!repository.path(".theseus/mutation.cleanup").exists());
        recovered.commit().expect("recovered lease is released");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_committed_journal_recovers_past_a_symlink_cleanup_blocker() {
        use std::os::unix::fs::symlink;

        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        repository.write("sentinel", b"outside");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[generated("model.rs", "committed")])
            .await
            .expect("write set is applied");
        symlink(
            repository.path("sentinel"),
            repository.path(".theseus/mutation.cleanup"),
        )
        .expect("the cleanup blocker is linked");
        mutation.commit().expect("the durable commit succeeds");

        let recovered = FsMutation::begin(&repository.root, &[expected("model.rs", "committed")])
            .expect("the symlink blocker is safely unlinked");
        assert_eq!(fs::read(repository.path("sentinel")).unwrap(), b"outside");
        assert!(!repository.path(".theseus/mutation").exists());
        assert!(!repository.path(".theseus/mutation.cleanup").exists());
        recovered.commit().expect("recovered lease is released");
    }

    #[test]
    fn stale_and_duplicate_expected_projections_are_rejected() {
        let repository = TestRepository::new();
        repository.write("generated.rs", b"disk");
        let stale = FsMutation::begin(&repository.root, &[expected("generated.rs", "expected")]);
        assert!(matches!(stale, Err(MutationError::StaleWorkspace { .. })));

        repository.write("unexpected.rs", b"manual");
        let unexpected = FsMutation::begin(&repository.root, &[expected_absent("unexpected.rs")]);
        assert!(matches!(
            unexpected,
            Err(MutationError::StaleWorkspace { .. })
        ));

        let duplicate = FsMutation::begin(
            &repository.root,
            &[
                expected("generated.rs", "disk"),
                expected("generated.rs", "disk"),
            ],
        );
        assert!(matches!(
            duplicate,
            Err(MutationError::DuplicatePath { .. })
        ));

        let overlapping = FsMutation::begin(
            &repository.root,
            &[
                expected("generated.rs", "disk"),
                expected_absent("generated.rs/child"),
            ],
        );
        assert!(matches!(
            overlapping,
            Err(MutationError::OverlappingPaths { .. })
        ));
    }

    #[tokio::test]
    async fn planning_reads_and_exists_checks_share_the_lease() {
        let repository = TestRepository::new();
        repository.write("present.txt", b"contents");
        let mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");

        assert!(mutation.exists("present.txt").await.unwrap());
        assert!(!mutation.exists("absent.txt").await.unwrap());
        assert_eq!(
            mutation.read_to_string("present.txt").await.unwrap(),
            "contents"
        );
        mutation.commit().expect("lease is released");
    }

    #[test]
    fn repository_lease_serializes_independent_callers() {
        let repository = Arc::new(TestRepository::new());
        let first = FsMutation::begin(&repository.root, &[]).expect("first lease is acquired");
        let (started_tx, started_rx) = mpsc::channel();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let contender_root = Arc::clone(&repository);
        let contender = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let second =
                FsMutation::begin(&contender_root.root, &[]).expect("second lease is acquired");
            acquired_tx.send(()).unwrap();
            second.commit().expect("second lease is released");
        });

        started_rx.recv().unwrap();
        assert!(
            acquired_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        first.commit().expect("first lease is released");
        acquired_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second caller acquires after release");
        contender.join().unwrap();
    }

    #[test]
    fn repository_lease_serializes_independent_processes() {
        let repository = TestRepository::new();
        let mut holder = spawn_helper(&repository, "holder");
        wait_for(&repository.path("holder-ready"));
        let mut contender = spawn_helper(&repository, "contender");

        std::thread::sleep(Duration::from_millis(150));
        assert!(!repository.path("contender-acquired").exists());
        fs::write(repository.path("holder-release"), b"").expect("the holder is released");
        assert!(holder.wait().expect("the holder exits").success());
        assert!(contender.wait().expect("the contender exits").success());
        assert!(repository.path("contender-acquired").exists());
    }

    #[test]
    fn the_next_process_recovers_a_killed_prepared_writer() {
        let repository = TestRepository::new();
        repository.write("model.rs", b"persisted");
        let mut writer = spawn_helper(&repository, "crash-prepared");
        wait_for(&repository.path("crash-ready"));
        writer.kill().expect("the interrupted writer is killed");
        writer.wait().expect("the killed writer is reaped");

        let recovered = FsMutation::begin(&repository.root, &[expected("model.rs", "persisted")])
            .expect("the next process recovers before checking staleness");
        assert_eq!(fs::read(repository.path("model.rs")).unwrap(), b"persisted");
        assert!(!repository.path(".theseus/mutation").exists());
        recovered.commit().expect("the recovery lease is released");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn leaf_symlinks_are_replaced_without_following_them() {
        use std::os::unix::fs::symlink;

        let repository = TestRepository::new();
        repository.write("outside.txt", b"outside");
        symlink(repository.path("outside.txt"), repository.path("link.txt"))
            .expect("test symlink is created");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[generated("link.txt", "changed")])
            .await
            .expect("an atomic replacement does not follow the link");
        assert_eq!(fs::read(repository.path("link.txt")).unwrap(), b"changed");
        assert_eq!(
            fs::read(repository.path("outside.txt")).unwrap(),
            b"outside"
        );
        mutation.rollback().expect("the original link is restored");
        assert_eq!(
            fs::read_link(repository.path("link.txt")).unwrap(),
            repository.path("outside.txt")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn parent_symlinks_and_reserved_control_paths_are_rejected() {
        use std::os::unix::fs::symlink;

        let repository = TestRepository::new();
        fs::create_dir(repository.path("outside")).expect("outside directory is created");
        symlink(repository.path("outside"), repository.path("linked-parent"))
            .expect("test symlink is created");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        assert!(matches!(
            mutation
                .apply(&[generated("linked-parent/file", "changed")])
                .await,
            Err(MutationError::UnsafeTarget { .. })
        ));
        assert!(matches!(
            mutation
                .apply(&[generated(".theseus/state", "changed")])
                .await,
            Err(MutationError::UnsafeTarget { .. })
        ));
        assert!(matches!(
            mutation
                .apply(&[generated(".THESEUS/state", "changed")])
                .await,
            Err(MutationError::UnsafeTarget { .. })
        ));
        mutation.commit().expect("lease is released");
        assert!(!repository.path("outside/file").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exact_binary_modes_and_symlinks_commit_as_one_batch() {
        let repository = TestRepository::new();
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[
                MutationFile::regular("bin/tool", b"\0binary\xff".to_vec(), Some(0o755)),
                MutationFile::symlink("tool-link", b"bin/tool".to_vec()),
            ])
            .await
            .expect("the exact entries are published");
        mutation.commit().expect("the batch commits");

        assert_eq!(
            fs::read(repository.path("bin/tool")).unwrap(),
            b"\0binary\xff"
        );
        assert_eq!(
            file_mode(&fs::metadata(repository.path("bin/tool")).unwrap()),
            0o755
        );
        assert_eq!(
            fs::read_link(repository.path("tool-link")).unwrap(),
            Path::new("bin/tool")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rollback_restores_regular_files_and_symlinks_exactly() {
        use std::os::unix::fs::symlink;

        let repository = TestRepository::new();
        repository.write("regular", b"before\0");
        set_file_mode(&repository.path("regular"), 0o600).unwrap();
        symlink("regular", repository.path("link")).expect("the original link is created");
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[
                MutationFile::symlink("regular", b"other".to_vec()),
                MutationFile::regular("link", b"replacement".to_vec(), Some(0o755)),
            ])
            .await
            .expect("the entry kinds are exchanged");
        mutation.rollback().expect("the batch rolls back");

        assert_eq!(fs::read(repository.path("regular")).unwrap(), b"before\0");
        assert_eq!(
            file_mode(&fs::metadata(repository.path("regular")).unwrap()),
            0o600
        );
        assert_eq!(
            fs::read_link(repository.path("link")).unwrap(),
            Path::new("regular")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn journals_are_private_while_the_mutation_is_pending() {
        let repository = TestRepository::new();
        let mut mutation = FsMutation::begin(&repository.root, &[]).expect("lease is acquired");
        mutation
            .apply(&[generated("new.rs", "contents")])
            .await
            .expect("write set is applied");

        assert_eq!(
            file_mode(&fs::metadata(repository.path(".theseus")).unwrap()),
            0o700
        );
        assert_eq!(
            file_mode(&fs::metadata(repository.path(".theseus/repository.lock")).unwrap()),
            0o600
        );
        assert_eq!(
            file_mode(&fs::metadata(repository.path(".theseus/mutation")).unwrap()),
            0o700
        );
        for entry in fs::read_dir(repository.path(".theseus/mutation")).unwrap() {
            let metadata = entry.unwrap().metadata().unwrap();
            assert_eq!(file_mode(&metadata), 0o600);
        }
        mutation.rollback().expect("write set is rolled back");
    }
}
