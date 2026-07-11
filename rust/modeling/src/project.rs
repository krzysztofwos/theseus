//! Versioned, data-driven layout for a modeled Rust workspace.
//!
//! The model describes architecture. A project layout describes where that
//! architecture is projected. Each policy version fixes the Rust workspace
//! conventions and varies only the project identity and canonical model record,
//! so checkpoint ownership can be reconstructed from stable data.

use std::{collections::BTreeSet, fmt, path::Component, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use thiserror::Error;

use crate::{
    GeneratedFile, Inbound, Model, RenderError, Service, Transport,
    codegen::validate_render_inputs, render_model_source, render_module_for_crate, render_proto,
    scaffold_files,
};

const RUST_WORKSPACE_LAYOUT_VERSION_V1: u32 = 1;
const RUST_WORKSPACE_LAYOUT_VERSION: u32 = 2;
const MAX_PROJECT_ID_BYTES: usize = 64;
const MAX_LAYOUT_PATH_BYTES: usize = 4_096;
const MAX_MODEL_HEADER_BYTES: usize = 64 * 1_024;
const MAX_MODEL_FUNCTION_BYTES: usize = 256;
const CARGO_LOCK_PATH: &str = "Cargo.lock";
const ROOT_MANIFEST_PATH: &str = "Cargo.toml";
const PROJECT_MANIFEST_PATH: &str = "theseus.json";

/// A stable project identifier safe as one private-ref namespace component.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProjectId(String);

impl ProjectId {
    /// Parse a lowercase ASCII identifier containing letters, digits, and
    /// interior hyphens.
    pub fn new(value: impl Into<String>) -> Result<Self, ProjectIdError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ProjectIdError::Empty);
        }
        if value.len() > MAX_PROJECT_ID_BYTES {
            return Err(ProjectIdError::TooLong {
                length: value.len(),
                maximum: MAX_PROJECT_ID_BYTES,
            });
        }
        let bytes = value.as_bytes();
        if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
            return Err(ProjectIdError::InvalidBoundary);
        }
        if !bytes[bytes.len() - 1].is_ascii_lowercase() && !bytes[bytes.len() - 1].is_ascii_digit()
        {
            return Err(ProjectIdError::InvalidBoundary);
        }
        if let Some((index, byte)) =
            bytes.iter().copied().enumerate().find(|(_, byte)| {
                !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'-'
            })
        {
            return Err(ProjectIdError::InvalidByte { index, byte });
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ProjectId {
    type Err = ProjectIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl TryFrom<String> for ProjectId {
    type Error = ProjectIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl Serialize for ProjectId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProjectId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(D::Error::custom)
    }
}

/// A project identifier that cannot safely name a project namespace.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ProjectIdError {
    #[error("project id must not be empty")]
    Empty,
    #[error("project id is {length} bytes; the maximum is {maximum}")]
    TooLong { length: usize, maximum: usize },
    #[error("project id must begin and end with a lowercase ASCII letter or digit")]
    InvalidBoundary,
    #[error(
        "project id byte 0x{byte:02x} at offset {index} is not lowercase ASCII, a digit, or a hyphen"
    )]
    InvalidByte { index: usize, byte: u8 },
}

/// The canonical on-disk representation of a project's model of record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelRecord {
    /// Canonical Rust builder source rendered through [`render_model_source`].
    RustBuilder(RustBuilderModelRecord),
    /// Canonical pretty JSON with one trailing newline.
    Json(JsonModelRecord),
}

impl ModelRecord {
    pub fn rust_builder(
        path: impl Into<String>,
        header: impl Into<String>,
        function: impl Into<String>,
    ) -> Result<Self, ProjectLayoutError> {
        Ok(Self::RustBuilder(RustBuilderModelRecord::new(
            path, header, function,
        )?))
    }

    pub fn json(path: impl Into<String>) -> Result<Self, ProjectLayoutError> {
        Ok(Self::Json(JsonModelRecord::new(path)?))
    }

    pub fn path(&self) -> &str {
        match self {
            Self::RustBuilder(record) => &record.path,
            Self::Json(record) => &record.path,
        }
    }

