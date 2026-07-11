//! Transactional creation of a minimal modeled Rust project.

use std::{
    collections::HashSet,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};

use async_trait::async_trait;
use theseus_modeling::{
    Model, ModelRecord, Port, ProjectId, ProjectLayoutError, RustWorkspaceLayout, Service,
    Transport, scaffold_files,
};
use theseus_workspace::inspect_creation_recovery_with_control_directories;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::{
    CargoToolchain, CheckReport, FsMutation, MutationError, MutationFile, PROJECT_MANIFEST_PATH,
    PendingMutation, ProjectContext, ProjectContextError, ProjectManifest, ProjectOpenError,
    Toolchain,
};

const MODEL_RECORD_PATH: &str = "model.json";
const ROOT_MANIFEST_PATH: &str = "Cargo.toml";
const LOCKFILE_PATH: &str = "Cargo.lock";
const MODELING_MANIFEST_PATH: &str = "Cargo.toml";
const MAX_MODELING_PATH_BYTES: usize = 4_096;
const MAX_MODELING_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 16 * 1024;
const MAX_ROOT_SCAN_ENTRIES: usize = 256;
const INITIALIZE_TARGET_DIRECTORY: &str = "initialize-target";
const INITIALIZE_TARGET: &str = ".theseus/initialize-target";

/// Create a minimal project in an existing empty directory.
pub async fn initialize_project(
    root: impl AsRef<Path>,
    project_id: ProjectId,
    modeling_path: impl AsRef<Path>,
) -> Result<ProjectContext, ProjectInitError> {
    let supplied_root = root.as_ref().to_path_buf();
    let supplied_modeling = modeling_path.as_ref().to_path_buf();
    let (root, modeling_path) = tokio::task::spawn_blocking(move || {
        Ok::<_, ProjectInitError>((
            canonical_root(&supplied_root)?,
            canonical_modeling_path(&supplied_modeling)?,
        ))
    })
    .await
    .map_err(|source| ProjectInitError::BlockingTask { source })??;

    let model = initial_model(project_id.as_str());
    let layout = RustWorkspaceLayout::new(project_id, ModelRecord::json(MODEL_RECORD_PATH)?);
    let provisional = ProjectContext::new(&root, model.clone(), layout.clone())?;
    let changes = initial_changes(&model, &layout, &modeling_path)?;
    let mut mutation = begin_initialization_mutation(&root, &changes).await?;
    let target = match BuildTargetGuard::prepare(&root) {
        Ok(target) => target,
        Err(error) => return rollback_with(mutation, error),
    };

    if let Err(source) = mutation.apply(&changes).await {
        return rollback_with(mutation, ProjectInitError::Mutation(source));
    }
    initialize_after_apply(mutation, provisional, target, &CargoInitChecker).await
}

async fn begin_initialization_mutation(
    root: &Path,
    changes: &[MutationFile],
) -> Result<PendingMutation, ProjectInitError> {
    let inspected_root = root.to_path_buf();
    let target_paths: Vec<String> = changes.iter().map(|change| change.path.clone()).collect();
    let allowed_control_directories = vec![INITIALIZE_TARGET_DIRECTORY.to_owned()];
    let recovery = tokio::task::spawn_blocking(move || {
        inspect_creation_recovery_with_control_directories(
            &inspected_root,
            &target_paths,
            &allowed_control_directories,
        )
    })
    .await
    .map_err(|source| ProjectInitError::BlockingTask { source })??;

    if !recovery.interrupted() {
        let unexpected = inspect_root_entries(root).await?;
        if !unexpected.is_empty() {
            return Err(ProjectInitError::NonEmptyRoot {
                root: root.to_path_buf(),
                entries: unexpected,
            });
        }
    }
    validate_git_root(root).await?;

    let mutation =
        FsMutation::begin_async_recovering_creation(root.to_path_buf(), Vec::new(), recovery)
            .await?;
    let unexpected = inspect_root_entries(root).await?;
    if !unexpected.is_empty() {
        return rollback_with(
            mutation,
            ProjectInitError::NonEmptyRoot {
                root: root.to_path_buf(),
                entries: unexpected,
            },
        );
    }
    if let Err(error) = validate_git_root(root).await {
        return rollback_with(mutation, error);
    }
    Ok(mutation)
}

async fn inspect_root_entries(root: &Path) -> Result<Vec<PathBuf>, ProjectInitError> {
    let inspected_root = root.to_path_buf();
    tokio::task::spawn_blocking(move || unexpected_root_entries(&inspected_root))
        .await
        .map_err(|source| ProjectInitError::BlockingTask { source })?
}

async fn initialize_after_apply(
    mutation: PendingMutation,
    provisional: ProjectContext,
    mut target: BuildTargetGuard,
    checker: &dyn InitChecker,
) -> Result<ProjectContext, ProjectInitError> {
    let report = match checker.check(&provisional).await {
        Ok(report) => report,
        Err(source) => {
            return rollback_after_check(
                mutation,
                &mut target,
                ProjectInitError::CheckCommand {
                    source: source.into_boxed_dyn_error(),
                },
            );
        }
    };
    if !report.ok {
        return rollback_after_check(
            mutation,
            &mut target,
            ProjectInitError::CheckFailed { report },
        );
    }
    if let Err(source) = target.cleanup() {
        return rollback_with(
            mutation,
            ProjectInitError::BuildCleanup {
                path: target.path.clone(),
                source,
            },
        );
    }

    // Opening and committing are deliberately synchronous and adjacent. Once
    // the durable project is accepted, cancellation cannot split adoption from
    // the WAL commit point.
    let opened = match ProjectContext::open(provisional.root()) {
        Ok(opened) => opened,
        Err(source) => {
            return rollback_after_check(mutation, &mut target, ProjectInitError::Open { source });
        }
    };
    mutation.commit()?;
    Ok(opened)
}

