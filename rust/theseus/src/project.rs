//! Immutable, operator-selected project identity and projection policy.

use std::{
    fs::{self, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use theseus_modeling::{
    CheckpointProjectDescriptor, GeneratedFile, Inbound, Model, ModelRecord, ProjectLayoutError,
    RustWorkspaceLayout, Service,
};
use thiserror::Error;

use crate::{ExpectedFile, ExpectedFileSet, MutationError, Project, validate_workspace_paths};

/// The root-relative manifest used to reopen a durable Theseus project.
pub const PROJECT_MANIFEST_PATH: &str = "theseus.json";

const MAX_PROJECT_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_PROJECT_MODEL_BYTES: u64 = 16 * 1024 * 1024;

/// Version-one durable project metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectManifest {
    layout: RustWorkspaceLayout,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectManifestWire {
    version: u32,
    layout: RustWorkspaceLayout,
}

impl ProjectManifest {
    pub const VERSION: u32 = 1;

    pub fn new(layout: RustWorkspaceLayout) -> Self {
        Self { layout }
    }

    pub fn version(&self) -> u32 {
        Self::VERSION
    }

    pub fn layout(&self) -> &RustWorkspaceLayout {
        &self.layout
    }

    pub fn into_layout(self) -> RustWorkspaceLayout {
        self.layout
    }
}

impl Serialize for ProjectManifest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ProjectManifestWire {
            version: Self::VERSION,
            layout: self.layout.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProjectManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ProjectManifestWire::deserialize(deserializer)?;
        if wire.version != Self::VERSION {
            return Err(D::Error::custom(format_args!(
                "unsupported project manifest version {}",
                wire.version
            )));
        }
        Ok(Self::new(wire.layout))
    }
}

#[derive(Deserialize)]
struct ProjectManifestVersionProbe {
    version: u32,
    layout: ProjectLayoutVersionProbe,
}

#[derive(Deserialize)]
struct ProjectLayoutVersionProbe {
    version: u32,
}

/// One trusted project boundary shared by every adapter in a session.
///
/// The root is canonicalized once at construction. The model and versioned
/// layout are immutable, so an agent operation cannot redirect later reads,
/// writes, compiler processes, or checkpoints to another workspace.
#[derive(Clone, Debug)]
pub struct ProjectContext(Arc<ProjectDefinition>);

#[derive(Debug)]
struct ProjectDefinition {
    root: PathBuf,
    initial_model: Model,
    layout: RustWorkspaceLayout,
}

impl ProjectContext {
    /// Establish a project capability from an operator-selected root.
    pub fn new(
        root: impl AsRef<Path>,
        initial_model: Model,
        layout: RustWorkspaceLayout,
    ) -> Result<Self, ProjectContextError> {
        let supplied = root.as_ref();
        let root = fs::canonicalize(supplied).map_err(|source| ProjectContextError::Root {
            path: supplied.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(ProjectContextError::NotDirectory { path: root });
        }

        // Force the full live and frozen policies through validation before the
        // context can be shared with any adapter.
        layout.generated_files(&initial_model)?;
        layout.authored_impls(&initial_model)?;
        layout.interior_impls(&initial_model)?;
        let owned_paths = layout.owned_paths(&initial_model)?;
        validate_workspace_paths(&owned_paths)?;

        Ok(Self(Arc::new(ProjectDefinition {
            root,
            initial_model,
            layout,
        })))
    }

    /// Reconstruct a project from its root manifest and canonical JSON model.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ProjectOpenError> {
        let supplied = root.as_ref();
        let root = fs::canonicalize(supplied).map_err(|source| ProjectOpenError::Root {
            path: supplied.to_path_buf(),
            source,
        })?;
        if !root.is_dir() {
            return Err(ProjectOpenError::NotDirectory { path: root });
        }

        let manifest_bytes =
            read_project_file(&root, PROJECT_MANIFEST_PATH, MAX_PROJECT_MANIFEST_BYTES)?;
        let versions: ProjectManifestVersionProbe = serde_json::from_slice(&manifest_bytes)
            .map_err(|source| ProjectOpenError::InvalidManifest { source })?;
        if versions.version != ProjectManifest::VERSION {
            return Err(ProjectOpenError::UnsupportedManifestVersion {
                version: versions.version,
            });
        }
        if !RustWorkspaceLayout::supports_version(versions.layout.version) {
            return Err(ProjectOpenError::UnsupportedLayoutVersion {
                version: versions.layout.version,
            });
        }
        let manifest: ProjectManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|source| ProjectOpenError::InvalidManifest { source })?;
        let layout = manifest.into_layout();
        let model_path = match layout.model_record() {
            ModelRecord::Json(record) => record.path().to_string(),
            ModelRecord::RustBuilder(record) => {
                return Err(ProjectOpenError::UnsupportedRustBuilderModel {
                    path: record.path().to_string(),
                });
            }
        };

        let model_bytes = read_project_file(&root, &model_path, MAX_PROJECT_MODEL_BYTES)?;
        let model: Model = serde_json::from_slice(&model_bytes).map_err(|source| {
            ProjectOpenError::InvalidModelRecord {
                path: model_path.clone(),
                source,
            }
        })?;
        let generated = layout.generated_files(&model)?;
        let canonical = generated
            .iter()
            .find(|file| file.path == model_path)
            .ok_or_else(|| ProjectOpenError::MissingModelProjection {
                path: model_path.clone(),
            })?;
        if canonical.contents.as_bytes() != model_bytes {
            return Err(ProjectOpenError::NonCanonicalModelRecord { path: model_path });
        }

        Self::new(root, model, layout).map_err(ProjectOpenError::Context)
    }

    pub fn root(&self) -> &Path {
        &self.0.root
    }

    pub fn initial_model(&self) -> &Model {
        &self.0.initial_model
    }

    pub fn layout(&self) -> &RustWorkspaceLayout {
        &self.0.layout
    }

    pub fn descriptor(&self) -> CheckpointProjectDescriptor {
        self.layout().checkpoint_descriptor()
    }

    /// Prove the selected pathname still resolves to the directory fixed at
    /// construction. This narrows rename-and-replace attacks; individual file
    /// opens still share the documented same-account pathname race boundary.
    pub fn validate_root(&self) -> Result<(), ProjectRootError> {
        let actual =
            self.root()
                .canonicalize()
                .map_err(|source| ProjectRootError::Unavailable {
                    path: self.root().to_path_buf(),
                    source,
                })?;
        if actual != self.root() {
            return Err(ProjectRootError::Changed {
                expected: self.root().to_path_buf(),
                actual,
            });
        }
        Ok(())
    }

    /// Require another context to name the same root and frozen layout.
    pub fn ensure_same_project(&self, actual: &Self) -> Result<(), ProjectBindingError> {
        if self.root() != actual.root() {
            return Err(ProjectBindingError::RootMismatch {
                expected: self.root().to_path_buf(),
                actual: actual.root().to_path_buf(),
            });
        }
        let expected = self.descriptor();
        let actual = actual.descriptor();
        if expected != actual {
            return Err(ProjectBindingError::LayoutMismatch {
                expected: Box::new(expected),
                actual: Box::new(actual),
            });
        }
        Ok(())
    }

    pub fn generated_files(&self, model: &Model) -> Result<Vec<GeneratedFile>, ProjectLayoutError> {
        self.layout().generated_files(model)
    }

    pub fn projected_files(&self, model: &Model) -> Result<Vec<GeneratedFile>, ProjectLayoutError> {
        Ok(self
            .generated_files(model)?
            .into_iter()
            .filter(|file| crate_is_scaffolded(self.root(), file))
            .collect())
    }

    pub fn expected_files(&self, model: &Model) -> Result<ExpectedFileSet, ProjectLayoutError> {
        Ok(self
            .generated_files(model)?
            .into_iter()
            .map(|file| ExpectedFile {
                contents: crate_is_scaffolded(self.root(), &file).then_some(file.contents),
                path: file.path,
            })
            .collect())
    }

    pub fn authored_impl_path(
        &self,
        model: &Model,
        service: &Service,
    ) -> Result<String, ProjectLayoutError> {
        self.layout().authored_impl_path(model, service)
    }

    pub fn adapter_impl_path(
        &self,
        model: &Model,
        service: &Service,
    ) -> Result<String, ProjectLayoutError> {
        self.layout().adapter_impl_path(model, service)
    }

    pub fn inbound_adapter_impl_path(
        &self,
        model: &Model,
        inbound: &Inbound,
    ) -> Result<String, ProjectLayoutError> {
        self.layout().inbound_adapter_impl_path(model, inbound)
    }

    pub fn authored_impls(
        &self,
        model: &Model,
    ) -> Result<Vec<(String, String)>, ProjectLayoutError> {
        self.layout().authored_impls(model)
    }

    pub fn interior_impls(
        &self,
        model: &Model,
    ) -> Result<Vec<(String, String)>, ProjectLayoutError> {
        self.layout().interior_impls(model)
    }

    pub fn owned_paths(&self, model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
        self.layout().owned_paths(model)
    }

    /// Resolve an existing user-selected path without crossing this project.
    pub fn resolve_existing(&self, path: &str) -> Result<PathBuf, ProjectPathError> {
        self.validate_root()?;
        if !path.is_empty() {
            validate_workspace_paths(&[path.to_owned()]).map_err(|source| {
                ProjectPathError::Invalid {
                    path: path.to_owned(),
                    source,
                }
            })?;
        }
        let resolved =
            self.root()
                .join(path)
                .canonicalize()
                .map_err(|source| ProjectPathError::Resolve {
                    path: path.to_owned(),
                    source,
                })?;
        if !resolved.starts_with(self.root()) {
            return Err(ProjectPathError::Escapes {
                path: path.to_owned(),
            });
        }
        Ok(resolved)
    }
}

