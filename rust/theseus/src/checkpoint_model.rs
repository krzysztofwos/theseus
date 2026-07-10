//! Frozen model wire format embedded in version-one checkpoint manifests.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use theseus_modeling as modeling;
use thiserror::Error;

const SELF_MODEL_PATH_V1: &str = "rust/model/src/self_model.rs";

/// A frozen checkpoint model cannot reconstruct its declared workspace.
#[derive(Debug, Error)]
pub enum SnapshotModelError {
    #[error("checkpoint model names unmodeled crate {crate_name:?}")]
    MissingCrate { crate_name: String },
    #[error("checkpoint model names unmodeled service {service:?}")]
    MissingService { service: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SnapshotModelV1 {
    name: String,
    crates: Vec<SnapshotCrateV1>,
    types: Vec<SnapshotTypeDefV1>,
    services: Vec<SnapshotServiceV1>,
    inbounds: Vec<SnapshotInboundV1>,
    #[serde(default)]
    clients: Vec<SnapshotClientV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotCrateV1 {
    name: String,
    dir: String,
    layer: u32,
    depends_on: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotTypeDefV1 {
    name: String,
    shape: SnapshotTypeShapeV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum SnapshotTypeShapeV1 {
    Struct(Vec<SnapshotFieldV1>),
    Newtype(String),
    Enum {
        variants: Vec<SnapshotVariantV1>,
        rust: Option<String>,
    },
    Foreign(String),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotVariantV1 {
    name: String,
    fields: Vec<SnapshotFieldV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFieldV1 {
    name: String,
    ty: String,
    doc: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotServiceV1 {
    name: String,
    crate_name: String,
    operations: Vec<SnapshotOperationV1>,
    outbound: Vec<SnapshotPortV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotOperationV1 {
    name: String,
    summary: String,
    request: String,
    response: String,
    uses: Vec<String>,
    tool: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPortV1 {
    name: String,
    summary: String,
    target: Option<String>,
    methods: Vec<SnapshotMethodV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotMethodV1 {
    name: String,
    summary: String,
    request: String,
    response: String,
    gated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotInboundV1 {
    name: String,
    transport: SnapshotTransportV1,
    service: String,
    crate_name: String,
    outbound: Vec<SnapshotPortV1>,
    turns: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotClientV1 {
    name: String,
    transport: SnapshotTransportV1,
    service: String,
    crate_name: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
enum SnapshotTransportV1 {
    Cli,
    Http,
    Grpc,
    Agent,
    Mcp,
}

impl SnapshotModelV1 {
    pub(crate) fn owned_paths(&self) -> Result<Vec<String>, SnapshotModelError> {
        let mut paths = BTreeSet::new();

        let mut generated_hosts = BTreeSet::new();
        generated_hosts.extend(
            self.services
                .iter()
                .map(|service| service.crate_name.as_str()),
        );
        generated_hosts.extend(
            self.inbounds
                .iter()
                .filter(|inbound| {
                    matches!(
                        inbound.transport,
                        SnapshotTransportV1::Cli
                            | SnapshotTransportV1::Http
                            | SnapshotTransportV1::Grpc
                    ) || !inbound.outbound.is_empty()
                        || inbound.turns.is_some()
                })
                .map(|inbound| inbound.crate_name.as_str()),
        );
        generated_hosts.extend(self.clients.iter().map(|client| client.crate_name.as_str()));
        for crate_name in generated_hosts {
            paths.insert(format!(
                "rust/{}/src/generated.rs",
                self.crate_dir(crate_name)?
            ));
        }

        for (crate_name, service_name) in self
            .inbounds
            .iter()
            .filter(|inbound| inbound.transport == SnapshotTransportV1::Grpc)
            .map(|inbound| (inbound.crate_name.as_str(), inbound.service.as_str()))
            .chain(
                self.clients
                    .iter()
                    .filter(|client| client.transport == SnapshotTransportV1::Grpc)
                    .map(|client| (client.crate_name.as_str(), client.service.as_str())),
            )
        {
            let service = self.service(service_name)?;
            paths.insert(format!(
                "rust/{}/proto/{}.proto",
                self.crate_dir(crate_name)?,
                service.name.to_lowercase()
            ));
        }
        paths.insert(SELF_MODEL_PATH_V1.to_string());

        let mut scaffolded = BTreeSet::new();
        for service in &self.services {
            if !scaffolded.insert(service.crate_name.as_str())
                || self
                    .inbounds
                    .iter()
                    .any(|inbound| inbound.crate_name == service.crate_name)
            {
                continue;
            }
            let dir = self.crate_dir(&service.crate_name)?;
            paths.insert(format!("rust/{dir}/Cargo.toml"));
            paths.insert(format!("rust/{dir}/src/lib.rs"));
            paths.insert(format!("rust/{dir}/src/service.rs"));
        }
        for inbound in &self.inbounds {
            if !scaffolded.insert(inbound.crate_name.as_str())
                || self
                    .services
                    .iter()
                    .any(|service| service.crate_name == inbound.crate_name)
            {
                continue;
            }
            self.service(&inbound.service)?;
            let dir = self.crate_dir(&inbound.crate_name)?;
            paths.insert(format!("rust/{dir}/Cargo.toml"));
            paths.insert(format!("rust/{dir}/src/main.rs"));
        }

        for service in &self.services {
            let dir = self.crate_dir(&service.crate_name)?;
            paths.insert(format!("rust/{dir}/src/lib.rs"));
            paths.insert(format!("rust/{dir}/src/service.rs"));
        }
        for inbound in self
            .inbounds
            .iter()
            .filter(|inbound| !inbound.outbound.is_empty())
        {
            paths.insert(format!(
                "rust/{}/src/adapters.rs",
                self.crate_dir(&inbound.crate_name)?
            ));
        }
        paths.insert("Cargo.lock".to_string());
        Ok(paths.into_iter().collect())
    }

    fn crate_dir(&self, crate_name: &str) -> Result<&str, SnapshotModelError> {
        self.crates
            .iter()
            .find(|node| node.name == crate_name)
            .map(|node| node.dir.as_str())
            .ok_or_else(|| SnapshotModelError::MissingCrate {
                crate_name: crate_name.to_string(),
            })
    }

    fn service(&self, name: &str) -> Result<&SnapshotServiceV1, SnapshotModelError> {
        self.services
            .iter()
            .find(|service| service.name == name)
            .ok_or_else(|| SnapshotModelError::MissingService {
                service: name.to_string(),
            })
    }
}

impl From<&modeling::Model> for SnapshotModelV1 {
    fn from(model: &modeling::Model) -> Self {
        Self {
            name: model.name.clone(),
            crates: model.crates.iter().map(SnapshotCrateV1::from).collect(),
            types: model.types.iter().map(SnapshotTypeDefV1::from).collect(),
            services: model.services.iter().map(SnapshotServiceV1::from).collect(),
            inbounds: model.inbounds.iter().map(SnapshotInboundV1::from).collect(),
            clients: model.clients.iter().map(SnapshotClientV1::from).collect(),
        }
    }
}

impl From<SnapshotModelV1> for modeling::Model {
    fn from(model: SnapshotModelV1) -> Self {
        Self {
            name: model.name,
            crates: model.crates.into_iter().map(Into::into).collect(),
            types: model.types.into_iter().map(Into::into).collect(),
            services: model.services.into_iter().map(Into::into).collect(),
            inbounds: model.inbounds.into_iter().map(Into::into).collect(),
            clients: model.clients.into_iter().map(Into::into).collect(),
        }
    }
}

macro_rules! convert_struct {
    ($snapshot:ty, $model:ty, { $($field:ident),+ $(,)? }) => {
        impl From<&$model> for $snapshot {
            fn from(value: &$model) -> Self {
                Self { $($field: value.$field.clone()),+ }
            }
        }

        impl From<$snapshot> for $model {
            fn from(value: $snapshot) -> Self {
                Self { $($field: value.$field),+ }
            }
        }
    };
}

convert_struct!(SnapshotCrateV1, modeling::CrateNode, {
    name,
    dir,
    layer,
    depends_on
});
convert_struct!(SnapshotFieldV1, modeling::Field, { name, ty, doc });
convert_struct!(SnapshotOperationV1, modeling::Operation, {
    name,
    summary,
    request,
    response,
    uses,
    tool
});
convert_struct!(SnapshotMethodV1, modeling::Method, {
    name,
    summary,
    request,
    response,
    gated
});

impl From<&modeling::TypeDef> for SnapshotTypeDefV1 {
    fn from(value: &modeling::TypeDef) -> Self {
        Self {
            name: value.name.clone(),
            shape: SnapshotTypeShapeV1::from(&value.shape),
        }
    }
}

impl From<SnapshotTypeDefV1> for modeling::TypeDef {
    fn from(value: SnapshotTypeDefV1) -> Self {
        Self {
            name: value.name,
            shape: value.shape.into(),
        }
    }
}

impl From<&modeling::TypeShape> for SnapshotTypeShapeV1 {
    fn from(value: &modeling::TypeShape) -> Self {
        match value {
            modeling::TypeShape::Struct(fields) => {
                Self::Struct(fields.iter().map(SnapshotFieldV1::from).collect())
            }
            modeling::TypeShape::Newtype(inner) => Self::Newtype(inner.clone()),
            modeling::TypeShape::Enum { variants, rust } => Self::Enum {
                variants: variants.iter().map(SnapshotVariantV1::from).collect(),
                rust: rust.clone(),
            },
            modeling::TypeShape::Foreign(path) => Self::Foreign(path.clone()),
        }
    }
}

impl From<SnapshotTypeShapeV1> for modeling::TypeShape {
    fn from(value: SnapshotTypeShapeV1) -> Self {
        match value {
            SnapshotTypeShapeV1::Struct(fields) => {
                Self::Struct(fields.into_iter().map(Into::into).collect())
            }
            SnapshotTypeShapeV1::Newtype(inner) => Self::Newtype(inner),
            SnapshotTypeShapeV1::Enum { variants, rust } => Self::Enum {
                variants: variants.into_iter().map(Into::into).collect(),
                rust,
            },
            SnapshotTypeShapeV1::Foreign(path) => Self::Foreign(path),
        }
    }
}

impl From<&modeling::Variant> for SnapshotVariantV1 {
    fn from(value: &modeling::Variant) -> Self {
        Self {
            name: value.name.clone(),
            fields: value.fields.iter().map(SnapshotFieldV1::from).collect(),
        }
    }
}

impl From<SnapshotVariantV1> for modeling::Variant {
    fn from(value: SnapshotVariantV1) -> Self {
        Self {
            name: value.name,
            fields: value.fields.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&modeling::Service> for SnapshotServiceV1 {
    fn from(value: &modeling::Service) -> Self {
        Self {
            name: value.name.clone(),
            crate_name: value.crate_name.clone(),
            operations: value
                .operations
                .iter()
                .map(SnapshotOperationV1::from)
                .collect(),
            outbound: value.outbound.iter().map(SnapshotPortV1::from).collect(),
        }
    }
}

impl From<SnapshotServiceV1> for modeling::Service {
    fn from(value: SnapshotServiceV1) -> Self {
        Self {
            name: value.name,
            crate_name: value.crate_name,
            operations: value.operations.into_iter().map(Into::into).collect(),
            outbound: value.outbound.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&modeling::Port> for SnapshotPortV1 {
    fn from(value: &modeling::Port) -> Self {
        Self {
            name: value.name.clone(),
            summary: value.summary.clone(),
            target: value.target.clone(),
            methods: value.methods.iter().map(SnapshotMethodV1::from).collect(),
        }
    }
}

impl From<SnapshotPortV1> for modeling::Port {
    fn from(value: SnapshotPortV1) -> Self {
        Self {
            name: value.name,
            summary: value.summary,
            target: value.target,
            methods: value.methods.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<&modeling::Inbound> for SnapshotInboundV1 {
    fn from(value: &modeling::Inbound) -> Self {
        Self {
            name: value.name.clone(),
            transport: value.transport.into(),
            service: value.service.clone(),
            crate_name: value.crate_name.clone(),
            outbound: value.outbound.iter().map(SnapshotPortV1::from).collect(),
            turns: value.turns,
        }
    }
}

impl From<SnapshotInboundV1> for modeling::Inbound {
    fn from(value: SnapshotInboundV1) -> Self {
        Self {
            name: value.name,
            transport: value.transport.into(),
            service: value.service,
            crate_name: value.crate_name,
            outbound: value.outbound.into_iter().map(Into::into).collect(),
            turns: value.turns,
        }
    }
}

impl From<&modeling::Client> for SnapshotClientV1 {
    fn from(value: &modeling::Client) -> Self {
        Self {
            name: value.name.clone(),
            transport: value.transport.into(),
            service: value.service.clone(),
            crate_name: value.crate_name.clone(),
        }
    }
}

impl From<SnapshotClientV1> for modeling::Client {
    fn from(value: SnapshotClientV1) -> Self {
        Self {
            name: value.name,
            transport: value.transport.into(),
            service: value.service,
            crate_name: value.crate_name,
        }
    }
}

impl From<modeling::Transport> for SnapshotTransportV1 {
    fn from(value: modeling::Transport) -> Self {
        match value {
            modeling::Transport::Cli => Self::Cli,
            modeling::Transport::Http => Self::Http,
            modeling::Transport::Grpc => Self::Grpc,
            modeling::Transport::Agent => Self::Agent,
            modeling::Transport::Mcp => Self::Mcp,
        }
    }
}

impl From<SnapshotTransportV1> for modeling::Transport {
    fn from(value: SnapshotTransportV1) -> Self {
        match value {
            SnapshotTransportV1::Cli => Self::Cli,
            SnapshotTransportV1::Http => Self::Http,
            SnapshotTransportV1::Grpc => Self::Grpc,
            SnapshotTransportV1::Agent => Self::Agent,
            SnapshotTransportV1::Mcp => Self::Mcp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SHAPES_V1: &str = r#"{
        "name":"Fixture",
        "crates":[{"name":"app","dir":"app","layer":1,"depends_on":[]}],
        "types":[
            {"name":"Record","shape":{"Struct":[{"name":"value","ty":"String","doc":"Value."}]}},
            {"name":"Id","shape":{"Newtype":"String"}},
            {"name":"Choice","shape":{"Enum":{"variants":[{"name":"Empty","fields":[]},{"name":"Data","fields":[{"name":"id","ty":"Id","doc":"Id."}]}],"rust":"fixture::Choice"}}},
            {"name":"External","shape":{"Foreign":"fixture::External"}}
        ],
        "services":[{
            "name":"App","crate_name":"app",
            "operations":[{"name":"run","summary":"Run.","request":"Record","response":"External","uses":["store"],"tool":"Run it."}],
            "outbound":[{"name":"store","summary":"Store.","target":null,"methods":[{"name":"put","summary":"Put.","request":"Id","response":"Empty","gated":true}]}]
        }],
        "inbounds":[
            {"name":"cli","transport":"Cli","service":"App","crate_name":"app","outbound":[],"turns":null},
            {"name":"http","transport":"Http","service":"App","crate_name":"app","outbound":[],"turns":null},
            {"name":"grpc","transport":"Grpc","service":"App","crate_name":"app","outbound":[],"turns":null},
            {"name":"agent","transport":"Agent","service":"App","crate_name":"app","outbound":[],"turns":8},
            {"name":"mcp","transport":"Mcp","service":"App","crate_name":"app","outbound":[],"turns":null}
        ],
        "clients":[{"name":"client","transport":"Http","service":"App","crate_name":"app"}]
    }"#;

    #[test]
    fn version_one_wire_schema_covers_every_nested_shape() {
        let snapshot: SnapshotModelV1 =
            serde_json::from_str(ALL_SHAPES_V1).expect("the frozen fixture parses");
        let encoded = serde_json::to_value(&snapshot).expect("the frozen fixture serializes");
        let expected: serde_json::Value =
            serde_json::from_str(ALL_SHAPES_V1).expect("the fixture is JSON");
        assert_eq!(encoded, expected);

        let current: modeling::Model = snapshot.into();
        let round_trip = SnapshotModelV1::from(&current);
        assert_eq!(
            serde_json::to_value(round_trip).unwrap(),
            expected,
            "explicit conversions preserve every version-one shape"
        );
    }

    #[test]
    fn version_one_wire_schema_rejects_future_fields() {
        let mut future_shape: serde_json::Value =
            serde_json::from_str(ALL_SHAPES_V1).expect("the fixture is JSON");
        future_shape["types"][2]["shape"]["Enum"]["future"] = serde_json::json!(true);
        assert!(serde_json::from_value::<SnapshotModelV1>(future_shape).is_err());
    }

    #[test]
    fn version_one_ownership_matches_the_manifest_creation_policy() {
        let model = theseus_model::theseus_model();
        let snapshot = SnapshotModelV1::from(&model);
        assert_eq!(
            snapshot.owned_paths().unwrap(),
            theseus_model::checkpoint_paths(&model).unwrap()
        );
    }
}