fn rollback_after_check<T>(
    mutation: PendingMutation,
    target: &mut BuildTargetGuard,
    primary: ProjectInitError,
) -> Result<T, ProjectInitError> {
    let primary = match target.cleanup() {
        Ok(()) => primary,
        Err(source) => ProjectInitError::FailureCleanup {
            primary: Box::new(primary),
            path: target.path.clone(),
            source,
        },
    };
    rollback_with(mutation, primary)
}

#[async_trait]
trait InitChecker: Send + Sync {
    async fn check(&self, project: &ProjectContext) -> anyhow::Result<CheckReport>;
}

struct CargoInitChecker;

#[async_trait]
impl InitChecker for CargoInitChecker {
    async fn check(&self, project: &ProjectContext) -> anyhow::Result<CheckReport> {
        let toolchain = CargoToolchain::for_project(project);
        let bound = toolchain.context().await?;
        let target = bound.root().join(INITIALIZE_TARGET);
        let output = tokio::process::Command::new("cargo")
            .args(["check", "--workspace", "--all-targets", "--quiet"])
            .arg("--target-dir")
            .arg(&target)
            .env_remove("CARGO_TARGET_DIR")
            .current_dir(bound.root())
            .kill_on_drop(true)
            .output()
            .await?;
        Ok(crate::report_from_output(
            &output,
            "every workspace target compiles",
            "every workspace target compiles, with warnings",
            "all-target check failed",
        ))
    }
}

fn rollback_with<T>(
    mutation: PendingMutation,
    primary: ProjectInitError,
) -> Result<T, ProjectInitError> {
    match mutation.rollback() {
        Ok(()) => Err(primary),
        Err(rollback) => Err(ProjectInitError::Rollback {
            primary: Box::new(primary),
            rollback,
        }),
    }
}

fn canonical_root(root: &Path) -> Result<PathBuf, ProjectInitError> {
    let canonical = root
        .canonicalize()
        .map_err(|source| ProjectInitError::Root {
            path: root.to_path_buf(),
            source,
        })?;
    if !canonical.is_dir() {
        return Err(ProjectInitError::RootNotDirectory { path: canonical });
    }
    Ok(canonical)
}

fn canonical_modeling_path(path: &Path) -> Result<PathBuf, ProjectInitError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| ProjectInitError::ModelingPath {
            path: path.to_path_buf(),
            source,
        })?;
    if !canonical.is_dir() {
        return Err(ProjectInitError::ModelingNotDirectory { path: canonical });
    }
    let directory_before =
        fs::metadata(&canonical).map_err(|source| ProjectInitError::ModelingPath {
            path: canonical.clone(),
            source,
        })?;
    let encoded_len = canonical
        .to_str()
        .ok_or_else(|| ProjectInitError::NonUtf8ModelingPath {
            path: canonical.clone(),
        })?
        .len();
    if encoded_len > MAX_MODELING_PATH_BYTES {
        return Err(ProjectInitError::ModelingPathTooLong {
            path: canonical,
            length: encoded_len,
            maximum: MAX_MODELING_PATH_BYTES,
        });
    }

    let manifest_path = canonical.join(MODELING_MANIFEST_PATH);
    read_modeling_manifest(&manifest_path)?;
    let rebound = path
        .canonicalize()
        .map_err(|source| ProjectInitError::ModelingPath {
            path: path.to_path_buf(),
            source,
        })?;
    let directory_after =
        fs::metadata(&canonical).map_err(|source| ProjectInitError::ModelingPath {
            path: canonical.clone(),
            source,
        })?;
    if rebound != canonical || !same_directory(&directory_before, &directory_after) {
        return Err(ProjectInitError::ModelingPathChanged {
            expected: canonical,
            actual: rebound,
        });
    }
    Ok(canonical)
}

fn read_modeling_manifest(path: &Path) -> Result<Vec<u8>, ProjectInitError> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(path)
        .map_err(|source| ProjectInitError::ModelingManifest {
            path: path.to_path_buf(),
            source,
        })?;
    let before = file
        .metadata()
        .map_err(|source| ProjectInitError::ModelingManifest {
            path: path.to_path_buf(),
            source,
        })?;
    if !before.is_file() {
        return Err(ProjectInitError::InvalidModelingManifest {
            path: path.to_path_buf(),
            reason: "manifest must be a regular file",
        });
    }
    if hard_link_count(&before) > 1 {
        return Err(ProjectInitError::HardlinkedModelingManifest {
            path: path.to_path_buf(),
            links: hard_link_count(&before),
        });
    }
    if before.len() > MAX_MODELING_MANIFEST_BYTES {
        return Err(ProjectInitError::ModelingManifestTooLarge {
            path: path.to_path_buf(),
            length: before.len(),
            maximum: MAX_MODELING_MANIFEST_BYTES,
        });
    }
    let mut contents = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_MODELING_MANIFEST_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|source| ProjectInitError::ModelingManifest {
            path: path.to_path_buf(),
            source,
        })?;
    if contents.len() as u64 > MAX_MODELING_MANIFEST_BYTES {
        return Err(ProjectInitError::ModelingManifestTooLarge {
            path: path.to_path_buf(),
            length: contents.len() as u64,
            maximum: MAX_MODELING_MANIFEST_BYTES,
        });
    }
    let after = file
        .metadata()
        .map_err(|source| ProjectInitError::ModelingManifest {
            path: path.to_path_buf(),
            source,
        })?;
    if contents.len() as u64 != before.len() || !same_file(&before, &after) {
        return Err(ProjectInitError::ModelingManifestChanged {
            path: path.to_path_buf(),
        });
    }
    Ok(contents)
}

#[cfg(unix)]
fn hard_link_count(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink()
}

#[cfg(not(unix))]
fn hard_link_count(_metadata: &fs::Metadata) -> u64 {
    1
}

#[cfg(unix)]
fn same_directory(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev() && before.ino() == after.ino()
}