#[async_trait::async_trait]
impl Project for ProjectContext {
    async fn context(&self) -> anyhow::Result<ProjectContext> {
        self.validate_root()?;
        Ok(self.clone())
    }
}

/// Theseus's checked-in self-project composition.
pub fn theseus_project() -> Result<ProjectContext, ProjectContextError> {
    ProjectContext::new(
        crate::workspace_root(),
        theseus_model::theseus_model(),
        theseus_model::project_layout()?,
    )
}

fn crate_is_scaffolded(root: &Path, file: &GeneratedFile) -> bool {
    match file
        .path
        .strip_prefix("rust/")
        .and_then(|rest| rest.split_once('/'))
    {
        Some((directory, _)) => root
            .join("rust")
            .join(directory)
            .join("Cargo.toml")
            .exists(),
        None => true,
    }
}

fn read_project_file(
    root: &Path,
    relative: &str,
    maximum: u64,
) -> Result<Vec<u8>, ProjectOpenError> {
    let requested = root.join(relative);
    let resolved = requested
        .canonicalize()
        .map_err(|source| ProjectOpenError::ResolveFile {
            path: requested.clone(),
            source,
        })?;
    if !resolved.starts_with(root) {
        return Err(ProjectOpenError::EscapingFile {
            path: requested,
            resolved,
        });
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(&resolved)
        .map_err(|source| ProjectOpenError::OpenFile {
            path: resolved.clone(),
            source,
        })?;
    let before = file
        .metadata()
        .map_err(|source| ProjectOpenError::InspectFile {
            path: resolved.clone(),
            source,
        })?;
    if !before.is_file() {
        return Err(ProjectOpenError::NotRegularFile { path: resolved });
    }
    let links = hard_link_count(&before);
    if links > 1 {
        return Err(ProjectOpenError::HardlinkedFile {
            path: resolved,
            links,
        });
    }
    if before.len() > maximum {
        return Err(ProjectOpenError::FileTooLarge {
            path: resolved,
            length: before.len(),
            maximum,
        });
    }

    let mut contents = Vec::with_capacity(before.len() as usize);
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut contents)
        .map_err(|source| ProjectOpenError::ReadFile {
            path: resolved.clone(),
            source,
        })?;
    if contents.len() as u64 > maximum {
        return Err(ProjectOpenError::FileTooLarge {
            path: resolved,
            length: contents.len() as u64,
            maximum,
        });
    }
    let after = file
        .metadata()
        .map_err(|source| ProjectOpenError::InspectFile {
            path: resolved.clone(),
            source,
        })?;
    if contents.len() as u64 != before.len() || !same_file_state(&before, &after) {
        return Err(ProjectOpenError::ChangedWhileReading { path: resolved });
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
fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
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
fn same_file_state(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    before.len() == after.len()
}

/// A durable project that cannot be reconstructed safely.
#[derive(Debug, Error)]
pub enum ProjectOpenError {
    #[error("resolving project root {}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("project root is not a directory: {}", path.display())]
    NotDirectory { path: PathBuf },
    #[error("resolving project file {}", path.display())]
    ResolveFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "project file {} resolves outside the project root to {}",
        path.display(),
        resolved.display()
    )]
    EscapingFile { path: PathBuf, resolved: PathBuf },
    #[error("opening project file {}", path.display())]
    OpenFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("inspecting project file {}", path.display())]
    InspectFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("project file is not a regular file: {}", path.display())]
    NotRegularFile { path: PathBuf },
    #[error("project file has {links} hard links: {}", path.display())]
    HardlinkedFile { path: PathBuf, links: u64 },
    #[error(
        "project file {} is {length} bytes; the maximum is {maximum}",
        path.display()
    )]
    FileTooLarge {
        path: PathBuf,
        length: u64,
        maximum: u64,
    },
    #[error("reading project file {}", path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("project file changed while it was read: {}", path.display())]
    ChangedWhileReading { path: PathBuf },
    #[error("project manifest is not valid version-one JSON")]
    InvalidManifest {
        #[source]
        source: serde_json::Error,
    },
    #[error("project manifest has unsupported version {version}")]
    UnsupportedManifestVersion { version: u32 },
    #[error("Rust workspace layout has unsupported version {version}")]
    UnsupportedLayoutVersion { version: u32 },
    #[error("Rust-builder model records cannot be opened from disk: {path:?}")]
    UnsupportedRustBuilderModel { path: String },
    #[error("JSON model record {path:?} is invalid")]
    InvalidModelRecord {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("layout did not project its model record at {path:?}")]
    MissingModelProjection { path: String },
    #[error("JSON model record is stale or noncanonical: {path:?}")]
    NonCanonicalModelRecord { path: String },
    #[error(transparent)]
    Layout(#[from] ProjectLayoutError),
    #[error(transparent)]
    Context(#[from] ProjectContextError),
}

#[derive(Debug, Error)]
pub enum ProjectContextError {
    #[error("resolving project root {}", path.display())]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("project root is not a directory: {}", path.display())]
    NotDirectory { path: PathBuf },
    #[error(transparent)]
    Layout(#[from] ProjectLayoutError),
    #[error("project ownership contains an unsafe path")]
    Ownership {
        #[from]
        source: MutationError,
    },
}

#[derive(Debug, Error)]
pub enum ProjectRootError {
    #[error("project root is no longer available at {}", path.display())]
    Unavailable {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "project root changed since selection: expected {}, found {}",
        expected.display(),
        actual.display()
    )]
    Changed { expected: PathBuf, actual: PathBuf },
}

#[derive(Debug, Error)]
pub enum ProjectBindingError {
    #[error(
        "adapter project root does not match the session: expected {}, found {}",
        expected.display(),
        actual.display()
    )]
    RootMismatch { expected: PathBuf, actual: PathBuf },
    #[error("adapter project layout does not match the session")]
    LayoutMismatch {
        expected: Box<CheckpointProjectDescriptor>,
        actual: Box<CheckpointProjectDescriptor>,
    },
}