    fn render(&self, model: &Model) -> Result<String, ProjectLayoutError> {
        match self {
            Self::RustBuilder(record) => Ok(render_model_source(
                model,
                &record.header,
                &record.function,
            )?),
            Self::Json(_) => {
                validate_render_inputs(model)?;
                let mut source = serde_json::to_string_pretty(model)
                    .map_err(|source| ProjectLayoutError::SerializeModel { source })?;
                source.push('\n');
                Ok(source)
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustBuilderModelRecord {
    path: String,
    header: String,
    function: String,
}

impl RustBuilderModelRecord {
    fn new(
        path: impl Into<String>,
        header: impl Into<String>,
        function: impl Into<String>,
    ) -> Result<Self, ProjectLayoutError> {
        let path = path.into();
        validate_model_record_path(&path, "rs")?;
        let header = header.into();
        if header.len() > MAX_MODEL_HEADER_BYTES {
            return Err(ProjectLayoutError::HeaderTooLong {
                length: header.len(),
                maximum: MAX_MODEL_HEADER_BYTES,
            });
        }
        if !header.is_empty() && !header.ends_with('\n') {
            return Err(ProjectLayoutError::InvalidHeader);
        }
        if header.contains('\r')
            || header
                .lines()
                .any(|line| !line.is_empty() && !line.starts_with("//"))
        {
            return Err(ProjectLayoutError::InvalidHeader);
        }
        let function = function.into();
        if function.len() > MAX_MODEL_FUNCTION_BYTES {
            return Err(ProjectLayoutError::FunctionTooLong {
                length: function.len(),
                maximum: MAX_MODEL_FUNCTION_BYTES,
            });
        }
        syn::parse_str::<syn::Ident>(&function).map_err(|source| {
            ProjectLayoutError::InvalidModelFunction {
                function: function.clone(),
                message: source.to_string(),
            }
        })?;
        Ok(Self {
            path,
            header,
            function,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn header(&self) -> &str {
        &self.header
    }

    pub fn function(&self) -> &str {
        &self.function
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsonModelRecord {
    path: String,
}

impl JsonModelRecord {
    fn new(path: impl Into<String>) -> Result<Self, ProjectLayoutError> {
        let path = path.into();
        validate_model_record_path(&path, "json")?;
        Ok(Self { path })
    }

    pub fn path(&self) -> &str {
        &self.path
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case", deny_unknown_fields)]
enum ModelRecordWire {
    RustBuilder {
        path: String,
        header: String,
        function: String,
    },
    Json {
        path: String,
    },
}

impl From<&ModelRecord> for ModelRecordWire {
    fn from(record: &ModelRecord) -> Self {
        match record {
            ModelRecord::RustBuilder(record) => Self::RustBuilder {
                path: record.path.clone(),
                header: record.header.clone(),
                function: record.function.clone(),
            },
            ModelRecord::Json(record) => Self::Json {
                path: record.path.clone(),
            },
        }
    }
}

impl TryFrom<ModelRecordWire> for ModelRecord {
    type Error = ProjectLayoutError;

    fn try_from(record: ModelRecordWire) -> Result<Self, Self::Error> {
        match record {
            ModelRecordWire::RustBuilder {
                path,
                header,
                function,
            } => Self::rust_builder(path, header, function),
            ModelRecordWire::Json { path } => Self::json(path),
        }
    }
}

impl Serialize for ModelRecord {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ModelRecordWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ModelRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ModelRecordWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(D::Error::custom)
    }
}

/// Current Rust/Cargo workspace policy for one project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustWorkspaceLayout {
    project_id: ProjectId,
    model_record: ModelRecord,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RustWorkspaceLayoutWire {
    version: u32,
    project_id: ProjectId,
    model_record: ModelRecord,
}

impl From<&RustWorkspaceLayout> for RustWorkspaceLayoutWire {
    fn from(layout: &RustWorkspaceLayout) -> Self {
        Self {
            version: RustWorkspaceLayout::VERSION,
            project_id: layout.project_id.clone(),
            model_record: layout.model_record.clone(),
        }
    }
}

impl TryFrom<RustWorkspaceLayoutWire> for RustWorkspaceLayout {
    type Error = ProjectLayoutError;

    fn try_from(layout: RustWorkspaceLayoutWire) -> Result<Self, Self::Error> {
        if !RustWorkspaceLayout::supports_version(layout.version) {
            return Err(ProjectLayoutError::UnsupportedLayoutVersion {
                version: layout.version,
            });
        }
        Ok(Self::new(layout.project_id, layout.model_record))
    }
}

impl Serialize for RustWorkspaceLayout {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        RustWorkspaceLayoutWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RustWorkspaceLayout {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::try_from(RustWorkspaceLayoutWire::deserialize(deserializer)?)
            .map_err(D::Error::custom)
    }
}

impl RustWorkspaceLayout {
    pub const VERSION: u32 = RUST_WORKSPACE_LAYOUT_VERSION;

    /// Whether a serialized layout version can be migrated to the current policy.
    pub fn supports_version(version: u32) -> bool {
        matches!(version, RUST_WORKSPACE_LAYOUT_VERSION_V1 | Self::VERSION)
    }

    pub fn new(project_id: ProjectId, model_record: ModelRecord) -> Self {
        Self {
            project_id,
            model_record,
        }
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn model_record(&self) -> &ModelRecord {
        &self.model_record
    }

    /// The frozen data embedded in a checkpoint manifest.
    pub fn checkpoint_descriptor(&self) -> CheckpointProjectDescriptor {
        CheckpointProjectDescriptor {
            version: Self::VERSION,
            project_id: self.project_id.clone(),
            model_record: self.model_record.clone(),
        }
    }

    /// Every file projected from `model`, including its model of record.
    pub fn generated_files(&self, model: &Model) -> Result<Vec<GeneratedFile>, ProjectLayoutError> {
        validate_render_inputs(model)?;
        let mut files = rendered_contract_files_v1(model)?;
        let model_record = GeneratedFile {
            path: self.model_record.path().to_string(),
            contents: self.model_record.render(model)?,
        };
        if files.iter().any(|file| file.path == model_record.path) {
            return Err(ProjectLayoutError::PathCollision {
                path: model_record.path,
            });
        }
        files.push(model_record);
        reject_model_record_lifecycle_collision_v2(model, self.model_record.path())?;
        validate_generated_paths(&files)?;
        Ok(files)
    }

    pub fn authored_impl_path(
        &self,
        model: &Model,
        service: &Service,
    ) -> Result<String, ProjectLayoutError> {
        let path = format!(
            "rust/{}/src/service.rs",
            crate_dir(model, &service.crate_name)?
        );
        validate_layout_path(&path)?;
        Ok(path)
    }

    pub fn adapter_impl_path(
        &self,
        model: &Model,
        service: &Service,
    ) -> Result<String, ProjectLayoutError> {
        let path = format!("rust/{}/src/lib.rs", crate_dir(model, &service.crate_name)?);
        validate_layout_path(&path)?;
        Ok(path)
    }

    pub fn inbound_adapter_impl_path(
        &self,
        model: &Model,
        inbound: &Inbound,
    ) -> Result<String, ProjectLayoutError> {
        let path = format!(
            "rust/{}/src/adapters.rs",
            crate_dir(model, &inbound.crate_name)?
        );
        validate_layout_path(&path)?;
        Ok(path)
    }

    pub fn authored_impls(
        &self,
        model: &Model,
    ) -> Result<Vec<(String, String)>, ProjectLayoutError> {
        model
            .services
            .iter()
            .map(|service| {
                Ok((
                    service.name.clone(),
                    self.authored_impl_path(model, service)?,
                ))
            })
            .collect()
    }

    pub fn interior_impls(
        &self,
        model: &Model,
    ) -> Result<Vec<(String, String)>, ProjectLayoutError> {
        model
            .inbounds
            .iter()
            .filter(|inbound| !inbound.outbound.is_empty())
            .map(|inbound| {
                Ok((
                    inbound.name.clone(),
                    self.inbound_adapter_impl_path(model, inbound)?,
                ))
            })
            .collect()
    }

    /// Every modeled Rust source whose contents are an authored project leaf.
    ///
    /// The current policy derives this allowlist from exact model ownership, then
    /// removes generated projections and the canonical model record. It is
    /// independent of which files currently exist on disk.
    pub fn authored_rust_paths(&self, model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
        let mut excluded: BTreeSet<String> = generated_paths_v1(model)?.into_iter().collect();
        excluded.insert(self.model_record.path().to_string());
        Ok(self
            .owned_paths(model)?
            .into_iter()
            .filter(|path| path.ends_with(".rs") && !excluded.contains(path))
            .collect())
    }

    /// Authorize one exact project-relative Rust source for an authored edit.
    pub fn authorize_authored_rust_path(
        &self,
        model: &Model,
        path: &str,
    ) -> Result<(), ProjectLayoutError> {
        validate_layout_path(path)?;
        if !self
            .authored_rust_paths(model)?
            .iter()
            .any(|candidate| candidate == path)
        {
            return Err(ProjectLayoutError::AuthoredRustPathNotAuthorized {
                path: path.to_string(),
            });
        }
        Ok(())
    }

    /// Every exact project-relative path owned by the model-driven workflow.
    pub fn owned_paths(&self, model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
        self.checkpoint_descriptor().owned_paths(model)
    }
}

/// Stable versioned layout data stored with a checkpoint.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CheckpointProjectDescriptor {
    version: u32,
    project_id: ProjectId,
    model_record: ModelRecord,
}

impl CheckpointProjectDescriptor {
    pub fn version(&self) -> u32 {
        self.version
    }

    pub fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub fn model_record(&self) -> &ModelRecord {
        &self.model_record
    }

    /// Reconstruct the descriptor's frozen ownership without ambient state.
    pub fn owned_paths(&self, model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
        match self.version {
            RUST_WORKSPACE_LAYOUT_VERSION_V1 => owned_paths_v1(model, self.model_record.path()),
            RUST_WORKSPACE_LAYOUT_VERSION => owned_paths_v2(model, self.model_record.path()),
            version => Err(ProjectLayoutError::UnsupportedLayoutVersion { version }),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointProjectDescriptorWire {
    version: u32,
    project_id: ProjectId,
    model_record: CheckpointModelRecordV1Wire,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "format", rename_all = "kebab-case", deny_unknown_fields)]
enum CheckpointModelRecordV1Wire {
    RustBuilder {
        path: String,
        header: String,
        function: String,
    },
    Json {
        path: String,
    },
}

impl From<&ModelRecord> for CheckpointModelRecordV1Wire {
    fn from(record: &ModelRecord) -> Self {
        match record {
            ModelRecord::RustBuilder(record) => Self::RustBuilder {
                path: record.path.clone(),
                header: record.header.clone(),
                function: record.function.clone(),
            },
            ModelRecord::Json(record) => Self::Json {
                path: record.path.clone(),
            },
        }
    }
}

impl TryFrom<CheckpointModelRecordV1Wire> for ModelRecord {
    type Error = ProjectLayoutError;

    fn try_from(record: CheckpointModelRecordV1Wire) -> Result<Self, Self::Error> {
        match record {
            CheckpointModelRecordV1Wire::RustBuilder {
                path,
                header,
                function,
            } => Self::rust_builder(path, header, function),
            CheckpointModelRecordV1Wire::Json { path } => Self::json(path),
        }
    }
}

impl Serialize for CheckpointProjectDescriptor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        CheckpointProjectDescriptorWire {
            version: self.version,
            project_id: self.project_id.clone(),
            model_record: CheckpointModelRecordV1Wire::from(&self.model_record),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CheckpointProjectDescriptor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = CheckpointProjectDescriptorWire::deserialize(deserializer)?;
        if !RustWorkspaceLayout::supports_version(wire.version) {
            return Err(D::Error::custom(
                ProjectLayoutError::UnsupportedLayoutVersion {
                    version: wire.version,
                },
            ));
        }
        Ok(Self {
            version: wire.version,
            project_id: wire.project_id,
            model_record: ModelRecord::try_from(wire.model_record).map_err(D::Error::custom)?,
        })
    }
}

/// A project layout that cannot be interpreted safely.
#[derive(Debug, Error)]
pub enum ProjectLayoutError {
    #[error(transparent)]
    ProjectId(#[from] ProjectIdError),
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error("workspace path must be a non-empty normalized relative path: {path:?}")]
    InvalidPath { path: String },
    #[error("workspace path is {length} bytes; the maximum is {maximum}: {path:?}")]
    PathTooLong {
        path: String,
        length: usize,
        maximum: usize,
    },
    #[error("workspace path uses reserved project metadata: {path:?}")]
    ReservedPath { path: String },
    #[error("Rust source path is not authorized for authored edits: {path:?}")]
    AuthoredRustPathNotAuthorized { path: String },
    #[error("model record path must have the .{expected} extension: {path:?}")]
    InvalidModelRecordExtension {
        path: String,
        expected: &'static str,
    },
    #[error("model source header must contain only line comments and end in a newline")]
    InvalidHeader,
    #[error("model source header is {length} bytes; the maximum is {maximum}")]
    HeaderTooLong { length: usize, maximum: usize },
    #[error("model function is {length} bytes; the maximum is {maximum}")]
    FunctionTooLong { length: usize, maximum: usize },
    #[error("model function {function:?} is not a Rust identifier: {message}")]
    InvalidModelFunction { function: String, message: String },
    #[error("model record path collides with another project-owned path: {path:?}")]
    PathCollision { path: String },
    #[error("crate {crate_name:?} is not modeled")]
    CrateNotModeled { crate_name: String },
    #[error("serializing the canonical JSON model record")]
    SerializeModel {
        #[source]
        source: serde_json::Error,
    },
    #[error("checkpoint project layout has unsupported version {version}")]
    UnsupportedLayoutVersion { version: u32 },
}

fn rendered_contract_files_v1(model: &Model) -> Result<Vec<GeneratedFile>, ProjectLayoutError> {
    let mut files = Vec::new();
    let mut rendered = BTreeSet::new();
    let hosting = model
        .services
        .iter()
        .map(|service| service.crate_name.as_str())
        .chain(
            model
                .inbounds
                .iter()
                .filter(|inbound| {
                    matches!(
                        inbound.transport,
                        Transport::Cli | Transport::Http | Transport::Grpc
                    ) || !inbound.outbound.is_empty()
                        || inbound.turns.is_some()
                })
                .map(|inbound| inbound.crate_name.as_str()),
        )
        .chain(
            model
                .clients
                .iter()
                .map(|client| client.crate_name.as_str()),
        );
    for crate_name in hosting {
        if !rendered.insert(crate_name) {
            continue;
        }
        let dir = crate_dir(model, crate_name)?;
        files.push(GeneratedFile {
            path: format!("rust/{dir}/src/generated.rs"),
            contents: render_module_for_crate(model, crate_name)?,
        });
    }

    let grpc_hosts = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.transport == Transport::Grpc)
        .map(|inbound| (inbound.crate_name.as_str(), inbound.service.as_str()))
        .chain(
            model
                .clients
                .iter()
                .filter(|client| client.transport == Transport::Grpc)
                .map(|client| (client.crate_name.as_str(), client.service.as_str())),
        );
    for (crate_name, service_name) in grpc_hosts {
        let service = model.service_named(service_name).ok_or_else(|| {
            ProjectLayoutError::Render(RenderError::ServiceNotModeled {
                service: service_name.to_string(),
            })
        })?;
        let dir = crate_dir(model, crate_name)?;
        files.push(GeneratedFile {
            path: format!("rust/{dir}/proto/{}.proto", service.name.to_lowercase()),
            contents: render_proto(model, service)?,
        });
    }
    Ok(files)
}

fn owned_paths_v1(
    model: &Model,
    model_record_path: &str,
) -> Result<Vec<String>, ProjectLayoutError> {
    validate_render_inputs(model)?;
    validate_layout_path(model_record_path)?;
    reject_model_record_lifecycle_collision_v1(model, model_record_path)?;

    let mut paths = BTreeSet::new();
    for path in generated_paths_v1(model)? {
        insert_owned_path(&mut paths, path)?;
    }
    insert_owned_path(&mut paths, model_record_path.to_string())?;
    for file in scaffold_files(model) {
        insert_owned_path(&mut paths, file.path)?;
    }
    for service in &model.services {
        let dir = crate_dir(model, &service.crate_name)?;
        insert_owned_path(&mut paths, format!("rust/{dir}/src/service.rs"))?;
        insert_owned_path(&mut paths, format!("rust/{dir}/src/lib.rs"))?;
    }
    for inbound in model
        .inbounds
        .iter()
        .filter(|inbound| !inbound.outbound.is_empty())
    {
        let dir = crate_dir(model, &inbound.crate_name)?;
        insert_owned_path(&mut paths, format!("rust/{dir}/src/adapters.rs"))?;
    }
    insert_owned_path(&mut paths, CARGO_LOCK_PATH.to_string())?;
    Ok(paths.into_iter().collect())
}

fn owned_paths_v2(
    model: &Model,
    model_record_path: &str,
) -> Result<Vec<String>, ProjectLayoutError> {
    reject_model_record_lifecycle_collision_v2(model, model_record_path)?;
    let mut paths: BTreeSet<String> = owned_paths_v1(model, model_record_path)?
        .into_iter()
        .collect();
    insert_owned_path(&mut paths, ROOT_MANIFEST_PATH.to_string())?;
    insert_owned_path(&mut paths, PROJECT_MANIFEST_PATH.to_string())?;
    Ok(paths.into_iter().collect())
}

fn generated_paths_v1(model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
    let mut paths = Vec::new();
    let mut rendered = BTreeSet::new();
    let hosting = model
        .services
        .iter()
        .map(|service| service.crate_name.as_str())
        .chain(
            model
                .inbounds
                .iter()
                .filter(|inbound| {
                    matches!(
                        inbound.transport,
                        Transport::Cli | Transport::Http | Transport::Grpc
                    ) || !inbound.outbound.is_empty()
                        || inbound.turns.is_some()
                })
                .map(|inbound| inbound.crate_name.as_str()),
        )
        .chain(
            model
                .clients
                .iter()
                .map(|client| client.crate_name.as_str()),
        );
    for crate_name in hosting {
        if rendered.insert(crate_name) {
            paths.push(format!(
                "rust/{}/src/generated.rs",
                crate_dir(model, crate_name)?
            ));
        }
    }
    let grpc_hosts = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.transport == Transport::Grpc)
        .map(|inbound| (inbound.crate_name.as_str(), inbound.service.as_str()))
        .chain(
            model
                .clients
                .iter()
                .filter(|client| client.transport == Transport::Grpc)
                .map(|client| (client.crate_name.as_str(), client.service.as_str())),
        );
    for (crate_name, service_name) in grpc_hosts {
        let service = model.service_named(service_name).ok_or_else(|| {
            ProjectLayoutError::Render(RenderError::ServiceNotModeled {
                service: service_name.to_string(),
            })
        })?;
        paths.push(format!(
            "rust/{}/proto/{}.proto",
            crate_dir(model, crate_name)?,
            service.name.to_lowercase()
        ));
    }
    let mut unique = BTreeSet::new();
    if let Some(path) = paths.iter().find(|path| !unique.insert(path.as_str())) {
        return Err(ProjectLayoutError::PathCollision { path: path.clone() });
    }
    Ok(paths)
}

fn reject_model_record_lifecycle_collision_v1(
    model: &Model,
    model_record_path: &str,
) -> Result<(), ProjectLayoutError> {
    let mut paths = generated_paths_v1(model)?;
    paths.extend(scaffold_files(model).into_iter().map(|file| file.path));
    for service in &model.services {
        let dir = crate_dir(model, &service.crate_name)?;
        paths.push(format!("rust/{dir}/src/service.rs"));
        paths.push(format!("rust/{dir}/src/lib.rs"));
    }
    for inbound in model
        .inbounds
        .iter()
        .filter(|inbound| !inbound.outbound.is_empty())
    {
        paths.push(format!(
            "rust/{}/src/adapters.rs",
            crate_dir(model, &inbound.crate_name)?
        ));
    }
    paths.push(CARGO_LOCK_PATH.to_string());
    if paths.iter().any(|path| path == model_record_path) {
        return Err(ProjectLayoutError::PathCollision {
            path: model_record_path.to_string(),
        });
    }
    Ok(())
}

fn reject_model_record_lifecycle_collision_v2(
    model: &Model,
    model_record_path: &str,
) -> Result<(), ProjectLayoutError> {
    reject_model_record_lifecycle_collision_v1(model, model_record_path)?;
    if matches!(
        model_record_path,
        ROOT_MANIFEST_PATH | PROJECT_MANIFEST_PATH
    ) {
        return Err(ProjectLayoutError::PathCollision {
            path: model_record_path.to_string(),
        });
    }
    Ok(())
}

fn crate_dir<'a>(model: &'a Model, crate_name: &str) -> Result<&'a str, ProjectLayoutError> {
    let node =
        model
            .crate_named(crate_name)
            .ok_or_else(|| ProjectLayoutError::CrateNotModeled {
                crate_name: crate_name.to_string(),
            })?;
    let mut components = std::path::Path::new(&node.dir).components();
    let safe = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !node.dir.contains('/')
        && !node.dir.contains('\\');
    if !safe {
        return Err(ProjectLayoutError::Render(
            RenderError::InvalidCrateDirectory {
                crate_name: node.name.clone(),
                dir: node.dir.clone(),
            },
        ));
    }
    Ok(&node.dir)
}

fn validate_generated_paths(files: &[GeneratedFile]) -> Result<(), ProjectLayoutError> {
    let mut paths = BTreeSet::new();
    for file in files {
        validate_layout_path(&file.path)?;
        if !paths.insert(file.path.as_str()) {
            return Err(ProjectLayoutError::PathCollision {
                path: file.path.clone(),
            });
        }
    }
    Ok(())
}

fn insert_owned_path(paths: &mut BTreeSet<String>, path: String) -> Result<(), ProjectLayoutError> {
    validate_layout_path(&path)?;
    paths.insert(path);
    Ok(())
}

fn validate_model_record_path(
    path: &str,
    expected_extension: &'static str,
) -> Result<(), ProjectLayoutError> {
    validate_layout_path(path)?;
    if std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        != Some(expected_extension)
    {
        return Err(ProjectLayoutError::InvalidModelRecordExtension {
            path: path.to_string(),
            expected: expected_extension,
        });
    }
    Ok(())
}

fn validate_layout_path(path: &str) -> Result<(), ProjectLayoutError> {
    if path.len() > MAX_LAYOUT_PATH_BYTES {
        return Err(ProjectLayoutError::PathTooLong {
            path: path.to_string(),
            length: path.len(),
            maximum: MAX_LAYOUT_PATH_BYTES,
        });
    }
    let parsed = std::path::Path::new(path);
    let components: Vec<_> = parsed.components().collect();
    let normalized: std::path::PathBuf = components
        .iter()
        .map(|component| component.as_os_str())
        .collect();
    if components.is_empty()
        || components
            .iter()
            .any(|component| !matches!(component, Component::Normal(_)))
        || normalized != parsed
        || path.contains("//")
        || path.contains('\\')
        || path.contains('\0')
        || path.ends_with('/')
    {
        return Err(ProjectLayoutError::InvalidPath {
            path: path.to_string(),
        });
    }
    let first = components[0].as_os_str().to_string_lossy();
    if first.eq_ignore_ascii_case(".git") || first.eq_ignore_ascii_case(".theseus") {
        return Err(ProjectLayoutError::ReservedPath {
            path: path.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Port, Service};

    fn project_model() -> Model {
        Model::new("Fixture")
            .crate_node("fixture", "fixture", 1, &[])
            .crate_node("fixture-cli", "cli", 2, &["fixture"])
            .crate_node("fixture-grpc", "grpc", 2, &["fixture"])
            .crate_node("fixture-grpc-client", "grpc-client", 2, &["fixture"])
            .crate_node("fixture-agent", "agent", 2, &["fixture"])
            .service(
                Service::new("Fixture")
                    .crate_name("fixture")
                    .operation("ping", "Ping.", "Empty", "String"),
            )
            .inbound("fixture", Transport::Cli, "Fixture", "fixture-cli")
            .inbound("fixture-grpc", Transport::Grpc, "Fixture", "fixture-grpc")
            .inbound(
                "fixture-agent",
                Transport::Agent,
                "Fixture",
                "fixture-agent",
            )
            .turns(4)
            .inbound_port(Port::new("llm", "Completes turns."))
            .client(
                "fixture-grpc-client",
                Transport::Grpc,
                "Fixture",
                "fixture-grpc-client",
            )
    }

    fn rust_layout() -> RustWorkspaceLayout {
        RustWorkspaceLayout::new(
            ProjectId::new("fixture").unwrap(),
            ModelRecord::rust_builder(
                "rust/model/src/self_model.rs",
                "// canonical fixture model\n",
                "fixture_model",
            )
            .unwrap(),
        )
    }

    #[test]
    fn project_ids_are_safe_stable_namespace_components() {
        for accepted in ["a", "theseus", "journal-2", "7-project"] {
            let id = ProjectId::new(accepted).expect("safe id");
            assert_eq!(id.as_str(), accepted);
            assert_eq!(
                serde_json::from_str::<ProjectId>(&serde_json::to_string(&id).unwrap()).unwrap(),
                id
            );
        }
        for rejected in [
            "",
            "-journal",
            "journal-",
            "Journal",
            "journal_id",
            "a/b",
            ".",
            "two..dots",
        ] {
            assert!(
                ProjectId::new(rejected).is_err(),
                "unsafe id accepted: {rejected:?}"
            );
        }
        assert!(ProjectId::new("a".repeat(MAX_PROJECT_ID_BYTES + 1)).is_err());
        assert!(serde_json::from_str::<ProjectId>(r#""../outside""#).is_err());
    }

    #[test]
    fn model_record_paths_and_rust_functions_are_validated() {
        for path in [
            "",
            "/tmp/model.rs",
            "../model.rs",
            "rust/../model.rs",
            "rust//model.rs",
            "rust/model.rs/",
            "rust\\model.rs",
            ".git/model.rs",
            ".GIT/model.rs",
            ".theseus/model.rs",
            "model.json",
        ] {
            assert!(
                ModelRecord::rust_builder(path, "", "model").is_err(),
                "unsafe Rust model path accepted: {path:?}"
            );
        }
        assert!(ModelRecord::json("model.rs").is_err());
        assert!(ModelRecord::rust_builder("model.rs", "missing newline", "model").is_err());
        assert!(ModelRecord::rust_builder("model.rs", "fn injected() {}\n", "model").is_err());
        assert!(ModelRecord::rust_builder("model.rs", "", "not a function").is_err());

        for json in [
            r#"{"format":"json","path":"../model.json"}"#,
            r#"{"format":"rust-builder","path":"model.rs","header":"","function":"fn"}"#,
            r#"{"format":"json","path":"model.json","future":true}"#,
        ] {
            assert!(
                serde_json::from_str::<ModelRecord>(json).is_err(),
                "unsafe record parsed: {json}"
            );
        }
    }

    #[test]
    fn rust_and_json_model_records_are_canonical() {
        let model = Model::new("Record").crate_node("record", "record", 1, &[]);
        let rust = RustWorkspaceLayout::new(
            ProjectId::new("rust-record").unwrap(),
            ModelRecord::rust_builder("model.rs", "// model\n", "record_model").unwrap(),
        );
        let first = rust.generated_files(&model).unwrap();
        let second = rust.generated_files(&model).unwrap();
        assert_eq!(first, second);
        let source = &first
            .iter()
            .find(|file| file.path == "model.rs")
            .unwrap()
            .contents;
        assert!(
            source.starts_with("// model\nuse theseus_modeling::Model;"),
            "{source}"
        );
        assert!(
            source.contains("pub fn record_model() -> Model"),
            "{source}"
        );
        syn::parse_file(source).expect("canonical Rust model parses");

        let json = RustWorkspaceLayout::new(
            ProjectId::new("json-record").unwrap(),
            ModelRecord::json("model.json").unwrap(),
        );
        let source = json
            .generated_files(&model)
            .unwrap()
            .into_iter()
            .find(|file| file.path == "model.json")
            .unwrap()
            .contents;
        assert!(source.ends_with('\n'));
        assert_eq!(serde_json::from_str::<Model>(&source).unwrap(), model);
        let descriptor = json.checkpoint_descriptor();
        assert_eq!(
            serde_json::from_str::<CheckpointProjectDescriptor>(
                &serde_json::to_string(&descriptor).unwrap()
            )
            .unwrap(),
            descriptor
        );
    }

    #[test]
    fn rust_workspace_layout_has_a_strict_versioned_schema() {
        let layout = RustWorkspaceLayout::new(
            ProjectId::new("fixture").unwrap(),
            ModelRecord::json("model.json").unwrap(),
        );
        let encoded = serde_json::to_string(&layout).unwrap();
        assert_eq!(
            encoded,
            r#"{"version":2,"project_id":"fixture","model_record":{"format":"json","path":"model.json"}}"#
        );
        assert_eq!(
            serde_json::from_str::<RustWorkspaceLayout>(&encoded).unwrap(),
            layout
        );

        let legacy = encoded.replacen("\"version\":2", "\"version\":1", 1);
        let migrated = serde_json::from_str::<RustWorkspaceLayout>(&legacy).unwrap();
        assert_eq!(migrated, layout);
        assert_eq!(serde_json::to_string(&migrated).unwrap(), encoded);

        let future = encoded.replacen("\"version\":2", "\"version\":3", 1);
        assert!(serde_json::from_str::<RustWorkspaceLayout>(&future).is_err());
        let unknown = encoded.replacen("\"version\":2", "\"version\":2,\"future\":true", 1);
        assert!(serde_json::from_str::<RustWorkspaceLayout>(&unknown).is_err());
    }

    #[test]
    fn projections_cover_every_host_and_proto() {
        let files = rust_layout().generated_files(&project_model()).unwrap();
        let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "rust/fixture/src/generated.rs",
                "rust/cli/src/generated.rs",
                "rust/grpc/src/generated.rs",
                "rust/agent/src/generated.rs",
                "rust/grpc-client/src/generated.rs",
                "rust/grpc/proto/fixture.proto",
                "rust/grpc-client/proto/fixture.proto",
                "rust/model/src/self_model.rs",
            ]
        );
    }

    #[test]
    fn authored_adapter_and_interior_paths_are_structured() {
        let model = project_model();
        let layout = rust_layout();
        let service = model.service_named("Fixture").unwrap();
        let inbound = model
            .inbounds
            .iter()
            .find(|inbound| inbound.name == "fixture-agent")
            .unwrap();
        assert_eq!(
            layout.authored_impl_path(&model, service).unwrap(),
            "rust/fixture/src/service.rs"
        );
        assert_eq!(
            layout.adapter_impl_path(&model, service).unwrap(),
            "rust/fixture/src/lib.rs"
        );
        assert_eq!(
            layout.inbound_adapter_impl_path(&model, inbound).unwrap(),
            "rust/agent/src/adapters.rs"
        );
        assert_eq!(
            layout.authored_impls(&model).unwrap(),
            [(
                "Fixture".to_string(),
                "rust/fixture/src/service.rs".to_string()
            )]
        );
        assert_eq!(
            layout.interior_impls(&model).unwrap(),
            [(
                "fixture-agent".to_string(),
                "rust/agent/src/adapters.rs".to_string()
            )]
        );

        let missing = Service::new("Missing").crate_name("missing");
        assert!(matches!(
            layout.authored_impl_path(&model, &missing),
            Err(ProjectLayoutError::CrateNotModeled { .. })
        ));
    }

    #[test]
    fn authored_rust_paths_are_exact_model_owned_leaves() {
        let model = project_model();
        let layout = rust_layout();
        assert_eq!(
            layout.authored_rust_paths(&model).unwrap(),
            [
                "rust/agent/src/adapters.rs",
                "rust/agent/src/main.rs",
                "rust/cli/src/main.rs",
                "rust/fixture/src/lib.rs",
                "rust/fixture/src/service.rs",
                "rust/grpc/src/main.rs",
            ]
        );

        for path in [
            "rust/fixture/src/service.rs",
            "rust/fixture/src/lib.rs",
            "rust/cli/src/main.rs",
            "rust/agent/src/adapters.rs",
        ] {
            layout
                .authorize_authored_rust_path(&model, path)
                .unwrap_or_else(|error| panic!("authored path {path:?} was rejected: {error}"));
        }
    }

    #[test]
    fn authored_rust_authorization_rejects_unauthorized_surfaces() {
        let model = project_model();
        let layout = rust_layout();
        for path in [
            "rust/fixture/src/generated.rs",
            "rust/model/src/self_model.rs",
            ".theseus/session.rs",
            "rust/foreign/src/lib.rs",
            "rust/fixture/tests/integration.rs",
        ] {
            assert!(
                layout.authorize_authored_rust_path(&model, path).is_err(),
                "unauthorized Rust path was accepted: {path:?}"
            );
        }
        assert!(matches!(
            layout.authorize_authored_rust_path(&model, "rust/foreign/src/lib.rs"),
            Err(ProjectLayoutError::AuthoredRustPathNotAuthorized { .. })
        ));
        assert!(matches!(
            layout.authorize_authored_rust_path(&model, ".theseus/session.rs"),
            Err(ProjectLayoutError::ReservedPath { .. })
        ));
    }

    #[test]
    fn checkpoint_descriptor_freezes_exact_ownership() {
        let model = project_model();
        let layout = rust_layout();
        let descriptor = layout.checkpoint_descriptor();
        let encoded = serde_json::to_string(&descriptor).unwrap();
        assert_eq!(
            encoded,
            r#"{"version":2,"project_id":"fixture","model_record":{"format":"rust-builder","path":"rust/model/src/self_model.rs","header":"// canonical fixture model\n","function":"fixture_model"}}"#
        );
        let restored: CheckpointProjectDescriptor = serde_json::from_str(&encoded).unwrap();
        assert_eq!(restored, descriptor);
        assert_eq!(
            restored.owned_paths(&model).unwrap(),
            layout.owned_paths(&model).unwrap()
        );
        assert_eq!(
            restored.owned_paths(&model).unwrap(),
            [
                "Cargo.lock",
                "Cargo.toml",
                "rust/agent/Cargo.toml",
                "rust/agent/src/adapters.rs",
                "rust/agent/src/generated.rs",
                "rust/agent/src/main.rs",
                "rust/cli/Cargo.toml",
                "rust/cli/src/generated.rs",
                "rust/cli/src/main.rs",
                "rust/fixture/Cargo.toml",
                "rust/fixture/src/generated.rs",
                "rust/fixture/src/lib.rs",
                "rust/fixture/src/service.rs",
                "rust/grpc-client/proto/fixture.proto",
                "rust/grpc-client/src/generated.rs",
                "rust/grpc/Cargo.toml",
                "rust/grpc/proto/fixture.proto",
                "rust/grpc/src/generated.rs",
                "rust/grpc/src/main.rs",
                "rust/model/src/self_model.rs",
                "theseus.json",
            ]
        );

        let future = encoded.replacen("\"version\":2", "\"version\":3", 1);
        assert!(serde_json::from_str::<CheckpointProjectDescriptor>(&future).is_err());
        let unknown = encoded.replacen("\"version\":2", "\"version\":2,\"future\":true", 1);
        assert!(serde_json::from_str::<CheckpointProjectDescriptor>(&unknown).is_err());
    }

    #[test]
    fn version_one_checkpoint_descriptor_retains_legacy_ownership() {
        let model = project_model();
        let current = serde_json::to_string(&rust_layout().checkpoint_descriptor()).unwrap();
        let legacy = current.replacen("\"version\":2", "\"version\":1", 1);
        let descriptor: CheckpointProjectDescriptor = serde_json::from_str(&legacy).unwrap();

        assert_eq!(descriptor.version(), 1);
        assert_eq!(serde_json::to_string(&descriptor).unwrap(), legacy);
        let owned = descriptor.owned_paths(&model).unwrap();
        assert!(owned.contains(&"Cargo.lock".to_string()));
        assert!(!owned.contains(&"Cargo.toml".to_string()));
        assert!(!owned.contains(&"theseus.json".to_string()));
    }

    #[test]
    fn model_record_cannot_overwrite_another_owned_file() {
        let model = project_model();
        for path in [
            "rust/fixture/src/generated.rs",
            "rust/fixture/src/service.rs",
            "rust/agent/src/adapters.rs",
        ] {
            let record = ModelRecord::rust_builder(path, "", "fixture_model").unwrap();
            let layout = RustWorkspaceLayout::new(ProjectId::new("fixture").unwrap(), record);
            assert!(matches!(
                layout.owned_paths(&model),
                Err(ProjectLayoutError::PathCollision { .. })
            ));
        }

        let layout = RustWorkspaceLayout::new(
            ProjectId::new("fixture").unwrap(),
            ModelRecord::json(PROJECT_MANIFEST_PATH).unwrap(),
        );
        assert!(matches!(
            layout.owned_paths(&model),
            Err(ProjectLayoutError::PathCollision { .. })
        ));
    }
}