#[cfg(not(unix))]
fn same_directory(_before: &fs::Metadata, _after: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.len() == after.len()
        && before.mode() == after.mode()
        && before.nlink() == after.nlink()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(not(unix))]
fn same_file(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len()
}

async fn validate_git_root(root: &Path) -> Result<(), ProjectInitError> {
    let mut command = tokio::process::Command::new("git");
    crate::checkpoint::configure_tokio_git(&mut command);
    let child = command
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| ProjectInitError::GitCommand { source })?;
    let output = bounded_git_output(child, MAX_GIT_OUTPUT_BYTES).await?;
    if !output.status.success() {
        return Err(ProjectInitError::NotGitRepository {
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    let reported = String::from_utf8(output.stdout)
        .map_err(|source| ProjectInitError::InvalidGitOutput { source })?;
    let reported = PathBuf::from(reported.trim_end_matches(['\r', '\n']));
    let reported = reported
        .canonicalize()
        .map_err(|source| ProjectInitError::GitRoot {
            path: reported,
            source,
        })?;
    if reported != root {
        return Err(ProjectInitError::GitRootMismatch {
            expected: root.to_path_buf(),
            actual: reported,
        });
    }
    Ok(())
}

struct ManagedInitChild {
    child: tokio::process::Child,
    reaped: bool,
}

impl ManagedInitChild {
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
}

impl Drop for ManagedInitChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        let _ = self.child.start_kill();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(1)),
            }
        }
    }
}

async fn bounded_git_output(
    child: tokio::process::Child,
    maximum: usize,
) -> Result<Output, ProjectInitError> {
    let mut child = ManagedInitChild::new(child);
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or_else(|| ProjectInitError::GitCommand {
            source: std::io::Error::other("Git command has no standard output pipe"),
        })?;
    let stderr = child
        .child
        .stderr
        .take()
        .ok_or_else(|| ProjectInitError::GitCommand {
            source: std::io::Error::other("Git command has no standard error pipe"),
        })?;
    let outcome = {
        let wait = async {
            child
                .child
                .wait()
                .await
                .map_err(|source| ProjectInitError::GitCommand { source })
        };
        tokio::try_join!(
            read_limited_git_stream(stdout, maximum),
            read_limited_git_stream(stderr, maximum),
            wait
        )
    };
    match outcome {
        Ok((stdout, stderr, status)) => {
            child.reaped = true;
            let length = stdout.len().saturating_add(stderr.len());
            if length > maximum {
                return Err(ProjectInitError::GitOutputTooLarge { length, maximum });
            }
            Ok(Output {
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
    maximum: usize,
) -> Result<Vec<u8>, ProjectInitError> {
    let mut bytes = Vec::with_capacity(maximum.min(4_096));
    stream
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|source| ProjectInitError::GitCommand { source })?;
    if bytes.len() > maximum {
        return Err(ProjectInitError::GitOutputTooLarge {
            length: bytes.len(),
            maximum,
        });
    }
    Ok(bytes)
}

struct BuildTargetGuard {
    path: PathBuf,
    control: PathBuf,
    armed: bool,
}

impl BuildTargetGuard {
    fn prepare(root: &Path) -> Result<Self, ProjectInitError> {
        let mut guard = Self {
            path: root.join(INITIALIZE_TARGET),
            control: root.join(".theseus"),
            armed: true,
        };
        guard
            .cleanup()
            .map_err(|source| ProjectInitError::BuildCleanup {
                path: guard.path.clone(),
                source,
            })?;
        guard.armed = true;
        Ok(guard)
    }

    fn cleanup(&mut self) -> Result<(), std::io::Error> {
        match fs::symlink_metadata(&self.path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                fs::remove_dir_all(&self.path)?;
                File::open(&self.control)?.sync_all()?;
            }
            Ok(_) => {
                return Err(std::io::Error::other(
                    "initialization target is not a real directory",
                ));
            }
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(source),
        }
        self.armed = false;
        Ok(())
    }
}

impl Drop for BuildTargetGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.cleanup();
        }
    }
}

fn initial_model(name: &str) -> Model {
    Model::new(name)
        .crate_node("app", "app", 0, &[])
        .crate_node("app-cli", "app-cli", 1, &["app"])
        .service(
            Service::new("App")
                .crate_name("app")
                .port(Port::new("store", "Persists application state.")),
        )
        .inbound("app-cli", Transport::Cli, "App", "app-cli")
}

fn initial_changes(
    model: &Model,
    layout: &RustWorkspaceLayout,
    modeling_path: &Path,
) -> Result<Vec<MutationFile>, ProjectInitError> {
    let manifest = ProjectManifest::new(layout.clone());
    let mut encoded_manifest = serde_json::to_string_pretty(&manifest)
        .map_err(|source| ProjectInitError::SerializeManifest { source })?;
    encoded_manifest.push('\n');

    let mut changes = vec![
        MutationFile::text(ROOT_MANIFEST_PATH, workspace_manifest(modeling_path)?),
        MutationFile::text(PROJECT_MANIFEST_PATH, encoded_manifest),
    ];
    let mut scaffold = scaffold_files(model);
    for file in &mut scaffold {
        match file.path.as_str() {
            "rust/app/Cargo.toml" => {
                file.contents
                    .push_str("serde_json = { workspace = true }\n");
            }
            "rust/app/src/lib.rs" => file.contents.push_str(initial_app_authored_source()),
            "rust/app-cli/src/main.rs" => {
                file.contents = initial_cli_main_source().to_string();
            }
            _ => {}
        }
    }
    changes.extend(
        scaffold
            .into_iter()
            .map(|file| MutationFile::text(file.path, file.contents)),
    );
    changes.extend(
        layout
            .generated_files(model)?
            .into_iter()
            .map(|file| MutationFile::text(file.path, file.contents)),
    );
    changes.push(MutationFile::absent(LOCKFILE_PATH));
    reject_duplicate_targets(&changes)?;
    Ok(changes)
}

