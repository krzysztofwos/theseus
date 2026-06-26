//! The model types: Theseus's vocabulary for describing a tool's architecture.
//!
//! A [`Model`] describes a layered set of [`CrateNode`]s (so conformance has a
//! dependency direction to check), the [`TypeDef`]s the system exchanges, and
//! the [`Service`]s it runs. A service has one inbound [`Transport`], a set of
//! [`Operation`]s, and outbound [`Port`]s naming its dependencies. Theseus is
//! itself one such service — its inbound port is the CLI, and its outbound ports
//! are the filesystem interactions of `generate` and `patch`.

use serde::{Deserialize, Serialize};

/// A complete model of a tool: its crate layering, its types, and its services.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Model {
    /// Human-facing name of the modeled tool.
    pub name: String,
    /// The intended crate layering. Verification checks the real workspace
    /// against this.
    pub crates: Vec<CrateNode>,
    /// The named types the system's operations and ports exchange.
    pub types: Vec<TypeDef>,
    /// The services the tool runs.
    pub services: Vec<Service>,
}

impl Model {
    /// Every operation across all services, in declaration order.
    pub fn operations(&self) -> Vec<&Operation> {
        self.services
            .iter()
            .flat_map(|service| service.operations.iter())
            .collect()
    }

    /// Look up an operation by name across all services.
    pub fn operation(&self, name: &str) -> Option<&Operation> {
        self.operations().into_iter().find(|op| op.name == name)
    }

    /// The service whose operations include `op_name`.
    pub fn service_of_operation(&self, op_name: &str) -> Option<&Service> {
        self.services
            .iter()
            .find(|service| service.operations.iter().any(|op| op.name == op_name))
    }

    /// Look up a type definition by name.
    pub fn type_def(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.name == name)
    }

    /// Look up a crate node by package name.
    pub fn crate_named(&self, name: &str) -> Option<&CrateNode> {
        self.crates.iter().find(|c| c.name == name)
    }
}

/// One crate in the workspace, with its intended layer and intra-workspace
/// dependencies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrateNode {
    /// Cargo package name, e.g. `theseus-model`.
    pub name: String,
    /// Directory under `rust/`, e.g. `model`.
    pub dir: String,
    /// Architectural layer. `0` is the innermost (the kernel). Higher layers
    /// may depend only on lower layers.
    pub layer: u32,
    /// Package names of the other workspace crates this one depends on.
    pub depends_on: Vec<String>,
}

/// A service: an inbound transport, a set of operations, and outbound ports,
/// living in one crate of the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    /// Service name.
    pub name: String,
    /// Cargo package name of the crate this service lives in. Code generation
    /// renders the service's contract into that crate.
    pub crate_name: String,
    /// How the service is invoked.
    pub inbound: Transport,
    /// The operations the service exposes.
    pub operations: Vec<Operation>,
    /// The outbound dependencies the service calls.
    pub outbound: Vec<Port>,
}

/// How a service is driven from the outside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    Cli,
    Http,
    Grpc,
    /// An in-process call from another service. The service contributes a trait
    /// its callers depend on, without a command surface of its own.
    InProcess,
}

/// One operation in a service's inbound surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Operation {
    /// Operation name (a CLI subcommand for a `Cli` service).
    pub name: String,
    /// One-line description.
    pub summary: String,
    /// Request type label. Its [`TypeDef`] fields drive the inbound surface —
    /// for a `Cli` service, each field becomes a command-line argument.
    pub request: String,
    /// Response type label.
    pub response: String,
}

/// An outbound dependency of a service: a named set of methods the service
/// calls and an adapter must implement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// Port name, e.g. `workspace`.
    pub name: String,
    /// One-line description.
    pub summary: String,
    /// The methods the port exposes.
    pub methods: Vec<Method>,
}

/// One method of an outbound [`Port`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method {
    /// Method name.
    pub name: String,
    /// One-line description.
    pub summary: String,
    /// Request type label.
    pub request: String,
    /// Response type label.
    pub response: String,
}

/// A named type the system exchanges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeDef {
    /// Type name.
    pub name: String,
    /// The type's structure.
    pub shape: TypeShape,
}

/// The structure of a [`TypeDef`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeShape {
    /// A record with named fields.
    Struct(Vec<Field>),
    /// A validated wrapper around one inner type.
    Newtype(String),
    /// A closed set of variant names.
    Enum(Vec<String>),
    /// A type provided outside the model, named by its Rust path. Operations and
    /// ports may reference it. Code generation names it rather than emitting it.
    Foreign(String),
}

/// One field of a struct [`TypeDef`].
///
/// The field type drives the CLI projection: a `bool` field becomes a flag, a
/// `Vec<T>` field a repeatable argument, an `Option<T>` field an optional
/// argument, anything else a required argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: String,
    pub doc: String,
}
