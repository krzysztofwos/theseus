//! Immutable, operator-selected project identity and projection policy.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use theseus_modeling::{
    CheckpointProjectDescriptor, GeneratedFile, Inbound, Model, ProjectLayoutError,
    RustWorkspaceLayout, Service,
};
use thiserror::Error;

use crate::{ExpectedFile, ExpectedFileSet, MutationError, Project, validate_workspace_paths};

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