fn initial_app_authored_source() -> &'static str {
    "\n/// The initial in-memory adapter for the application's store port.\n\
     pub struct MemoryStore;\n\n\
     #[async_trait::async_trait]\n\
     impl Store for MemoryStore {}\n\n\
     /// Load the current canonical model record from this workspace.\n\
     pub fn load_model() -> anyhow::Result<theseus_modeling::Model> {\n\
         let root = std::path::Path::new(env!(\"CARGO_MANIFEST_DIR\"))\n\
             .parent()\n\
             .and_then(std::path::Path::parent)\n\
             .ok_or_else(|| std::io::Error::other(\"app crate is not below rust/\"))?;\n\
         let source = std::fs::read_to_string(root.join(\"model.json\"))?;\n\
         Ok(serde_json::from_str(&source)?)\n\
     }\n"
}

fn initial_cli_main_source() -> &'static str {
    "//! A standalone command-line interface to the App service.\n\n\
     mod generated;\n\n\
     #[tokio::main(flavor = \"current_thread\")]\n\
     async fn main() -> anyhow::Result<()> {\n\
         let app = app::Standalone {\n\
             model: app::load_model()?,\n\
             store: app::MemoryStore,\n\
         };\n\
         let matches = generated::command().get_matches();\n\
         generated::dispatch(&app, generated::Invocation::from_matches(&matches)?).await\n\
     }\n"
}

fn reject_duplicate_targets(changes: &[MutationFile]) -> Result<(), ProjectInitError> {
    let mut paths = HashSet::with_capacity(changes.len());
    for change in changes {
        let folded = change.path.to_ascii_lowercase();
        if !paths.insert(folded) {
            return Err(ProjectInitError::DuplicateTarget {
                path: change.path.clone(),
            });
        }
    }
    Ok(())
}

fn workspace_manifest(modeling_path: &Path) -> Result<String, ProjectInitError> {
    let path = modeling_path
        .to_str()
        .ok_or_else(|| ProjectInitError::NonUtf8ModelingPath {
            path: modeling_path.to_path_buf(),
        })?;
    let path = toml_basic_string(path);
    Ok(format!(
        "[workspace]\nresolver = \"2\"\nmembers = [\"rust/*\"]\n\n\
         [workspace.dependencies]\ntheseus-modeling = {{ path = \"{path}\" }}\n\n\
         anyhow = \"1.0.103\"\nasync-trait = \"0.1\"\n\
         clap = {{ version = \"4.6.1\", features = [\"string\"] }}\n\
         serde = {{ version = \"1.0.228\", features = [\"derive\"] }}\n\
         serde_json = \"1.0.150\"\n\
         tokio = {{ version = \"1.52\", features = [\"fs\", \"io-util\", \"macros\", \"process\", \"rt\", \"sync\"] }}\n\n\
         reqwest = {{ version = \"0.13\", default-features = false, features = [\"rustls\", \"json\"] }}\n\
         axum = \"0.8\"\n\
         prost = \"0.14\"\n\
         protox = \"0.9\"\n\
         protox-parse = \"0.9\"\n\
         tonic = \"0.14\"\n\
         tonic-prost = \"0.14\"\n\
         tonic-prost-build = \"0.14\"\n\
         tokio-stream = \"0.1\"\n\
         rmcp = {{ version = \"2.0\", features = [\"server\", \"transport-io\"] }}\n"
    ))
}

fn toml_basic_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\u{0008}' => escaped.push_str("\\b"),
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\u{000c}' => escaped.push_str("\\f"),
            '\r' => escaped.push_str("\\r"),
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            character if character.is_control() && u32::from(character) <= 0xffff => {
                escaped.push_str(&format!("\\u{:04X}", u32::from(character)));
            }
            character if character.is_control() => {
                escaped.push_str(&format!("\\U{:08X}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn unexpected_root_entries(root: &Path) -> Result<Vec<PathBuf>, ProjectInitError> {
    let mut unexpected = Vec::new();
    let mut scanned = 0usize;
    let entries = fs::read_dir(root).map_err(|source| ProjectInitError::InspectRoot {
        path: root.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ProjectInitError::InspectRoot {
            path: root.to_path_buf(),
            source,
        })?;
        count_root_entry(&mut scanned, root)?;
        match entry.file_name().to_str() {
            Some(".git") => {}
            Some(".theseus") => {
                inspect_control_directory(root, &mut unexpected, &mut scanned)?;
            }
            _ => unexpected.push(PathBuf::from(entry.file_name())),
        }
    }
    unexpected.sort();
    Ok(unexpected)
}

fn inspect_control_directory(
    root: &Path,
    unexpected: &mut Vec<PathBuf>,
    scanned: &mut usize,
) -> Result<(), ProjectInitError> {
    let control = root.join(".theseus");
    let metadata =
        fs::symlink_metadata(&control).map_err(|source| ProjectInitError::InspectRoot {
            path: control.clone(),
            source,
        })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProjectInitError::ControlPathBlocker { path: control });
    }
    let entries = fs::read_dir(&control).map_err(|source| ProjectInitError::InspectRoot {
        path: control.clone(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| ProjectInitError::InspectRoot {
            path: control.clone(),
            source,
        })?;
        count_root_entry(scanned, &control)?;
        let name = entry.file_name();
        let path = control.join(&name);
        match name.to_str() {
            Some("repository.lock") => {
                let metadata = fs::symlink_metadata(&path).map_err(|source| {
                    ProjectInitError::InspectRoot {
                        path: path.clone(),
                        source,
                    }
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ProjectInitError::ControlPathBlocker { path });
                }
            }
            Some("initialize-target") => {
                let metadata = fs::symlink_metadata(&path).map_err(|source| {
                    ProjectInitError::InspectRoot {
                        path: path.clone(),
                        source,
                    }
                })?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ProjectInitError::BuildTargetBlocker { path });
                }
            }
            _ => unexpected.push(PathBuf::from(".theseus").join(name)),
        }
    }
    Ok(())
}

