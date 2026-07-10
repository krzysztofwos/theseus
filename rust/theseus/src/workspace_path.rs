use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
};

use thiserror::Error;

/// A normalized path proven to stay relative to a workspace root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct WorkspacePath {
    path: PathBuf,
    components: Vec<OsString>,
}

impl WorkspacePath {
    pub(crate) fn as_path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn components(&self) -> impl Iterator<Item = &OsString> {
        self.components.iter()
    }
}

impl TryFrom<&str> for WorkspacePath {
    type Error = WorkspacePathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let path = Path::new(value);
        let components: Vec<OsString> = path
            .components()
            .map(|component| match component {
                Component::Normal(value) => Ok(value.to_os_string()),
                _ => Err(WorkspacePathError::Invalid {
                    path: value.to_string(),
                }),
            })
            .collect::<Result<_, _>>()?;
        let normalized: PathBuf = components.iter().collect();
        if components.is_empty()
            || normalized != path
            || value.contains("//")
            || value.ends_with('/')
            || value.ends_with('\\')
        {
            return Err(WorkspacePathError::Invalid {
                path: value.to_string(),
            });
        }
        Ok(Self {
            path: normalized,
            components,
        })
    }
}

/// A generated-file path that cannot be resolved without leaving the workspace
/// or following a symlink.
#[derive(Debug, Error)]
pub(crate) enum WorkspacePathError {
    #[error("workspace path {path:?} must be a non-empty normalized relative path")]
    Invalid { path: String },
    #[error("workspace path {path:?} crosses symbolic link {link}")]
    Symlink { path: String, link: PathBuf },
    #[error("workspace path {path:?} crosses non-directory component {component}")]
    ParentNotDirectory { path: String, component: PathBuf },
    #[error("resolving workspace path {path:?} at {component}")]
    Io {
        path: String,
        component: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_normal_relative_paths_are_accepted() {
        let accepted = WorkspacePath::try_from("rust/model/src/generated.rs")
            .expect("a normalized relative path is accepted");
        assert_eq!(accepted.as_path(), Path::new("rust/model/src/generated.rs"));

        for path in [
            "",
            ".",
            "..",
            "../outside",
            "rust/../outside",
            "/tmp/outside",
            "rust//model",
            "rust/model/",
        ] {
            assert!(
                WorkspacePath::try_from(path).is_err(),
                "unsafe path was accepted: {path:?}"
            );
        }
    }
}
