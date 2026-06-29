//! The model types: Theseus's vocabulary for describing a tool's architecture.
//!
//! A [`Model`] describes a layered set of [`CrateNode`]s (so conformance has a
//! dependency direction to check), the [`TypeDef`]s the system exchanges, the
//! [`Service`]s it runs, and the [`Inbound`] adapters that drive them. A service
//! is a set of [`Operation`]s and outbound [`Port`]s naming its dependencies. An
//! inbound binds a service to a [`Transport`], and a service may carry more than
//! one. Theseus is itself one such service. Its outbound port is the filesystem
//! that `generate` and `patch` write to, and it is driven inbound over the CLI
//! and an agent loop.

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
    /// The inbound adapters that drive services over a transport.
    pub inbounds: Vec<Inbound>,
}

/// An inbound adapter: a transport that drives a service from the outside. The
/// service's operations are its inbound contract. The adapter translates external
/// input into operation calls. A service may carry more than one, and an adapter
/// may live in a crate other than the service it drives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inbound {
    /// Adapter name, e.g. `calculator`.
    pub name: String,
    /// The transport the adapter speaks.
    pub transport: Transport,
    /// The service the adapter drives.
    pub service: String,
    /// Cargo package name of the crate that hosts the adapter.
    pub crate_name: String,
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

    /// Look up a service by name.
    pub fn service_named(&self, name: &str) -> Option<&Service> {
        self.services.iter().find(|service| service.name == name)
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

/// A service: a set of operations and outbound ports, living in one crate of the
/// workspace. Its operations are its inbound contract. An [`Inbound`] adapter
/// drives it over a transport, and a service may carry none, one, or several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Service {
    /// Service name.
    pub name: String,
    /// Cargo package name of the crate this service lives in. Code generation
    /// renders the service's contract into that crate.
    pub crate_name: String,
    /// The operations the service exposes.
    pub operations: Vec<Operation>,
    /// The outbound dependencies the service calls.
    pub outbound: Vec<Port>,
}

/// A transport an [`Inbound`] adapter speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    Cli,
    Http,
    Grpc,
    /// An LLM-driven agent loop. The adapter runs the loop in its own binary, so
    /// the model renders no command surface for it.
    Agent,
    /// A Model Context Protocol server, exposing the service's operations as tools
    /// to an external host. The adapter serves them in its own binary, so the
    /// model renders no command surface for it.
    Mcp,
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
/// calls and an adapter must implement. A port may instead be bound to another
/// service, in which case its contract is that service's operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// Port name, e.g. `workspace`.
    pub name: String,
    /// One-line description.
    pub summary: String,
    /// When set, the port is bound to the named service: its contract is that
    /// service's operations, and code generation wires it to that service's
    /// trait rather than emitting one of its own.
    pub target: Option<String>,
    /// The methods the port exposes. Empty for a service-targeting port.
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