fn count_root_entry(scanned: &mut usize, path: &Path) -> Result<(), ProjectInitError> {
    *scanned = scanned.saturating_add(1);
    if *scanned > MAX_ROOT_SCAN_ENTRIES {
        return Err(ProjectInitError::RootInspectionLimit {
            path: path.to_path_buf(),
            maximum: MAX_ROOT_SCAN_ENTRIES,
        });
    }
    Ok(())
}

/// An empty project that could not be initialized transactionally.
#[derive(Debug, Error)]
pub enum ProjectInitError {
    #[error("resolving project root {}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("project root is not a directory: {}", path.display())]
    RootNotDirectory { path: PathBuf },
    #[error("running Git repository-root validation")]
    GitCommand {
        #[source]
        source: std::io::Error,
    },
    #[error("project root is not a Git repository: {detail}")]
    NotGitRepository { detail: String },
    #[error("Git repository-root output is not UTF-8")]
    InvalidGitOutput {
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error("resolving Git repository root {}", path.display())]
    GitRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "project root is not the canonical Git top level: expected {}, found {}",
        expected.display(),
        actual.display()
    )]
    GitRootMismatch { expected: PathBuf, actual: PathBuf },
    #[error("Git repository-root output is {length} bytes; the maximum is {maximum}")]
    GitOutputTooLarge { length: usize, maximum: usize },
    #[error("resolving theseus-modeling path {}", path.display())]
    ModelingPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("theseus-modeling path is not a directory: {}", path.display())]
    ModelingNotDirectory { path: PathBuf },
    #[error("theseus-modeling path is not UTF-8: {}", path.display())]
    NonUtf8ModelingPath { path: PathBuf },
    #[error(
        "theseus-modeling path {} is {length} bytes; the maximum is {maximum}",
        path.display()
    )]
    ModelingPathTooLong {
        path: PathBuf,
        length: usize,
        maximum: usize,
    },
    #[error(
        "theseus-modeling path changed while it was validated: expected {}, found {}",
        expected.display(),
        actual.display()
    )]
    ModelingPathChanged { expected: PathBuf, actual: PathBuf },
    #[error("reading theseus-modeling manifest {}", path.display())]
    ModelingManifest {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid theseus-modeling manifest {}: {reason}", path.display())]
    InvalidModelingManifest { path: PathBuf, reason: &'static str },
    #[error("theseus-modeling manifest has {links} hard links: {}", path.display())]
    HardlinkedModelingManifest { path: PathBuf, links: u64 },
    #[error("theseus-modeling manifest changed while it was read: {}", path.display())]
    ModelingManifestChanged { path: PathBuf },
    #[error(
        "theseus-modeling manifest {} is {length} bytes; the maximum is {maximum}",
        path.display()
    )]
    ModelingManifestTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },
    #[error("inspecting empty project root {}", path.display())]
    InspectRoot {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspection of {} exceeded the limit of {maximum} entries", path.display())]
    RootInspectionLimit { path: PathBuf, maximum: usize },
    #[error("project control path is not a real file or directory: {}", path.display())]
    ControlPathBlocker { path: PathBuf },
    #[error("initialization target is not a real directory: {}", path.display())]
    BuildTargetBlocker { path: PathBuf },
    #[error("project root {} is not empty: {entries:?}", root.display())]
    NonEmptyRoot {
        root: PathBuf,
        entries: Vec<PathBuf>,
    },
    #[error("initial project contains duplicate target {path:?}")]
    DuplicateTarget { path: String },
    #[error("serializing project manifest")]
    SerializeManifest {
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    Layout(#[from] ProjectLayoutError),
    #[error(transparent)]
    Context(#[from] ProjectContextError),
    #[error(transparent)]
    Mutation(#[from] MutationError),
    #[error("running the initial all-target compile check")]
    CheckCommand {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("initial all-target compile check failed: {report}")]
    CheckFailed { report: CheckReport },
    #[error("opening the initialized project")]
    Open {
        #[source]
        source: ProjectOpenError,
    },
    #[error("cleaning initialization build artifacts at {}", path.display())]
    BuildCleanup {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "cleaning initialization build artifacts at {} after initialization failed: {primary}",
        path.display()
    )]
    FailureCleanup {
        primary: Box<ProjectInitError>,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("initialization failed and rollback also failed: {primary}; {rollback}")]
    Rollback {
        primary: Box<ProjectInitError>,
        rollback: MutationError,
    },
    #[error("joining initialization filesystem worker")]
    BlockingTask {
        #[source]
        source: tokio::task::JoinError,
    },
}

#[cfg(test)]
mod tests {
    use std::{
        process::{Child, Command, Stdio},
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
    };

    use tokio::io::AsyncWriteExt;

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    struct TestRoot(PathBuf);

    impl TestRoot {
        fn new(label: &str) -> Self {
            let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "theseus-initialize-{label}-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct FixedChecker(CheckReport);

    #[async_trait]
    impl InitChecker for FixedChecker {
        async fn check(&self, project: &ProjectContext) -> anyhow::Result<CheckReport> {
            if !self.0.ok {
                fs::create_dir(project.root().join(INITIALIZE_TARGET))?;
                fs::write(
                    project.root().join(INITIALIZE_TARGET).join("artifact"),
                    b"build output",
                )?;
            }
            Ok(self.0.clone())
        }
    }

    struct CancellationChecker {
        started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    }

    #[async_trait]
    impl InitChecker for CancellationChecker {
        async fn check(&self, project: &ProjectContext) -> anyhow::Result<CheckReport> {
            fs::create_dir(project.root().join(INITIALIZE_TARGET))?;
            fs::write(
                project.root().join(INITIALIZE_TARGET).join("artifact"),
                b"partial build",
            )?;
            let sender = self.started.lock().unwrap().take();
            if let Some(sender) = sender {
                let _ = sender.send(());
            }
            std::future::pending().await
        }
    }

    fn modeling_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("modeling")
    }

    fn git_init(root: &Path) {
        let output = Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(output.status.success());
    }

    fn spawn_initializer_helper(root: &Path, mode: &str) -> Child {
        Command::new(std::env::current_exe().expect("the test executable has a path"))
            .args([
                "--exact",
                "initialize::tests::interrupted_initializer_process_helper",
                "--nocapture",
            ])
            .env("THESEUS_INITIALIZER_HELPER", mode)
            .env("THESEUS_INITIALIZER_ROOT", root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("the initializer helper starts")
    }

    fn wait_for_initializer_helper(child: &mut Child, marker: &Path) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !marker.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for {}",
                marker.display()
            );
            assert!(
                child.try_wait().unwrap().is_none(),
                "initializer helper exited before publishing its marker"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[tokio::test]
    async fn interrupted_initializer_process_helper() {
        let Ok(mode) = std::env::var("THESEUS_INITIALIZER_HELPER") else {
            return;
        };
        let root = PathBuf::from(
            std::env::var_os("THESEUS_INITIALIZER_ROOT")
                .expect("the initializer helper receives a root"),
        );
        let changes = match mode.as_str() {
            "exact" => {
                let project_id = ProjectId::new("fixture").unwrap();
                let modeling_path = canonical_modeling_path(&modeling_path()).unwrap();
                let model = initial_model(project_id.as_str());
                let layout = RustWorkspaceLayout::new(
                    project_id,
                    ModelRecord::json(MODEL_RECORD_PATH).unwrap(),
                );
                initial_changes(&model, &layout, &modeling_path).unwrap()
            }
            "foreign" => vec![MutationFile::text("foreign.txt", "foreign")],
            other => panic!("unknown initializer helper mode {other}"),
        };
        let mut mutation = FsMutation::begin_async(root.clone(), Vec::new())
            .await
            .expect("the helper acquires the repository lease");
        mutation
            .apply(&changes)
            .await
            .expect("the helper publishes its interrupted mutation");
        fs::write(root.join(format!(".git/initializer-{mode}-ready")), b"")
            .expect("the helper publishes its marker");
        std::future::pending::<()>().await;
    }

    async fn initialize_with_checker(
        root: &Path,
        checker: &dyn InitChecker,
    ) -> Result<ProjectContext, ProjectInitError> {
        let project_id = ProjectId::new("fixture").unwrap();
        let modeling_path = canonical_modeling_path(&modeling_path())?;
        let model = initial_model(project_id.as_str());
        let layout =
            RustWorkspaceLayout::new(project_id, ModelRecord::json(MODEL_RECORD_PATH).unwrap());
        let provisional = ProjectContext::new(root, model.clone(), layout.clone())?;
        let changes = initial_changes(&model, &layout, &modeling_path)?;
        let mut mutation = begin_initialization_mutation(root, &changes).await?;
        let target = BuildTargetGuard::prepare(root)?;
        mutation.apply(&changes).await?;
        initialize_after_apply(mutation, provisional, target, checker).await
    }

    #[tokio::test]
    async fn initializes_compiles_and_reopens_a_minimal_project() {
        let root = TestRoot::new("success");
        git_init(root.path());
        let context = initialize_project(
            root.path(),
            ProjectId::new("new-app").unwrap(),
            modeling_path(),
        )
        .await
        .expect("the empty project initializes");

        assert_eq!(context.root(), root.path().canonicalize().unwrap());
        assert_eq!(context.descriptor().project_id().as_str(), "new-app");
        assert_eq!(context.initial_model().services[0].name, "App");
        assert!(context.initial_model().services[0].operations.is_empty());
        assert_eq!(
            context.initial_model().services[0].outbound[0].name,
            "store"
        );
        assert_eq!(context.initial_model().inbounds[0].name, "app-cli");
        assert!(root.path().join(ROOT_MANIFEST_PATH).is_file());
        assert!(root.path().join(PROJECT_MANIFEST_PATH).is_file());
        assert!(root.path().join(MODEL_RECORD_PATH).is_file());
        assert!(root.path().join("rust/app/src/generated.rs").is_file());
        assert!(root.path().join("rust/app-cli/src/generated.rs").is_file());
        let app = fs::read_to_string(root.path().join("rust/app/src/lib.rs")).unwrap();
        assert!(app.contains("pub struct MemoryStore;"));
        assert!(app.contains("impl Store for MemoryStore {}"));
        assert!(app.contains("pub fn load_model()"));
        let app_manifest = fs::read_to_string(root.path().join("rust/app/Cargo.toml")).unwrap();
        assert!(app_manifest.contains("theseus-modeling = { workspace = true }"));
        assert!(app_manifest.contains("serde_json = { workspace = true }"));
        assert!(root.path().join(LOCKFILE_PATH).is_file());
        assert!(!root.path().join(INITIALIZE_TARGET).exists());
        assert!(!root.path().join("target").exists());
        let main = fs::read_to_string(root.path().join("rust/app-cli/src/main.rs")).unwrap();
        assert!(main.contains("app::Standalone"));
        assert!(!main.contains("todo!"));

        let reopened = ProjectContext::open(root.path()).expect("the initialized project reopens");
        assert_eq!(reopened.descriptor(), context.descriptor());
        assert_eq!(reopened.initial_model(), context.initial_model());
        let manifest = fs::read_to_string(root.path().join(PROJECT_MANIFEST_PATH)).unwrap();
        assert!(manifest.ends_with('\n'));
        serde_json::from_str::<ProjectManifest>(&manifest).unwrap();

        let output = Command::new("cargo")
            .args(["run", "--quiet", "-p", "app-cli"])
            .arg("--target-dir")
            .arg(root.path().join(".theseus/run-target"))
            .args(["--", "--help"])
            .env_remove("CARGO_TARGET_DIR")
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "seeded CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("app-cli"));
    }

    #[tokio::test]
    async fn refuses_nonempty_roots_without_touching_existing_entries() {
        let root = TestRoot::new("nonempty");
        git_init(root.path());
        fs::write(root.path().join("keep.txt"), b"keep exactly\n").unwrap();
        let error = initialize_project(
            root.path(),
            ProjectId::new("new-app").unwrap(),
            modeling_path(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ProjectInitError::NonEmptyRoot { .. }));
        assert_eq!(
            fs::read(root.path().join("keep.txt")).unwrap(),
            b"keep exactly\n"
        );
        assert!(!root.path().join(ROOT_MANIFEST_PATH).exists());
        assert!(!root.path().join(".theseus").exists());
    }

    #[tokio::test]
    async fn refuses_a_nested_directory_before_project_writes() {
        let outer = TestRoot::new("outer-git");
        git_init(outer.path());
        let nested = outer.path().join("nested");
        fs::create_dir(&nested).unwrap();

        let error = initialize_project(
            &nested,
            ProjectId::new("nested-app").unwrap(),
            modeling_path(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ProjectInitError::GitRootMismatch { .. }));
        assert!(!nested.join(ROOT_MANIFEST_PATH).exists());
        assert!(!nested.join(".theseus").exists());
    }

    #[tokio::test]
    async fn refuses_a_non_git_directory_without_creating_control_state() {
        let root = TestRoot::new("not-git");
        let error = initialize_project(
            root.path(),
            ProjectId::new("not-git").unwrap(),
            modeling_path(),
        )
        .await
        .unwrap_err();

        assert!(matches!(error, ProjectInitError::NotGitRepository { .. }));
        assert!(!root.path().join(".theseus").exists());
    }

    #[tokio::test]
    async fn failed_validation_rolls_back_every_declared_project_file() {
        let root = TestRoot::new("rollback");
        git_init(root.path());
        fs::write(root.path().join(".git/preserved"), b"git state").unwrap();
        let checker = FixedChecker(CheckReport::failure("intentional failure"));
        let error = initialize_with_checker(root.path(), &checker)
            .await
            .unwrap_err();

        assert!(matches!(error, ProjectInitError::CheckFailed { .. }));
        assert_eq!(
            fs::read(root.path().join(".git/preserved")).unwrap(),
            b"git state"
        );
        let mut entries: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        entries.sort();
        assert_eq!(entries, [".git", ".theseus"]);
        let control_entries: Vec<_> = fs::read_dir(root.path().join(".theseus"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(control_entries, ["repository.lock"]);
        assert!(!root.path().join("target").exists());
    }

    #[tokio::test]
    async fn cancellation_cleans_the_internal_target_and_rolls_back_sources() {
        let root = TestRoot::new("cancel");
        git_init(root.path());
        let (started, ready) = tokio::sync::oneshot::channel();
        let checker = Arc::new(CancellationChecker {
            started: Mutex::new(Some(started)),
        });
        let task_root = root.path().to_path_buf();
        let task_checker = Arc::clone(&checker);
        let task = tokio::spawn(async move {
            initialize_with_checker(&task_root, task_checker.as_ref()).await
        });

        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!root.path().join(INITIALIZE_TARGET).exists());
        assert!(!root.path().join(ROOT_MANIFEST_PATH).exists());
        assert!(!root.path().join(MODEL_RECORD_PATH).exists());
        assert!(root.path().join(".theseus/repository.lock").is_file());
    }

    #[tokio::test]
    async fn a_stale_real_internal_target_is_removed_before_retry() {
        let root = TestRoot::new("stale-target");
        git_init(root.path());
        fs::create_dir(root.path().join(".theseus")).unwrap();
        fs::create_dir(root.path().join(INITIALIZE_TARGET)).unwrap();
        fs::write(
            root.path().join(INITIALIZE_TARGET).join("stale"),
            b"stale build",
        )
        .unwrap();

        let context =
            initialize_with_checker(root.path(), &FixedChecker(CheckReport::success("checked")))
                .await
                .expect("a stale real target is recoverable");
        assert_eq!(context.descriptor().project_id().as_str(), "fixture");
        assert!(!root.path().join(INITIALIZE_TARGET).exists());
    }

    #[tokio::test]
    async fn partial_rollback_cleanup_and_a_stale_target_do_not_strand_retry() {
        let root = TestRoot::new("cleanup-with-target");
        git_init(root.path());
        fs::create_dir(root.path().join(".theseus")).unwrap();
        fs::create_dir(root.path().join(".theseus/mutation.cleanup")).unwrap();
        fs::write(
            root.path().join(".theseus/mutation.cleanup/state"),
            b"rolled-back\n",
        )
        .unwrap();
        fs::create_dir(root.path().join(INITIALIZE_TARGET)).unwrap();
        fs::write(
            root.path().join(INITIALIZE_TARGET).join("stale"),
            b"stale build",
        )
        .unwrap();

        let context =
            initialize_with_checker(root.path(), &FixedChecker(CheckReport::success("checked")))
                .await
                .expect("the partial rollback cleanup and stale target are recoverable");
        assert_eq!(context.descriptor().project_id().as_str(), "fixture");
        assert!(!root.path().join(".theseus/mutation.cleanup").exists());
        assert!(!root.path().join(INITIALIZE_TARGET).exists());
    }

    #[tokio::test]
    async fn a_non_directory_internal_target_is_refused_before_writes() {
        let root = TestRoot::new("target-blocker");
        git_init(root.path());
        fs::create_dir(root.path().join(".theseus")).unwrap();
        fs::write(root.path().join(INITIALIZE_TARGET), b"blocker").unwrap();

        let error = initialize_project(
            root.path(),
            ProjectId::new("blocked-app").unwrap(),
            modeling_path(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, ProjectInitError::BuildTargetBlocker { .. }));
        assert!(!root.path().join(ROOT_MANIFEST_PATH).exists());
        assert_eq!(
            fs::read(root.path().join(INITIALIZE_TARGET)).unwrap(),
            b"blocker"
        );
    }

    #[tokio::test]
    async fn a_preexisting_mutation_journal_is_refused_without_recovery() {
        let root = TestRoot::new("journal-blocker");
        git_init(root.path());
        fs::create_dir(root.path().join(".theseus")).unwrap();
        fs::create_dir(root.path().join(".theseus/mutation")).unwrap();
        fs::write(root.path().join(".theseus/mutation/state"), b"prepared\n").unwrap();

        let error = initialize_project(
            root.path(),
            ProjectId::new("blocked-app").unwrap(),
            modeling_path(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            ProjectInitError::Mutation(MutationError::UnrecoverableCreationJournal { .. })
        ));
        assert_eq!(
            fs::read(root.path().join(".theseus/mutation/state")).unwrap(),
            b"prepared\n"
        );
        assert!(!root.path().join(".theseus/repository.lock").exists());
    }

    #[tokio::test]
    async fn a_killed_exact_initializer_is_recovered_and_retried() {
        let root = TestRoot::new("killed-exact");
        git_init(root.path());
        let marker = root.path().join(".git/initializer-exact-ready");
        let mut child = spawn_initializer_helper(root.path(), "exact");
        wait_for_initializer_helper(&mut child, &marker);
        child.kill().expect("the interrupted initializer is killed");
        child.wait().expect("the interrupted initializer is reaped");

        assert!(root.path().join(".theseus/mutation").is_dir());
        assert!(root.path().join(ROOT_MANIFEST_PATH).is_file());
        let context =
            initialize_with_checker(root.path(), &FixedChecker(CheckReport::success("checked")))
                .await
                .expect("the exact interrupted creation is recovered and retried");

        assert_eq!(context.descriptor().project_id().as_str(), "fixture");
        assert!(!root.path().join(".theseus/mutation").exists());
        assert!(root.path().join(ROOT_MANIFEST_PATH).is_file());
    }

    #[tokio::test]
    async fn a_killed_foreign_journal_is_refused_without_modification() {
        let root = TestRoot::new("killed-foreign");
        git_init(root.path());
        let marker = root.path().join(".git/initializer-foreign-ready");
        let mut child = spawn_initializer_helper(root.path(), "foreign");
        wait_for_initializer_helper(&mut child, &marker);
        child.kill().expect("the foreign writer is killed");
        child.wait().expect("the foreign writer is reaped");
        let manifest = fs::read(root.path().join(".theseus/mutation/manifest.json")).unwrap();
        let state = fs::read(root.path().join(".theseus/mutation/state")).unwrap();

        let error =
            initialize_with_checker(root.path(), &FixedChecker(CheckReport::success("checked")))
                .await
                .unwrap_err();

        assert!(matches!(
            error,
            ProjectInitError::Mutation(MutationError::UnrecoverableCreationJournal { .. })
        ));
        assert_eq!(
            fs::read(root.path().join(".theseus/mutation/manifest.json")).unwrap(),
            manifest
        );
        assert_eq!(
            fs::read(root.path().join(".theseus/mutation/state")).unwrap(),
            state
        );
        assert_eq!(
            fs::read(root.path().join("foreign.txt")).unwrap(),
            b"foreign"
        );
        assert!(!root.path().join(ROOT_MANIFEST_PATH).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_hardlinked_modeling_manifest_is_rejected() {
        let engine = TestRoot::new("hardlinked-engine");
        fs::write(
            engine.path().join(MODELING_MANIFEST_PATH),
            "[package]\nname = \"theseus-modeling\"\n",
        )
        .unwrap();
        fs::hard_link(
            engine.path().join(MODELING_MANIFEST_PATH),
            engine.path().join("manifest-link"),
        )
        .unwrap();

        assert!(matches!(
            canonical_modeling_path(engine.path()),
            Err(ProjectInitError::HardlinkedModelingManifest { .. })
        ));
    }

    #[test]
    fn modeling_paths_are_escaped_for_toml_basic_strings() {
        assert_eq!(toml_basic_string("a\\b\"c\n"), "a\\\\b\\\"c\\n");
    }

    #[test]
    fn duplicate_targets_are_rejected_case_insensitively() {
        let error = reject_duplicate_targets(&[
            MutationFile::text("file.rs", "one"),
            MutationFile::text("FILE.rs", "two"),
        ])
        .unwrap_err();
        assert!(matches!(error, ProjectInitError::DuplicateTarget { .. }));
    }

    #[tokio::test]
    async fn git_stream_reads_stop_at_the_configured_bound() {
        let (mut writer, reader) = tokio::io::duplex(64);
        writer.write_all(b"seventeen-bytes!!").await.unwrap();
        drop(writer);

        let error = read_limited_git_stream(reader, 16).await.unwrap_err();
        assert!(matches!(
            error,
            ProjectInitError::GitOutputTooLarge {
                length: 17,
                maximum: 16
            }
        ));
    }

    #[test]
    fn root_inspection_stops_at_the_configured_entry_bound() {
        let root = TestRoot::new("root-bound");
        for index in 0..=MAX_ROOT_SCAN_ENTRIES {
            fs::write(root.path().join(format!("entry-{index:04}")), b"").unwrap();
        }

        let error = unexpected_root_entries(root.path()).unwrap_err();
        assert!(matches!(
            error,
            ProjectInitError::RootInspectionLimit {
                maximum: MAX_ROOT_SCAN_ENTRIES,
                ..
            }
        ));
    }
}