#[derive(Debug, Error)]
pub enum ProjectPathError {
    #[error(transparent)]
    Root(#[from] ProjectRootError),
    #[error("invalid project path {path:?}: {source}")]
    Invalid {
        path: String,
        #[source]
        source: MutationError,
    },
    #[error("no such project path {path:?}: {source}")]
    Resolve {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("project path escapes the selected root: {path:?}")]
    Escapes { path: String },
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use theseus_modeling::ProjectId;

    use super::*;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    fn temporary_root(label: &str) -> PathBuf {
        let sequence = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "theseus-project-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        root
    }

    fn json_project() -> (Model, RustWorkspaceLayout) {
        (
            Model::new("Fixture").crate_node("fixture", "fixture", 1, &[]),
            RustWorkspaceLayout::new(
                ProjectId::new("fixture").unwrap(),
                ModelRecord::json("model.json").unwrap(),
            ),
        )
    }

    fn canonical_model(layout: &RustWorkspaceLayout, model: &Model) -> String {
        layout
            .generated_files(model)
            .unwrap()
            .into_iter()
            .find(|file| file.path == layout.model_record().path())
            .unwrap()
            .contents
    }

    fn write_manifest(root: &Path, layout: &RustWorkspaceLayout) {
        fs::write(
            root.join(PROJECT_MANIFEST_PATH),
            serde_json::to_vec_pretty(&ProjectManifest::new(layout.clone())).unwrap(),
        )
        .unwrap();
    }

    fn write_json_project(root: &Path, model: &Model, layout: &RustWorkspaceLayout) {
        write_manifest(root, layout);
        fs::write(
            root.join(layout.model_record().path()),
            canonical_model(layout, model),
        )
        .unwrap();
    }

    #[test]
    fn project_manifest_has_a_strict_versioned_schema() {
        let (_, layout) = json_project();
        let manifest = ProjectManifest::new(layout);
        let encoded = serde_json::to_string(&manifest).unwrap();
        assert_eq!(
            encoded,
            r#"{"version":1,"layout":{"version":2,"project_id":"fixture","model_record":{"format":"json","path":"model.json"}}}"#
        );
        assert_eq!(
            serde_json::from_str::<ProjectManifest>(&encoded).unwrap(),
            manifest
        );
        assert_eq!(manifest.version(), ProjectManifest::VERSION);

        let future = encoded.replacen("\"version\":1", "\"version\":2", 1);
        assert!(serde_json::from_str::<ProjectManifest>(&future).is_err());
        let unknown = encoded.replacen("\"version\":1", "\"version\":1,\"future\":true", 1);
        assert!(serde_json::from_str::<ProjectManifest>(&unknown).is_err());
    }

    #[test]
    fn a_canonical_json_project_opens_from_disk() {
        let root = temporary_root("open-json");
        let (model, layout) = json_project();
        write_json_project(&root, &model, &layout);

        let project = ProjectContext::open(&root).unwrap();
        assert_eq!(project.root(), root.canonicalize().unwrap());
        assert_eq!(project.initial_model(), &model);
        assert_eq!(project.layout(), &layout);
        assert_eq!(project.descriptor(), layout.checkpoint_descriptor());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_version_one_layout_manifest_opens_as_the_current_layout() {
        let root = temporary_root("open-version-one");
        let (model, layout) = json_project();
        write_json_project(&root, &model, &layout);
        let manifest_path = root.join(PROJECT_MANIFEST_PATH);
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            manifest.replacen(
                "\"layout\": {\n    \"version\": 2",
                "\"layout\": {\n    \"version\": 1",
                1,
            ),
        )
        .unwrap();

        let project = ProjectContext::open(&root).unwrap();
        assert_eq!(project.descriptor().version(), RustWorkspaceLayout::VERSION);
        let owned = project.owned_paths(project.initial_model()).unwrap();
        assert!(owned.contains(&"Cargo.toml".to_string()));
        assert!(owned.contains(&PROJECT_MANIFEST_PATH.to_string()));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_open_rejects_unknown_manifest_and_layout_versions() {
        let (model, layout) = json_project();

        let manifest_root = temporary_root("future-manifest");
        write_json_project(&manifest_root, &model, &layout);
        let manifest_path = manifest_root.join(PROJECT_MANIFEST_PATH);
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            manifest.replacen("\"version\": 1", "\"version\": 2", 1),
        )
        .unwrap();
        assert!(matches!(
            ProjectContext::open(&manifest_root),
            Err(ProjectOpenError::UnsupportedManifestVersion { version: 2 })
        ));

        let layout_root = temporary_root("future-layout");
        write_json_project(&layout_root, &model, &layout);
        let manifest_path = layout_root.join(PROJECT_MANIFEST_PATH);
        let manifest = fs::read_to_string(&manifest_path).unwrap();
        fs::write(
            &manifest_path,
            manifest.replacen(
                "\"layout\": {\n    \"version\": 2",
                "\"layout\": {\n    \"version\": 3",
                1,
            ),
        )
        .unwrap();
        assert!(matches!(
            ProjectContext::open(&layout_root),
            Err(ProjectOpenError::UnsupportedLayoutVersion { version: 3 })
        ));

        fs::remove_dir_all(manifest_root).unwrap();
        fs::remove_dir_all(layout_root).unwrap();
    }

    #[test]
    fn project_open_rejects_unknown_manifest_and_layout_fields() {
        let (model, layout) = json_project();

        let manifest_root = temporary_root("unknown-manifest-field");
        write_json_project(&manifest_root, &model, &layout);
        let manifest_path = manifest_root.join(PROJECT_MANIFEST_PATH);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::Value::Bool(true));
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(matches!(
            ProjectContext::open(&manifest_root),
            Err(ProjectOpenError::InvalidManifest { .. })
        ));

        let layout_root = temporary_root("unknown-layout-field");
        write_json_project(&layout_root, &model, &layout);
        let manifest_path = layout_root.join(PROJECT_MANIFEST_PATH);
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["layout"]
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::Value::Bool(true));
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(matches!(
            ProjectContext::open(&layout_root),
            Err(ProjectOpenError::InvalidManifest { .. })
        ));

