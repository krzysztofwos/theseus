//! Fluent builders for authoring a [`Model`](crate::model::Model).
//!
//! These are consuming-self builders: each adder returns the value so an adopter
//! writes its model as a single chain. The leaf fields are passed inline. Nested
//! values ([`Service`], [`Port`]) are built with their own chains and handed up.

use crate::model::{
    CrateNode, Field, Inbound, Method, Model, Operation, Port, Service, Transport, TypeDef,
    TypeShape,
};

impl Model {
    /// Start a model with the given name.
    pub fn new(name: &str) -> Self {
        Model {
            name: name.to_string(),
            crates: Vec::new(),
            types: Vec::new(),
            services: Vec::new(),
            inbounds: Vec::new(),
        }
    }

    /// Add an inbound adapter named `name`, speaking `transport`, driving
    /// `service`, hosted in `crate_name`.
    pub fn inbound(mut self, name: &str, transport: Transport, service: &str, crate_name: &str) -> Self {
        self.inbounds.push(Inbound {
            name: name.to_string(),
            transport,
            service: service.to_string(),
            crate_name: crate_name.to_string(),
        });
        self
    }

    /// Add a crate node at `layer` depending on `depends_on`.
    pub fn crate_node(mut self, name: &str, dir: &str, layer: u32, depends_on: &[&str]) -> Self {
        self.crates.push(CrateNode {
            name: name.to_string(),
            dir: dir.to_string(),
            layer,
            depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
        });
        self
    }

    /// Add a validated newtype over `inner`.
    pub fn newtype(mut self, name: &str, inner: &str) -> Self {
        self.types.push(TypeDef {
            name: name.to_string(),
            shape: TypeShape::Newtype(inner.to_string()),
        });
        self
    }

    /// Add a struct type from `(field, type, doc)` triples.
    pub fn struct_type(mut self, name: &str, fields: &[(&str, &str, &str)]) -> Self {
        self.types.push(TypeDef {
            name: name.to_string(),
            shape: TypeShape::Struct(
                fields
                    .iter()
                    .map(|(field, ty, doc)| Field {
                        name: field.to_string(),
                        ty: ty.to_string(),
                        doc: doc.to_string(),
                    })
                    .collect(),
            ),
        });
        self
    }

    /// Register a type provided outside the model, named by its Rust path. An
    /// operation or port may reference it. Code generation names it.
    pub fn foreign_type(mut self, name: &str, path: &str) -> Self {
        self.types.push(TypeDef {
            name: name.to_string(),
            shape: TypeShape::Foreign(path.to_string()),
        });
        self
    }

    /// Add an enum type from its variant names.
    pub fn enum_type(mut self, name: &str, variants: &[&str]) -> Self {
        self.types.push(TypeDef {
            name: name.to_string(),
            shape: TypeShape::Enum(variants.iter().map(|v| v.to_string()).collect()),
        });
        self
    }

    /// Add a service.
    pub fn service(mut self, service: Service) -> Self {
        self.services.push(service);
        self
    }
}

impl Service {
    /// Start a service with the given name and inbound transport.
    pub fn new(name: &str, inbound: Transport) -> Self {
        Service {
            name: name.to_string(),
            crate_name: String::new(),
            inbound,
            operations: Vec::new(),
            outbound: Vec::new(),
        }
    }

    /// Place the service in the crate named `crate_name`. Code generation renders
    /// its contract into that crate.
    pub fn crate_name(mut self, crate_name: &str) -> Self {
        self.crate_name = crate_name.to_string();
        self
    }

    /// Add an operation.
    pub fn operation(mut self, name: &str, summary: &str, request: &str, response: &str) -> Self {
        self.operations.push(Operation {
            name: name.to_string(),
            summary: summary.to_string(),
            request: request.to_string(),
            response: response.to_string(),
        });
        self
    }

    /// Add an outbound port.
    pub fn port(mut self, port: Port) -> Self {
        self.outbound.push(port);
        self
    }
}

impl Port {
    /// Start an outbound port with the given name and summary.
    pub fn new(name: &str, summary: &str) -> Self {
        Port {
            name: name.to_string(),
            summary: summary.to_string(),
            target: None,
            methods: Vec::new(),
        }
    }

    /// Bind the port to the named service: its contract becomes that service's
    /// operations, so it adds no methods of its own.
    pub fn targeting(mut self, service: &str) -> Self {
        self.target = Some(service.to_string());
        self
    }

    /// Add a method.
    pub fn method(mut self, name: &str, summary: &str, request: &str, response: &str) -> Self {
        self.methods.push(Method {
            name: name.to_string(),
            summary: summary.to_string(),
            request: request.to_string(),
            response: response.to_string(),
        });
        self
    }
}