        fs::remove_dir_all(manifest_root).unwrap();
        fs::remove_dir_all(layout_root).unwrap();
    }

    #[test]
    fn project_open_rejects_rust_builder_model_records() {
        let root = temporary_root("rust-builder");
        let layout = RustWorkspaceLayout::new(
            ProjectId::new("fixture").unwrap(),
            ModelRecord::rust_builder("model.rs", "", "fixture_model").unwrap(),
        );
        write_manifest(&root, &layout);

        assert!(matches!(
            ProjectContext::open(&root),
            Err(ProjectOpenError::UnsupportedRustBuilderModel { path }) if path == "model.rs"
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_open_requires_the_exact_canonical_model_projection() {
        let root = temporary_root("noncanonical-model");
        let (model, layout) = json_project();
        write_manifest(&root, &layout);
        fs::write(
            root.join(layout.model_record().path()),
            serde_json::to_vec(&model).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            ProjectContext::open(&root),
            Err(ProjectOpenError::NonCanonicalModelRecord { .. })
        ));

        let mut extended = serde_json::to_value(&model).unwrap();
        extended
            .as_object_mut()
            .unwrap()
            .insert("future".to_string(), serde_json::Value::Bool(true));
        let mut extended = serde_json::to_string_pretty(&extended).unwrap();
        extended.push('\n');
        fs::write(root.join(layout.model_record().path()), extended).unwrap();
        assert!(matches!(
            ProjectContext::open(&root),
            Err(ProjectOpenError::NonCanonicalModelRecord { .. })
        ));

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_open_rejects_malformed_and_oversized_files() {
        let malformed_root = temporary_root("malformed-model");
        let (_, layout) = json_project();
        write_manifest(&malformed_root, &layout);
        fs::write(malformed_root.join(layout.model_record().path()), b"{").unwrap();
        assert!(matches!(
            ProjectContext::open(&malformed_root),
            Err(ProjectOpenError::InvalidModelRecord { .. })
        ));

        let manifest_root = temporary_root("large-manifest");
        fs::File::create(manifest_root.join(PROJECT_MANIFEST_PATH))
            .unwrap()
            .set_len(MAX_PROJECT_MANIFEST_BYTES + 1)
            .unwrap();
        assert!(matches!(
            ProjectContext::open(&manifest_root),
            Err(ProjectOpenError::FileTooLarge { maximum, .. })
                if maximum == MAX_PROJECT_MANIFEST_BYTES
        ));

        let model_root = temporary_root("large-model");
        write_manifest(&model_root, &layout);
        fs::File::create(model_root.join(layout.model_record().path()))
            .unwrap()
            .set_len(MAX_PROJECT_MODEL_BYTES + 1)
            .unwrap();
        assert!(matches!(
            ProjectContext::open(&model_root),
            Err(ProjectOpenError::FileTooLarge { maximum, .. })
                if maximum == MAX_PROJECT_MODEL_BYTES
        ));

        fs::remove_dir_all(malformed_root).unwrap();
        fs::remove_dir_all(manifest_root).unwrap();
        fs::remove_dir_all(model_root).unwrap();
    }

    #[test]
    fn project_open_rejects_non_regular_manifest_and_model_files() {
        let manifest_root = temporary_root("directory-manifest");
        fs::create_dir(manifest_root.join(PROJECT_MANIFEST_PATH)).unwrap();
        assert!(matches!(
            ProjectContext::open(&manifest_root),
            Err(ProjectOpenError::NotRegularFile { .. })
        ));

        let model_root = temporary_root("directory-model");
        let (_, layout) = json_project();
        write_manifest(&model_root, &layout);
        fs::create_dir(model_root.join(layout.model_record().path())).unwrap();
        assert!(matches!(
            ProjectContext::open(&model_root),
            Err(ProjectOpenError::NotRegularFile { .. })
        ));

        fs::remove_dir_all(manifest_root).unwrap();
        fs::remove_dir_all(model_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_open_rejects_hardlinked_manifest_and_model_files() {
        let manifest_root = temporary_root("hardlinked-manifest");
        let (model, layout) = json_project();
        let manifest_source = manifest_root.join("manifest-source.json");
        fs::write(
            &manifest_source,
            serde_json::to_vec(&ProjectManifest::new(layout.clone())).unwrap(),
        )
        .unwrap();
        fs::hard_link(&manifest_source, manifest_root.join(PROJECT_MANIFEST_PATH)).unwrap();
        assert!(matches!(
            ProjectContext::open(&manifest_root),
            Err(ProjectOpenError::HardlinkedFile { links: 2, .. })
        ));

        let model_root = temporary_root("hardlinked-model");
        write_manifest(&model_root, &layout);
        let model_source = model_root.join("model-source.json");
        fs::write(&model_source, canonical_model(&layout, &model)).unwrap();
        fs::hard_link(&model_source, model_root.join(layout.model_record().path())).unwrap();
        assert!(matches!(
            ProjectContext::open(&model_root),
            Err(ProjectOpenError::HardlinkedFile { links: 2, .. })
        ));

        fs::remove_dir_all(manifest_root).unwrap();
        fs::remove_dir_all(model_root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_open_rejects_symlinks_that_escape_the_root() {
        use std::os::unix::fs::symlink;

        let outside = temporary_root("outside-open");
        let (model, layout) = json_project();
        let outside_manifest = outside.join("manifest.json");
        fs::write(
            &outside_manifest,
            serde_json::to_vec(&ProjectManifest::new(layout.clone())).unwrap(),
        )
        .unwrap();
        let manifest_root = temporary_root("escaping-manifest");
        symlink(&outside_manifest, manifest_root.join(PROJECT_MANIFEST_PATH)).unwrap();
        assert!(matches!(
            ProjectContext::open(&manifest_root),
            Err(ProjectOpenError::EscapingFile { .. })
        ));

        let outside_model = outside.join("model.json");
        fs::write(&outside_model, canonical_model(&layout, &model)).unwrap();
        let model_root = temporary_root("escaping-model");
        write_manifest(&model_root, &layout);
        symlink(
            &outside_model,
            model_root.join(layout.model_record().path()),
        )
        .unwrap();
        assert!(matches!(
            ProjectContext::open(&model_root),
            Err(ProjectOpenError::EscapingFile { .. })
        ));

        fs::remove_dir_all(outside).unwrap();
        fs::remove_dir_all(manifest_root).unwrap();
        fs::remove_dir_all(model_root).unwrap();
    }

    #[test]
    fn theseus_context_is_canonical_and_projected() {
        let project = theseus_project().expect("Theseus project context is valid");
        assert!(project.root().is_absolute());
        assert_eq!(project.layout().project_id().as_str(), "theseus");
        assert!(
            project
                .generated_files(project.initial_model())
                .unwrap()
                .iter()
                .any(|file| file.path == theseus_model::SELF_MODEL_PATH)
        );
    }

    #[test]
    fn path_resolution_rejects_parent_and_metadata_access() {
        let project = theseus_project().expect("Theseus project context is valid");
        assert!(project.resolve_existing("../").is_err());
        assert!(project.resolve_existing(".theseus").is_err());
        assert_eq!(project.resolve_existing("").unwrap(), project.root());
    }

    #[test]
    fn same_layout_on_another_root_is_not_the_same_project() {
        let first = temporary_root("first");
        let second = temporary_root("second");
        let layout = theseus_model::project_layout().unwrap();
        let expected =
            ProjectContext::new(&first, theseus_model::theseus_model(), layout.clone()).unwrap();
        let actual = ProjectContext::new(&second, theseus_model::theseus_model(), layout).unwrap();

        assert!(matches!(
            expected.ensure_same_project(&actual),
            Err(ProjectBindingError::RootMismatch { .. })
        ));
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[tokio::test]
    async fn a_session_rejects_adapters_bound_to_another_clone() {
        let first = temporary_root("session-first");
        let second = temporary_root("session-second");
        let layout = theseus_model::project_layout().unwrap();
        let project =
            ProjectContext::new(&first, theseus_model::theseus_model(), layout.clone()).unwrap();
        let other = ProjectContext::new(&second, theseus_model::theseus_model(), layout).unwrap();
        let workspace = crate::FsWorkspace::for_project(&other);
        let checkpoint = crate::GitCheckpoint::for_project(other.clone());
        let toolchain = crate::CargoToolchain::for_project(&other);
        let mut session = crate::Session::new(
            project,
            &workspace,
            &checkpoint,
            &theseus_calculator::Calculator,
            &toolchain,
            false,
        );

        let error = session
            .call("check", &serde_json::json!({}))
            .await
            .expect_err("the mismatched adapter root is rejected before Cargo runs");
        assert!(matches!(
            error.downcast_ref::<ProjectBindingError>(),
            Some(ProjectBindingError::RootMismatch { .. })
        ));
        for (operation, input) in [
            ("generate", serde_json::json!({})),
            ("snapshot", serde_json::json!({ "label": "wrong root" })),
        ] {
            let error = session
                .call(operation, &input)
                .await
                .expect_err("every project-bound adapter rejects the other clone");
            assert!(matches!(
                error.downcast_ref::<ProjectBindingError>(),
                Some(ProjectBindingError::RootMismatch { .. })
            ));
        }
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_selected_root_rejects_symlink_replacement() {
        use std::os::unix::fs::symlink;

        let root = temporary_root("selected");
        let moved = root.with_extension("moved");
        let project = ProjectContext::new(
            &root,
            theseus_model::theseus_model(),
            theseus_model::project_layout().unwrap(),
        )
        .unwrap();
        fs::rename(&root, &moved).unwrap();
        symlink(&moved, &root).unwrap();

        assert!(matches!(
            project.validate_root(),
            Err(ProjectRootError::Changed { .. })
        ));
        fs::remove_file(root).unwrap();
        fs::remove_dir_all(moved).unwrap();
    }
}
