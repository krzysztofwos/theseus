//! Code generation: planks rendered from a model.
//!
//! This is the Ship-of-Theseus move and the seed of the generation engine. From
//! the model it renders the CLI's inbound adapter (the command surface), the
//! typed request structs, the inbound service trait each operation must satisfy,
//! the outbound port traits a service depends on, and the composition root that
//! carries the wired ports. The hand-written adapters and the service impl are the
//! authored leaves, and regeneration writes only the generated file, so the leaves
//! stay intact. An adopter pairs the rendered contents with an output path.
//! [`verify`](crate::verify) drift-gates the result.
//!
//! The render builds a token stream and formats it with `prettyplease`, so the
//! output is canonical and stable across regenerations. The generated file carries
//! an `@generated` marker, and `rustfmt` is configured to leave such files alone,
//! so the drift gate compares like for like.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    label::{base_label, map_value, optional_inner, vec_inner},
    model::{
        Client, Field, Inbound, Method, Model, Operation, Port, Service, Transport, TypeDef,
        TypeShape, Variant,
    },
};

mod cli;
mod clients;
mod contract;
mod grpc;
mod http;
mod proto;
mod tools;

use cli::render_inbound_module;
use clients::{render_grpc_client_module, render_http_client_module};
use contract::{
    render_composition_root, render_port_trait, render_request_structs, render_service_trait,
    render_unimplemented,
};
use grpc::render_grpc_module;
use http::render_http_module;
pub use proto::render_proto;
use tools::{render_tool_catalog, render_tool_dispatch};

/// A file rendered from the model, addressed relative to the workspace root. An
/// adopter sets the path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedFile {
    pub path: String,
    pub contents: String,
}

/// A model projection that cannot be rendered safely.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RenderError {
    /// A crate directory must be one normal path component below `rust/`.
    #[error("crate `{crate_name}` directory `{dir}` is not a safe workspace directory")]
    InvalidCrateDirectory { crate_name: String, dir: String },
    /// A model node that contributes generated Rust is not a valid identifier.
    #[error("{kind} `{name}` is not a valid Rust identifier: {message}")]
    InvalidIdentifier {
        kind: &'static str,
        name: String,
        message: String,
    },
    /// A modeled type cannot be expressed as a Rust type.
    #[error("type label `{label}` is not valid Rust: {message}")]
    InvalidRustType { label: String, message: String },
    /// An operation request must be empty or name a modeled struct.
    #[error(
        "operation `{service}.{operation}` request `{request}` must be `Empty` or a modeled struct"
    )]
    InvalidRequestType {
        service: String,
        operation: String,
        request: String,
    },
    /// A boundary or field type label has no builtin or modeled definition.
    #[error("type label `{label}` has no modeled definition")]
    UnresolvedType { label: String },
    /// A modeled type shape has no Rust projection yet.
    #[error("modeled type `{ty}` is not supported by the Rust renderer: {reason}")]
    UnsupportedModeledType { ty: String, reason: &'static str },
    /// A generated file is hosted by a crate absent from the model.
    #[error("crate `{crate_name}` is not modeled")]
    CrateNotModeled { crate_name: String },
    /// A transport names a service absent from the model.
    #[error("service `{service}` is not modeled")]
    ServiceNotModeled { service: String },
    /// Protobuf has no supported projection for a modeled field type.
    #[error("gRPC field `{field}` has unsupported type `{ty}`")]
    UnsupportedGrpcFieldType { field: String, ty: String },
    /// Generated Rust surfaces do not yet convert nested struct fields.
    #[error("field `{field}` has unsupported nested struct type `{ty}`")]
    UnsupportedNestedStructType { field: String, ty: String },
    /// Generated Rust surfaces do not yet define and serialize struct responses.
    #[error("operation `{service}.{operation}` has unsupported struct response `{response}`")]
    UnsupportedResponseType {
        service: String,
        operation: String,
        response: String,
    },
    /// The generated CLI cannot parse this container shape.
    #[error("CLI field `{field}` has unsupported type `{ty}`")]
    UnsupportedCliFieldType { field: String, ty: String },
    /// Distinct model names collapse to the same generated identifier.
    #[error("generated {namespace} name `{name}` is not unique")]
    NameCollision {
        namespace: &'static str,
        name: String,
    },
    /// Prost's generated Rust shape cannot be converted to the modeled field.
    #[error("gRPC field `{field}` with type `{ty}` is unsupported: {reason}")]
    UnsupportedGrpcConversion {
        field: String,
        ty: String,
        reason: &'static str,
    },
    /// A protobuf package, message, RPC, or field name is not renderable.
    #[error("{kind} `{name}` is not a valid protobuf identifier")]
    InvalidProtoIdentifier { kind: &'static str, name: String },
    /// The complete generated protobuf contract does not parse.
    #[error("generated protobuf for service `{service}` is invalid: {message}")]
    InvalidProto { service: String, message: String },
    /// The assembled token stream is not a valid Rust file.
    #[error("generated code for crate `{crate_name}` is not valid Rust: {message}")]
    InvalidRust { crate_name: String, message: String },
    /// The self-model projection is not a valid Rust file.
    #[error("generated model source for `{function}` is not valid Rust: {message}")]
    InvalidModelSource { function: String, message: String },
}

/// Prove that every model-driven token used by the Rust renderers is valid
/// before the infallible helper functions assemble it. This is the common
/// preflight for every public Rust projection.
pub(crate) fn validate_render_inputs(model: &Model) -> Result<(), RenderError> {
    for crate_node in &model.crates {
        let mut components = std::path::Path::new(&crate_node.dir).components();
        let is_one_normal_component =
            matches!(components.next(), Some(std::path::Component::Normal(_)))
                && components.next().is_none()
                && !crate_node.dir.contains('/')
                && !crate_node.dir.contains('\\');
        if !is_one_normal_component {
            return Err(RenderError::InvalidCrateDirectory {
                crate_name: crate_node.name.clone(),
                dir: crate_node.dir.clone(),
            });
        }
    }

    for def in &model.types {
        validate_identifier("type", &def.name)?;
        if matches!(
            def.name.as_str(),
            "Empty"
                | "String"
                | "Vec"
                | "Option"
                | "BTreeMap"
                | "Send"
                | "Sync"
                | "Sized"
                | "Default"
                | "Some"
                | "None"
                | "Ok"
                | "Err"
                | "bool"
                | "char"
                | "f32"
                | "f64"
                | "i8"
                | "i16"
                | "i32"
                | "i64"
                | "i128"
                | "isize"
                | "u8"
                | "u16"
                | "u32"
                | "u64"
                | "u128"
                | "usize"
        ) {
            return Err(RenderError::NameCollision {
                namespace: "builtin type",
                name: def.name.clone(),
            });
        }
        validate_identifier(
            "type parser",
            &format!("parse_{}", proto_snake_case(&def.name)),
        )?;
        validate_identifier("protobuf module", &proto_snake_case(&def.name))?;
        match &def.shape {
            TypeShape::Struct(fields) => validate_fields(fields, model)?,
            TypeShape::Newtype(inner) | TypeShape::Foreign(inner) => {
                validate_rust_type(inner)?;
            }
            TypeShape::Enum { variants, rust } => {
                if let Some(path) = rust {
                    validate_rust_type(path)?;
                }
                let mut variant_names = Vec::new();
                for variant in variants {
                    let rendered = pascal_case(&variant.name);
                    validate_identifier("enum variant", &rendered)?;
                    ensure_unique_name(&mut variant_names, "enum variant", rendered)?;
                    validate_fields(&variant.fields, model)?;
                }
            }
        }
    }

    for service in &model.services {
        validate_identifier("service", &pascal_case(&service.name))?;
        validate_identifier("service module", &proto_snake_case(&service.name))?;
        let mut operation_names = Vec::new();
        for op in &service.operations {
            validate_identifier("operation", &op.name)?;
            let variant = pascal_case(&op.name);
            validate_identifier("operation variant", &variant)?;
            ensure_unique_name(&mut operation_names, "operation", variant)?;
            if op.request != "Empty" {
                match model.type_def(&op.request) {
                    Some(TypeDef {
                        shape: TypeShape::Struct(_),
                        ..
                    }) => {}
                    _ => {
                        validate_type_reference(&op.request, model)?;
                        return Err(RenderError::InvalidRequestType {
                            service: service.name.clone(),
                            operation: op.name.clone(),
                            request: op.request.clone(),
                        });
                    }
                }
            }
            validate_rust_type(&resolve_field_type(&op.response, model))?;
            validate_type_reference(&op.response, model)?;
            if model
                .type_def(base_label(&op.response))
                .is_some_and(|def| matches!(def.shape, TypeShape::Struct(_)))
            {
                return Err(RenderError::UnsupportedResponseType {
                    service: service.name.clone(),
                    operation: op.name.clone(),
                    response: op.response.clone(),
                });
            }
        }
        for port in &service.outbound {
            validate_port(port, model)?;
        }
    }

    for inbound in &model.inbounds {
        let service = model.service_named(&inbound.service).ok_or_else(|| {
            RenderError::ServiceNotModeled {
                service: inbound.service.clone(),
            }
        })?;
        if inbound.transport == Transport::Cli {
            for op in &service.operations {
                for field in request_fields(op, model) {
                    validate_cli_field(field)?;
                }
            }
        }
        if inbound.turns.is_some() {
            validate_identifier(
                "inbound turn budget",
                &snake_case(&inbound.name).to_uppercase(),
            )?;
        }
        for port in &inbound.outbound {
            validate_port(port, model)?;
        }
    }
    for client in &model.clients {
        if model.service_named(&client.service).is_none() {
            return Err(RenderError::ServiceNotModeled {
                service: client.service.clone(),
            });
        }
    }
    Ok(())
}

fn ensure_unique_name(
    names: &mut Vec<String>,
    namespace: &'static str,
    name: String,
) -> Result<(), RenderError> {
    if names.contains(&name) {
        Err(RenderError::NameCollision { namespace, name })
    } else {
        names.push(name);
        Ok(())
    }
}

fn validate_cli_field(field: &Field) -> Result<(), RenderError> {
    let unsupported = map_value(&field.ty).is_some()
        || optional_inner(&field.ty)
            .or_else(|| vec_inner(&field.ty))
            .is_some_and(|inner| {
                optional_inner(inner).is_some()
                    || vec_inner(inner).is_some()
                    || map_value(inner).is_some()
            });
    if unsupported {
        Err(RenderError::UnsupportedCliFieldType {
            field: field.name.clone(),
            ty: field.ty.clone(),
        })
    } else {
        Ok(())
    }
}

fn validate_grpc_conversions(service: &Service, model: &Model) -> Result<(), RenderError> {
    for op in &service.operations {
        let Some(request) = request_type(op, model) else {
            continue;
        };
        let TypeShape::Struct(fields) = &request.shape else {
            continue;
        };
        for field in fields {
            if optional_inner(&field.ty).is_some_and(|inner| map_value(inner).is_some()) {
                return Err(RenderError::UnsupportedGrpcConversion {
                    field: field.name.clone(),
                    ty: field.ty.clone(),
                    reason: "optional maps have no distinct protobuf representation",
                });
            }
            validate_grpc_conversion_field(field, model)?;
            let base = base_label(&field.ty);
            if let Some(TypeDef {
                shape: TypeShape::Enum { variants, .. },
                ..
            }) = model.type_def(base)
            {
                if optional_inner(&field.ty).is_some() || map_value(&field.ty).is_some() {
                    return Err(RenderError::UnsupportedGrpcConversion {
                        field: field.name.clone(),
                        ty: field.ty.clone(),
                        reason: "enum fields support only a value or Vec",
                    });
                }
                for variant in variants {
                    if variant.fields.is_empty() {
                        return Err(RenderError::UnsupportedGrpcConversion {
                            field: field.name.clone(),
                            ty: field.ty.clone(),
                            reason: "unit enum variants are not implemented",
                        });
                    }
                    for nested in &variant.fields {
                        validate_grpc_conversion_field(nested, model)?;
                        if model.type_def(base_label(&nested.ty)).is_some_and(|def| {
                            matches!(def.shape, TypeShape::Struct(_) | TypeShape::Enum { .. })
                        }) {
                            return Err(RenderError::UnsupportedGrpcConversion {
                                field: nested.name.clone(),
                                ty: nested.ty.clone(),
                                reason: "nested message values in enum variants are not implemented",
                            });
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_grpc_conversion_field(field: &Field, model: &Model) -> Result<(), RenderError> {
    let base = base_label(&field.ty);
    if matches!(base, "i8" | "i16" | "isize" | "u8" | "u16" | "usize") {
        return Err(RenderError::UnsupportedGrpcConversion {
            field: field.name.clone(),
            ty: field.ty.clone(),
            reason: "prost widens this scalar on the wire",
        });
    }
    if map_value(&field.ty).is_some()
        && model
            .type_def(base)
            .is_some_and(|def| matches!(def.shape, TypeShape::Enum { .. }))
    {
        return Err(RenderError::UnsupportedGrpcConversion {
            field: field.name.clone(),
            ty: field.ty.clone(),
            reason: "map values cannot use modeled enums",
        });
    }
    Ok(())
}

fn validate_fields(fields: &[Field], model: &Model) -> Result<(), RenderError> {
    for field in fields {
        validate_identifier("field", &field.name)?;
        validate_rust_type(&resolve_field_type(&field.ty, model))?;
        validate_type_reference(&field.ty, model)?;
        let nested = base_label(&field.ty);
        if model
            .type_def(nested)
            .is_some_and(|def| matches!(def.shape, TypeShape::Struct(_)))
        {
            return Err(RenderError::UnsupportedNestedStructType {
                field: field.name.clone(),
                ty: field.ty.clone(),
            });
        }
    }
    Ok(())
}

fn validate_port(port: &Port, model: &Model) -> Result<(), RenderError> {
    validate_identifier("port", &pascal_case(&port.name))?;
    validate_identifier("port field", &snake_case(&port.name))?;
    if let Some(target) = &port.target {
        let service =
            model
                .service_named(target)
                .ok_or_else(|| RenderError::ServiceNotModeled {
                    service: target.clone(),
                })?;
        validate_identifier("target service", &pascal_case(&service.name))?;
    }
    for method in &port.methods {
        validate_identifier("port method", &method.name)?;
        validate_rust_type(&resolve_field_type(&method.request, model))?;
        validate_rust_type(&resolve_field_type(&method.response, model))?;
        validate_type_reference(&method.request, model)?;
        validate_type_reference(&method.response, model)?;
        for (role, label) in [
            ("request", method.request.as_str()),
            ("response", method.response.as_str()),
        ] {
            let base = base_label(label);
            if label != base
                && model
                    .type_def(base)
                    .is_some_and(|def| matches!(def.shape, TypeShape::Struct(_)))
            {
                return Err(RenderError::UnsupportedNestedStructType {
                    field: format!("{}.{} {role}", port.name, method.name),
                    ty: label.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn validate_type_reference(label: &str, model: &Model) -> Result<(), RenderError> {
    if matches!(
        label,
        "Empty"
            | "String"
            | "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    ) {
        return Ok(());
    }
    if let Some(def) = model.type_def(label) {
        return match &def.shape {
            TypeShape::Enum { rust: None, .. } => Err(RenderError::UnsupportedModeledType {
                ty: label.to_string(),
                reason: "model-owned enums are not implemented",
            }),
            _ => Ok(()),
        };
    }
    if let Some(inner) = optional_inner(label).or_else(|| vec_inner(label)) {
        return validate_type_reference(inner, model);
    }
    if let Some(value) = map_value(label) {
        return validate_type_reference(value, model);
    }
    Err(RenderError::UnresolvedType {
        label: label.to_string(),
    })
}

fn validate_identifier(kind: &'static str, name: &str) -> Result<(), RenderError> {
    syn::parse_str::<syn::Ident>(name)
        .map(|_| ())
        .map_err(|error| RenderError::InvalidIdentifier {
            kind,
            name: name.to_string(),
            message: error.to_string(),
        })
}

fn validate_rust_type(label: &str) -> Result<(), RenderError> {
    syn::parse_str::<syn::Type>(label)
        .map(|_| ())
        .map_err(|error| RenderError::InvalidRustType {
            label: label.to_string(),
            message: error.to_string(),
        })
}

/// Validate every generated Rust and protobuf surface a complete model owns.
/// Patch calls this before accepting a proposal, so a render failure is a
/// diagnostic and never a partial write.
pub(crate) fn validate_model_render(model: &Model) -> Result<(), RenderError> {
    validate_render_inputs(model)?;
    let mut rendered = Vec::new();
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
        if rendered.contains(&crate_name) {
            continue;
        }
        if model.crate_named(crate_name).is_none() {
            return Err(RenderError::CrateNotModeled {
                crate_name: crate_name.to_string(),
            });
        }
        render_module_for_crate(model, crate_name)?;
        rendered.push(crate_name);
    }

    let grpc_services = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.transport == Transport::Grpc)
        .map(|inbound| inbound.service.as_str())
        .chain(
            model
                .clients
                .iter()
                .filter(|client| client.transport == Transport::Grpc)
                .map(|client| client.service.as_str()),
        );
    let mut rendered = Vec::new();
    for service_name in grpc_services {
        if rendered.contains(&service_name) {
            continue;
        }
        let service =
            model
                .service_named(service_name)
                .ok_or_else(|| RenderError::ServiceNotModeled {
                    service: service_name.to_string(),
                })?;
        render_proto(model, service)?;
        rendered.push(service_name);
    }
    Ok(())
}

/// Render the generated scaffolding for the crate hosting the inbound CLI adapter.
pub fn render_cli_module(model: &Model) -> Result<String, RenderError> {
    let crate_name = model
        .inbounds
        .iter()
        .find(|inbound| inbound.transport == Transport::Cli)
        .map(|inbound| inbound.crate_name.as_str())
        .unwrap_or("");
    render_module_for_crate(model, crate_name)
}

/// Render the generated scaffolding for one crate: the services it hosts, their
/// request types and contract traits, the outbound ports they depend on with the
/// composition root that carries them, and the command surface, request parsers,
/// parsed invocation, and dispatch contributed by any inbound adapter it hosts.
pub fn render_module_for_crate(model: &Model, crate_name: &str) -> Result<String, RenderError> {
    validate_render_inputs(model)?;
    if !model.crates.is_empty() && model.crate_named(crate_name).is_none() {
        return Err(RenderError::CrateNotModeled {
            crate_name: crate_name.to_string(),
        });
    }
    let services: Vec<&Service> = model
        .services
        .iter()
        .filter(|service| service.crate_name == crate_name)
        .collect();
    let ports: Vec<&Port> = services
        .iter()
        .flat_map(|service| service.outbound.iter())
        .collect();
    // An inbound's own outbound ports — its loop's interior — render into the
    // inbound's crate the way a service's ports render into its own.
    let interior: Vec<&Inbound> = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.crate_name == crate_name)
        .collect();
    let interior_ports: Vec<&Port> = interior
        .iter()
        .flat_map(|inbound| inbound.outbound.iter())
        .collect();
    // A service-targeting port reuses its target service's trait, so only a
    // method-bearing port contributes a trait of its own.
    let port_traits: Vec<TokenStream> = ports
        .iter()
        .chain(&interior_ports)
        .filter(|port| port.target.is_none())
        .map(|port| render_port_trait(port, model))
        .collect::<Result<_, _>>()?;
    // A port with a gated method carries a rendered write gate, so the
    // permission policy is a modeled fact of the method, never a wrapper to
    // keep in step by hand.
    let port_gates: Vec<TokenStream> = ports
        .iter()
        .chain(&interior_ports)
        .filter(|port| port.target.is_none())
        .map(|port| contract::render_port_gate(port, model))
        .collect::<Result<_, _>>()?;
    // A modeled turn budget renders as the loop's constant, so the budget is a
    // patchable fact of the model. A crate hosting one loop names its budget
    // plainly; a crate hosting several prefixes each with its inbound's name,
    // so the constants stay distinct.
    let turned: Vec<(&&Inbound, u32)> = interior
        .iter()
        .filter_map(|inbound| inbound.turns.map(|turns| (inbound, turns)))
        .collect();
    let turn_budgets: Vec<TokenStream> = turned
        .iter()
        .map(|(inbound, turns)| {
            let name = if turned.len() == 1 {
                format_ident!("TURN_BUDGET")
            } else {
                format_ident!("{}_TURN_BUDGET", snake_case(&inbound.name).to_uppercase())
            };
            let value = proc_macro2::Literal::usize_unsuffixed(*turns as usize);
            let budget_doc = doc("The most turns the loop runs before giving up.");
            quote! {
                #budget_doc
                pub const #name: usize = #value;
            }
        })
        .collect();
    let composition_root = if ports.is_empty() {
        quote! {}
    } else {
        render_composition_root(&ports, model, crate_name)?
    };
    let standalone = if ports.is_empty() {
        quote! {}
    } else {
        contract::render_standalone(&services, &ports, model, crate_name)?
    };

    let service_traits: Vec<TokenStream> = services
        .iter()
        .map(|service| render_service_trait(service, model))
        .collect::<Result<_, _>>()?;
    // The typed error the trait defaults return, rendered once beside the traits
    // so the generated contract stays self-contained.
    let unimplemented = if service_traits.is_empty() && interior_ports.is_empty() {
        quote! {}
    } else {
        render_unimplemented()
    };
    // The gate's error renders wherever a contract or a rendered gate can
    // refuse: with a service's traits, or beside a gated port.
    let has_gate = ports
        .iter()
        .chain(&interior_ports)
        .any(|port| port.methods.iter().any(|method| method.gated));
    let refused = if service_traits.is_empty() && !has_gate {
        quote! {}
    } else {
        contract::render_refused()
    };
    // A service driven by an agent or MCP inbound carries a tool catalog and the
    // dispatch behind it, both rendered from each exposed operation's contract,
    // so every catalog entry has a dispatch arm.
    let agent_service = services.iter().find(|service| {
        model.inbounds.iter().any(|inbound| {
            inbound.service == service.name
                && matches!(inbound.transport, Transport::Agent | Transport::Mcp)
        })
    });
    let tool_operations: Vec<&Operation> = agent_service
        .map(|service| {
            service
                .operations
                .iter()
                .filter(|op| op.tool.is_some())
                .collect()
        })
        .unwrap_or_default();
    let has_tool_catalog = agent_service.is_some() && !tool_operations.is_empty();
    let tool_catalog = match agent_service {
        Some(service) if !tool_operations.is_empty() => {
            let catalog = render_tool_catalog(&tool_operations, model);
            let dispatch = render_tool_dispatch(&tool_operations, service, model)?;
            quote! { #catalog #dispatch }
        }
        _ => quote! {},
    };
    let requests = render_request_structs(&services, &interior_ports, model)?;
    // An inbound adapter hosted in this crate renders its wire surface, even when
    // the service it drives lives in another crate: a CLI inbound the command
    // surface, request parsers, parsed invocation, and dispatch; an HTTP inbound
    // the operation handlers with their status map. An agent or MCP inbound runs
    // in an authored binary over the tool catalog and dispatch above.
    let inbound_modules: Vec<TokenStream> = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.crate_name == crate_name)
        .map(|inbound| {
            let service = model.service_named(&inbound.service).ok_or_else(|| {
                RenderError::ServiceNotModeled {
                    service: inbound.service.clone(),
                }
            })?;
            Ok(match inbound.transport {
                Transport::Cli => Some(render_inbound_module(inbound, service, model)?),
                Transport::Http => Some(render_http_module(inbound, service, model)?),
                Transport::Grpc => Some(render_grpc_module(inbound, service, model)?),
                _ => None,
            })
        })
        .collect::<Result<Vec<_>, RenderError>>()?
        .into_iter()
        .flatten()
        .collect();
    // A client adapter hosted in this crate renders the target service's contract
    // carried over its transport — the mirror of an inbound's surface.
    let client_modules: Vec<TokenStream> = model
        .clients
        .iter()
        .filter(|client| client.crate_name == crate_name)
        .map(|client| {
            let service = model.service_named(&client.service).ok_or_else(|| {
                RenderError::ServiceNotModeled {
                    service: client.service.clone(),
                }
            })?;
            Ok(match client.transport {
                Transport::Http => Some(render_http_client_module(client, service, model)?),
                Transport::Grpc => Some(render_grpc_client_module(client, service, model)?),
                _ => None,
            })
        })
        .collect::<Result<Vec<_>, RenderError>>()?
        .into_iter()
        .flatten()
        .collect();
    let hosts = |transport: Transport| {
        model
            .inbounds
            .iter()
            .any(|inbound| inbound.crate_name == crate_name && inbound.transport == transport)
    };
    let has_cli = hosts(Transport::Cli);
    let has_cli_arguments = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.crate_name == crate_name && inbound.transport == Transport::Cli)
        .filter_map(|inbound| model.service_named(&inbound.service))
        .flat_map(|service| &service.operations)
        .any(|operation| !request_fields(operation, model).is_empty());
    let has_http = hosts(Transport::Http);
    let has_grpc = hosts(Transport::Grpc);
    let serves = |transport: Transport| {
        model
            .clients
            .iter()
            .any(|client| client.crate_name == crate_name && client.transport == transport)
    };
    let has_http_client = serves(Transport::Http);
    let has_grpc_client = serves(Transport::Grpc);

    // The command surface and its parsers carry the only command-line dependency.
    // A crate without a CLI inbound adapter imports none of it.
    let command_import = if has_cli_arguments {
        quote! { use clap::{Arg, ArgAction, ArgMatches, Command}; }
    } else if has_cli {
        quote! { use clap::{ArgMatches, Command}; }
    } else {
        quote! {}
    };

    let tokens = quote! {
        #command_import

        #(#port_traits)*
        #(#port_gates)*
        #(#turn_budgets)*
        #composition_root
        #requests
        #unimplemented
        #refused
        #(#service_traits)*
        #standalone
        #tool_catalog
        #(#inbound_modules)*
        #(#client_modules)*
    };

    let file = syn::parse2(tokens).map_err(|error| RenderError::InvalidRust {
        crate_name: crate_name.to_string(),
        message: error.to_string(),
    })?;
    validate_rust_names(&file)?;
    let body = space_items(&prettyplease::unparse(&file));

    let mut out = String::from("// @generated by `theseus generate` — do not edit by hand.\n");
    out.push_str(&format!(
        "//! Theseus's generated scaffolding: {}.\n\n",
        module_doc_summary(
            &services,
            &ports,
            &Surfaces {
                cli: has_cli,
                http: has_http,
                grpc: has_grpc,
                http_client: has_http_client,
                grpc_client: has_grpc_client,
                tool_catalog: has_tool_catalog,
                interior: !interior_ports.is_empty() || !turn_budgets.is_empty(),
            }
        )
    ));
    out.push_str(&body);
    Ok(out)
}

/// Reject duplicate identifiers that Rust's parser accepts but name resolution
/// would reject later. Walking the emitted syntax catches collisions introduced
/// by case conversion and fixed helper names without duplicating renderer rules.
fn validate_rust_names(file: &syn::File) -> Result<(), RenderError> {
    validate_item_names(&file.items)
}

fn validate_item_names(items: &[syn::Item]) -> Result<(), RenderError> {
    let mut top_level = [
        "Send", "Sync", "Sized", "Default", "Some", "None", "Ok", "Err",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    for item in items {
        if let syn::Item::Use(item) = item {
            collect_use_names(&item.tree, &mut top_level)?;
        }
        let name = match item {
            syn::Item::Const(item) => Some(item.ident.to_string()),
            syn::Item::Enum(item) => Some(item.ident.to_string()),
            syn::Item::Fn(item) => Some(item.sig.ident.to_string()),
            syn::Item::Mod(item) => Some(item.ident.to_string()),
            syn::Item::Static(item) => Some(item.ident.to_string()),
            syn::Item::Struct(item) => Some(item.ident.to_string()),
            syn::Item::Trait(item) => Some(item.ident.to_string()),
            syn::Item::Type(item) => Some(item.ident.to_string()),
            syn::Item::Union(item) => Some(item.ident.to_string()),
            _ => None,
        };
        if let Some(name) = name {
            ensure_unique_name(&mut top_level, "top-level item", name)?;
        }

        match item {
            syn::Item::Enum(item) => {
                let mut names = Vec::new();
                for variant in &item.variants {
                    ensure_unique_name(&mut names, "enum variant", variant.ident.to_string())?;
                    validate_field_names(&variant.fields)?;
                }
            }
            syn::Item::Struct(item) => validate_field_names(&item.fields)?,
            syn::Item::Trait(item) => {
                let mut names = Vec::new();
                for member in &item.items {
                    if let syn::TraitItem::Fn(method) = member {
                        ensure_unique_name(
                            &mut names,
                            "trait method",
                            method.sig.ident.to_string(),
                        )?;
                    }
                }
            }
            syn::Item::Impl(item) => {
                let mut names = Vec::new();
                for member in &item.items {
                    if let syn::ImplItem::Fn(method) = member {
                        ensure_unique_name(
                            &mut names,
                            "impl method",
                            method.sig.ident.to_string(),
                        )?;
                    }
                }
            }
            syn::Item::Mod(item) => {
                if let Some((_, items)) = &item.content {
                    validate_item_names(items)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn collect_use_names(tree: &syn::UseTree, names: &mut Vec<String>) -> Result<(), RenderError> {
    match tree {
        syn::UseTree::Name(name) => {
            ensure_unique_name(names, "top-level item", name.ident.to_string())
        }
        syn::UseTree::Rename(rename) => {
            ensure_unique_name(names, "top-level item", rename.rename.to_string())
        }
        syn::UseTree::Path(path) => collect_use_names(&path.tree, names),
        syn::UseTree::Group(group) => {
            for item in &group.items {
                collect_use_names(item, names)?;
            }
            Ok(())
        }
        syn::UseTree::Glob(_) => Ok(()),
    }
}

fn validate_field_names(fields: &syn::Fields) -> Result<(), RenderError> {
    let mut names = Vec::new();
    for field in fields {
        if let Some(ident) = &field.ident {
            ensure_unique_name(&mut names, "field", ident.to_string())?;
        }
    }
    Ok(())
}

/// The wire surfaces a crate hosts, gathered for the module doc summary.
struct Surfaces {
    interior: bool,
    cli: bool,
    http: bool,
    grpc: bool,
    http_client: bool,
    grpc_client: bool,
    tool_catalog: bool,
}

/// Summarize what a crate's generated file holds, naming only the parts present.
fn module_doc_summary(services: &[&Service], ports: &[&Port], surfaces: &Surfaces) -> String {
    let mut concerns: Vec<&str> = Vec::new();
    if !services.is_empty() {
        concerns.push("the request types and service contract");
    }
    if !ports.is_empty() {
        concerns.push("the outbound port traits and composition roots");
    }
    if surfaces.interior {
        concerns.push("the loop's port contract and turn budget");
    }
    if surfaces.tool_catalog {
        concerns.push("the agent tool catalog and dispatch");
    }
    if surfaces.cli {
        concerns.push("the command surface, request parsers, invocation, and dispatch");
    }
    if surfaces.http {
        concerns.push("the HTTP operation handlers and their status map");
    }
    if surfaces.grpc {
        concerns.push("the gRPC service glue and its status map");
    }
    if surfaces.http_client {
        concerns.push("the HTTP client over the service contract");
    }
    if surfaces.grpc_client {
        concerns.push("the gRPC client over the service contract");
    }
    concerns.join(", ")
}

/// Insert a blank line before each item and member, so the rendered output reads
/// like hand-written Rust. The formatter sets items flush against each other. A
/// blank line goes before a line that opens an item — a doc comment, an attribute,
/// or an item keyword — when the previous line closes one by ending in `}` or `;`.
/// Struct fields, enum variants, and match arms end in `,`, so they stay compact.
pub(crate) fn space_items(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut prev_closes_item = false;
    for line in body.lines() {
        if prev_closes_item && opens_item(line) {
            out.push('\n');
        }
        out.push_str(line);
        out.push('\n');
        let trimmed = line.trim_end();
        prev_closes_item = trimmed.ends_with('}') || trimmed.ends_with(';');
    }
    out
}

/// Whether a line opens an item or member: a doc comment, an attribute, or an
/// item keyword.
fn opens_item(line: &str) -> bool {
    let line = line.trim_start();
    const KEYWORDS: [&str; 22] = [
        "use ",
        "pub use ",
        "fn ",
        "pub fn ",
        "async fn ",
        "pub async fn ",
        "struct ",
        "pub struct ",
        "enum ",
        "pub enum ",
        "trait ",
        "pub trait ",
        "impl ",
        "impl<",
        "const ",
        "pub const ",
        "type ",
        "pub type ",
        "mod ",
        "pub mod ",
        "static ",
        "pub static ",
    ];
    line.starts_with("///")
        || line.starts_with("#[")
        || KEYWORDS.iter().any(|keyword| line.starts_with(keyword))
}

/// A `#[doc = " ..."]` attribute. `prettyplease` renders it as a `///` line.
/// Empty text renders nothing, so an undocumented field carries no doc line.
fn doc(text: &str) -> TokenStream {
    if text.is_empty() {
        return quote! {};
    }
    let line = format!(" {text}");
    quote! { #[doc = #line] }
}

/// The fields of an operation's request type. They become its CLI arguments. An
/// `Empty` or undefined request contributes none.
fn request_fields<'a>(op: &Operation, model: &'a Model) -> &'a [Field] {
    match model.type_def(&op.request).map(|def| &def.shape) {
        Some(TypeShape::Struct(fields)) => fields,
        _ => &[],
    }
}

/// The request type an operation takes, if its request names a defined struct.
/// `Empty` and undefined requests yield `None`, so the operation takes no request.
fn request_type<'a>(op: &Operation, model: &'a Model) -> Option<&'a TypeDef> {
    if op.request == "Empty" {
        return None;
    }
    model
        .type_def(&op.request)
        .filter(|def| matches!(def.shape, TypeShape::Struct(_)))
}

/// Render one field's extraction from a tool call's JSON input. An absent `bool`
/// defaults false, but a present value must actually be boolean. A `String` is
/// required, mirroring the schema's `required` list. A container defaults empty
/// when absent, and any other type deserializes from the field's value — an
/// `Option` reads absence as `None`.
fn tool_field_init(field: &Field) -> TokenStream {
    let name = format_ident!("{}", field.name);
    let key = field.name.as_str();
    if field.ty == "bool" {
        let message = format!("the `{key}` field is invalid: expected a boolean");
        return quote! {
            #name: match input.get(#key) {
                None => false,
                Some(value) => value
                    .as_bool()
                    .ok_or_else(|| anyhow::anyhow!(#message))?,
            }
        };
    }
    if field.ty == "String" {
        let message = format!("the call needs a string `{key}`");
        return quote! {
            #name: input
                .get(#key)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| anyhow::anyhow!(#message))?
        };
    }
    let message = format!("the `{key}` field is invalid: {{error}}");
    if field.ty.starts_with("Vec<") || field.ty.starts_with("BTreeMap<") {
        return quote! {
            #name: match input.get(#key) {
                None => Default::default(),
                Some(value) => serde_json::from_value(value.clone())
                    .map_err(|error| anyhow::anyhow!(#message))?,
            }
        };
    }
    quote! {
        #name: serde_json::from_value(
            input.get(#key).cloned().unwrap_or(serde_json::Value::Null),
        )
        .map_err(|error| anyhow::anyhow!(#message))?
    }
}

/// The snake-case form the proto build gives a service's modules, matched by
/// construction: a word boundary where lowercase meets uppercase, and where an
/// uppercase run ends before a lowercase letter, so an acronym stays one word.
fn proto_snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if ch == '-' || ch == '_' {
            out.push('_');
            continue;
        }
        if ch.is_uppercase() {
            let boundary = match index.checked_sub(1).map(|i| chars[i]) {
                None | Some('-') | Some('_') => false,
                Some(prev) if prev.is_lowercase() || prev.is_ascii_digit() => true,
                Some(_) => chars.get(index + 1).is_some_and(|next| next.is_lowercase()),
            };
            if boundary {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// The paths an adapter names a service's contract with: the crate prefix for
/// its types (empty when the adapter shares the service's crate, otherwise the
/// service's crate module path followed by `::`), the service trait, and the
/// boundary error types every transport maps.
struct ContractPaths {
    prefix: String,
    service_trait: syn::Type,
    unimplemented: syn::Type,
    refused: syn::Type,
}

fn contract_paths(
    host_crate: &str,
    service: &Service,
    model: &Model,
) -> Result<ContractPaths, RenderError> {
    let prefix = host_path_prefix(host_crate, service, model)?;
    Ok(ContractPaths {
        service_trait: syn_type(&format!("{prefix}{}Service", pascal_case(&service.name)))?,
        unimplemented: syn_type(&format!("{prefix}Unimplemented"))?,
        refused: syn_type(&format!("{prefix}Refused"))?,
        prefix,
    })
}

/// How an operation's response crosses a wire: nothing, a string carried as
/// text, or a value carried as its JSON rendering. The one rule every
/// transport's renderer reads.
enum ResponseKind {
    Empty,
    Text,
    Json,
}

fn response_kind(op: &Operation, model: &Model) -> ResponseKind {
    if op.response == "Empty" {
        ResponseKind::Empty
    } else if rust_type(&op.response, model) == "String" {
        ResponseKind::Text
    } else {
        ResponseKind::Json
    }
}

/// The distinct request structs a set of operations takes, in first-use
/// order — the set a surface renders one parser for.
fn distinct_request_types<'a>(operations: &[&Operation], model: &'a Model) -> Vec<&'a TypeDef> {
    let mut seen: Vec<&str> = Vec::new();
    let mut types = Vec::new();
    for op in operations {
        if let Some(def) = request_type(op, model)
            && !seen.contains(&def.name.as_str())
        {
            seen.push(&def.name);
            types.push(def);
        }
    }
    types
}

/// Render a parser per distinct request struct, building the request from a
/// call's JSON input — the one wire-to-domain conversion every JSON transport
/// renders. `prefix` qualifies the struct where the adapter lives outside the
/// service's crate, `suffix` keeps each surface's parser names distinct, and
/// `public` widens the visibility for a surface whose callers live in sibling
/// modules.
fn render_json_parsers(
    operations: &[&Operation],
    model: &Model,
    prefix: &str,
    suffix: &str,
    public: bool,
) -> Result<TokenStream, RenderError> {
    let parsers: Vec<TokenStream> = distinct_request_types(operations, model)
        .into_iter()
        .map(|def| -> Result<TokenStream, RenderError> {
            let TypeShape::Struct(fields) = &def.shape else {
                return Ok(quote! {});
            };
            let fn_name = format_ident!("parse_{}_{suffix}", proto_snake_case(&def.name));
            let ty = syn_type(&format!("{prefix}{}", def.name))?;
            let vis = if public {
                quote! { pub(crate) }
            } else {
                quote! {}
            };
            let inits: Vec<TokenStream> = fields.iter().map(tool_field_init).collect();
            Ok(quote! {
                #vis fn #fn_name(input: &serde_json::Value) -> anyhow::Result<#ty> {
                    Ok(#ty { #(#inits),* })
                }
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(quote! { #(#parsers)* })
}

/// The binding a JSON dispatch takes its input by: named when any operation
/// parses a request from it, underscored when none does.
fn request_binding(operations: &[&Operation], model: &Model) -> proc_macro2::Ident {
    if operations
        .iter()
        .any(|op| request_type(op, model).is_some())
    {
        format_ident!("input")
    } else {
        format_ident!("_input")
    }
}

/// The crate-path prefix an adapter hosted in `host_crate` names the service's
/// types with: empty when it shares the service's crate, otherwise the service
/// crate's module path followed by `::`.
fn host_path_prefix(
    host_crate: &str,
    service: &Service,
    model: &Model,
) -> Result<String, RenderError> {
    if host_crate == service.crate_name {
        Ok(String::new())
    } else {
        let module = model
            .crate_named(&service.crate_name)
            .map(|node| node.name.replace('-', "_"))
            .ok_or_else(|| RenderError::CrateNotModeled {
                crate_name: service.crate_name.clone(),
            })?;
        validate_identifier("crate module", &module)?;
        Ok(format!("{module}::"))
    }
}

/// The response type a method returns, as a parsed Rust type.
fn response_type(label: &str, model: &Model) -> Result<TokenStream, RenderError> {
    let ty = syn_type(&resolve_field_type(label, model))?;
    Ok(quote! { #ty })
}

/// Render a port method's adapter signature, for splicing an authored body.
/// The parameter and response resolve to absolute types through
/// `request_path`, so the spliced method needs no imports.
pub(crate) fn adapter_signature(method: &Method, model: &Model, request_path: &str) -> String {
    let param = if method.request == "Empty" {
        String::new()
    } else if method.request == "String" {
        ", request: &str".to_string()
    } else {
        format!(
            ", request: &{}",
            absolute_type(&method.request, model, request_path)
        )
    };
    format!(
        "async fn {}(&self{param}) -> anyhow::Result<{}>",
        method.name,
        absolute_type(&method.response, model, request_path)
    )
}

/// A label's Rust type with an absolute path: a struct or enum the model owns
/// is prefixed with `request_path`, and everything else already resolves to a
/// full path or a builtin.
fn absolute_type(label: &str, model: &Model, request_path: &str) -> String {
    if let Some(inner) = vec_inner(label) {
        return format!("Vec<{}>", absolute_type(inner, model, request_path));
    }
    if let Some(inner) = optional_inner(label) {
        return format!("Option<{}>", absolute_type(inner, model, request_path));
    }
    if let Some(value) = map_value(label) {
        return format!(
            "std::collections::BTreeMap<String, {}>",
            absolute_type(value, model, request_path)
        );
    }
    let resolved = rust_type(label, model);
    let local = model
        .type_def(label)
        .is_some_and(|def| matches!(def.shape, TypeShape::Struct(_) | TypeShape::Enum { .. }));
    if local {
        format!("{request_path}{resolved}")
    } else {
        resolved
    }
}

/// Render an operation's handler signature, for splicing an authored body. The
/// request is named through `request_path` and the response resolves to an
/// absolute type, so the spliced method needs no imports.
pub(crate) fn handler_signature(op: &Operation, model: &Model, request_path: &str) -> String {
    let param = match request_type(op, model) {
        Some(def) => format!(", request: {request_path}{}", def.name),
        None => String::new(),
    };
    let response = absolute_type(&op.response, model, request_path);
    format!(
        "async fn {}(&self{param}) -> anyhow::Result<{response}>",
        op.name
    )
}

/// The `, request: &T` fragment for a method, or empty for an `Empty` request.
/// `String` requests borrow as `&str`, the idiomatic borrowed form. The
/// underscore form suits a defaulted declaration; the bound form binds the
/// value a forwarding body passes on.
fn request_param(label: &str, model: &Model) -> Result<TokenStream, RenderError> {
    typed_request_param(label, model, format_ident!("_request"))
}

/// The bound `, request: &T` fragment for a forwarding method body.
fn bound_request_param(label: &str, model: &Model) -> Result<TokenStream, RenderError> {
    typed_request_param(label, model, format_ident!("request"))
}

fn typed_request_param(
    label: &str,
    model: &Model,
    name: proc_macro2::Ident,
) -> Result<TokenStream, RenderError> {
    if label == "Empty" {
        return Ok(quote! {});
    }
    let ty = resolve_field_type(label, model);
    if ty == "String" {
        Ok(quote! { , #name: &str })
    } else {
        let ty = syn_type(&ty)?;
        Ok(quote! { , #name: &#ty })
    }
}

/// The Rust type a model type label maps to. A struct or enum resolves to its
/// local name, rendered in the same module. A newtype or foreign type resolves to
/// its target path, and `Empty` and `String` to the builtin.
fn rust_type(label: &str, model: &Model) -> String {
    match label {
        "Empty" => "()".to_string(),
        "String" => "String".to_string(),
        other => match model.types.iter().find(|t| t.name == other) {
            Some(def) => match &def.shape {
                TypeShape::Newtype(inner) => inner.clone(),
                TypeShape::Foreign(path) => path.clone(),
                // An enum standing for an existing Rust type resolves to that path.
                TypeShape::Enum {
                    rust: Some(path), ..
                } => path.clone(),
                TypeShape::Struct(_) | TypeShape::Enum { .. } => other.to_string(),
            },
            None => other.to_string(),
        },
    }
}

/// Resolve a field type label to its Rust type, resolving the element of a
/// `Vec<…>` or `Option<…>` through [`rust_type`] so a model type inside a
/// container names its real path.
fn resolve_field_type(label: &str, model: &Model) -> String {
    if let Some(inner) = vec_inner(label) {
        return format!("Vec<{}>", resolve_field_type(inner, model));
    }
    if let Some(inner) = optional_inner(label) {
        return format!("Option<{}>", resolve_field_type(inner, model));
    }
    if let Some(value) = map_value(label) {
        return format!(
            "std::collections::BTreeMap<String, {}>",
            resolve_field_type(value, model)
        );
    }
    rust_type(label, model)
}

/// Parse a rendered type string into a token type, e.g. `()` or `Option<String>`.
fn syn_type(text: &str) -> Result<syn::Type, RenderError> {
    syn::parse_str(text).map_err(|error| RenderError::InvalidRustType {
        label: text.to_string(),
        message: error.to_string(),
    })
}

pub(crate) fn pascal_case(name: &str) -> String {
    name.split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn snake_case(name: &str) -> String {
    name.replace('-', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_helpers() {
        assert_eq!(pascal_case("source-store"), "SourceStore");
        assert_eq!(snake_case("source-store"), "source_store");
    }

    #[test]
    fn present_tool_booleans_are_type_checked() {
        let rendered = tool_field_init(&Field {
            name: "write".to_string(),
            ty: "bool".to_string(),
            doc: "Apply the change.".to_string(),
        })
        .to_string();

        assert!(rendered.contains("None => false"), "{rendered}");
        assert!(rendered.contains("as_bool"), "{rendered}");
        assert!(rendered.contains("expected a boolean"), "{rendered}");
        assert!(!rendered.contains("unwrap_or_default"), "{rendered}");
    }

    #[test]
    fn an_unsupported_proto_scalar_is_a_render_error() {
        let model = Model::new("App")
            .struct_type("Payload", &[("value", "i128", "The value.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Payload", "Empty"),
            );
        let service = model.service_named("App").expect("service is modeled");

        assert_eq!(
            render_proto(&model, service),
            Err(RenderError::UnsupportedGrpcFieldType {
                field: "value".to_string(),
                ty: "i128".to_string(),
            })
        );

        let nested_container = Model::new("App")
            .struct_type(
                "Payload",
                &[("values", "Option<Vec<String>>", "The values.")],
            )
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Payload", "Empty"),
            );
        let service = nested_container
            .service_named("App")
            .expect("service is modeled");
        assert!(matches!(
            render_proto(&nested_container, service),
            Err(RenderError::InvalidProto { .. })
        ));
    }

    #[test]
    fn an_unsupported_cli_container_is_a_render_error() {
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .struct_type(
                "Payload",
                &[("values", "Option<Vec<String>>", "The values.")],
            )
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Payload", "Empty"),
            )
            .inbound("cli", Transport::Cli, "App", "app");

        assert_eq!(
            render_cli_module(&model),
            Err(RenderError::UnsupportedCliFieldType {
                field: "values".to_string(),
                ty: "Option<Vec<String>>".to_string(),
            })
        );

        let alias = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .newtype("Count", "u32")
            .struct_type("Payload", &[("count", "Count", "The count.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Payload", "Empty"),
            )
            .inbound("cli", Transport::Cli, "App", "app");
        let rendered = render_cli_module(&alias).expect("CLI alias renders");
        assert!(rendered.contains("clap::value_parser!(u32)"));
        assert!(rendered.contains("get_one::<u32>"));

        let import_collision = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .struct_type("Command", &[("value", "String", "The value.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Command", "Empty"),
            )
            .inbound("cli", Transport::Cli, "App", "app");
        assert_eq!(
            render_cli_module(&import_collision),
            Err(RenderError::NameCollision {
                namespace: "top-level item",
                name: "Command".to_string(),
            })
        );

        assert_eq!(
            render_module_for_crate(&alias, "missing"),
            Err(RenderError::CrateNotModeled {
                crate_name: "missing".to_string(),
            })
        );
    }

    #[test]
    fn a_nested_grpc_request_is_a_render_error() {
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .crate_node("app-grpc", "app-grpc", 1, &["app"])
            .struct_type("Inner", &[("value", "String", "The value.")])
            .struct_type("Request", &[("inner", "Inner", "The nested value.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Request", "Empty"),
            )
            .inbound("grpc", Transport::Grpc, "App", "app-grpc");

        assert_eq!(
            render_module_for_crate(&model, "app-grpc"),
            Err(RenderError::UnsupportedNestedStructType {
                field: "inner".to_string(),
                ty: "Inner".to_string(),
            })
        );

        let optional_map = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .crate_node("app-grpc", "app-grpc", 1, &["app"])
            .struct_type(
                "Request",
                &[("values", "Option<BTreeMap<String, String>>", "The values.")],
            )
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Request", "Empty"),
            )
            .inbound("grpc", Transport::Grpc, "App", "app-grpc");
        assert!(matches!(
            render_module_for_crate(&optional_map, "app-grpc"),
            Err(RenderError::UnsupportedGrpcConversion { reason, .. })
                if reason == "optional maps have no distinct protobuf representation"
        ));
    }

    #[test]
    fn invalid_rust_tokens_are_render_errors() {
        let bad_identifier = Model::new("App").service(
            Service::new("App")
                .crate_name("app")
                .operation("match", "Reserved.", "Empty", "Empty"),
        );
        assert!(matches!(
            render_module_for_crate(&bad_identifier, "app"),
            Err(RenderError::InvalidIdentifier {
                kind: "operation",
                ..
            })
        ));

        let bad_type = Model::new("App")
            .foreign_type("Reply", "not a type")
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("reply", "Reply.", "Empty", "Reply"),
            );
        assert!(matches!(
            render_module_for_crate(&bad_type, "app"),
            Err(RenderError::InvalidRustType { label, .. }) if label == "not a type"
        ));

        let scalar_request = Model::new("App").service(
            Service::new("App")
                .crate_name("app")
                .operation("send", "Send.", "String", "Empty"),
        );
        assert!(matches!(
            render_module_for_crate(&scalar_request, "app"),
            Err(RenderError::InvalidRequestType { request, .. }) if request == "String"
        ));

        let unresolved = Model::new("App").service(
            Service::new("App")
                .crate_name("app")
                .operation("send", "Send.", "Empty", "Missing"),
        );
        assert_eq!(
            render_module_for_crate(&unresolved, "app"),
            Err(RenderError::UnresolvedType {
                label: "Missing".to_string(),
            })
        );

        let container_alias = Model::new("App")
            .foreign_type("Reply", "crate::Reply")
            .service(Service::new("App").crate_name("app").operation(
                "list",
                "List.",
                "Empty",
                "Vec<Reply>",
            ));
        assert!(
            render_module_for_crate(&container_alias, "app")
                .expect("container alias renders")
                .contains("anyhow::Result<Vec<crate::Reply>>")
        );

        let owned_enum = Model::new("App")
            .enum_type("Status", &["Ready", "Busy"])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("status", "Status.", "Empty", "Status"),
            );
        assert_eq!(
            render_module_for_crate(&owned_enum, "app"),
            Err(RenderError::UnsupportedModeledType {
                ty: "Status".to_string(),
                reason: "model-owned enums are not implemented",
            })
        );

        let builtin_shadow = Model::new("App")
            .struct_type("String", &[("value", "String", "The value.")])
            .service(Service::new("App").crate_name("app"));
        assert_eq!(
            render_module_for_crate(&builtin_shadow, "app"),
            Err(RenderError::NameCollision {
                namespace: "builtin type",
                name: "String".to_string(),
            })
        );

        for name in [
            "Send", "Sync", "Sized", "Default", "Some", "None", "Ok", "Err",
        ] {
            let prelude_shadow = Model::new("App")
                .struct_type(name, &[("value", "String", "The value.")])
                .service(
                    Service::new("App")
                        .crate_name("app")
                        .operation("run", "Run.", name, "Empty"),
                );
            assert_eq!(
                render_module_for_crate(&prelude_shadow, "app"),
                Err(RenderError::NameCollision {
                    namespace: "builtin type",
                    name: name.to_string(),
                })
            );
        }

        let port_shadow = Model::new("App").service(
            Service::new("App")
                .crate_name("app")
                .port(Port::new("send", "A renderer-reserved trait name.")),
        );
        assert_eq!(
            render_module_for_crate(&port_shadow, "app"),
            Err(RenderError::NameCollision {
                namespace: "top-level item",
                name: "Send".to_string(),
            })
        );

        let struct_response = Model::new("App")
            .struct_type("Reply", &[("message", "String", "The reply.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("reply", "Reply.", "Empty", "Reply"),
            );
        assert_eq!(
            render_module_for_crate(&struct_response, "app"),
            Err(RenderError::UnsupportedResponseType {
                service: "App".to_string(),
                operation: "reply".to_string(),
                response: "Reply".to_string(),
            })
        );
    }

    #[test]
    fn an_invalid_proto_package_is_a_render_error() {
        let model = Model::new("bad-name").service(
            Service::new("App")
                .crate_name("app")
                .operation("run", "Run.", "Empty", "Empty"),
        );
        let service = model.service_named("App").expect("service is modeled");

        assert_eq!(
            render_proto(&model, service),
            Err(RenderError::InvalidProtoIdentifier {
                kind: "protobuf package segment",
                name: "bad-name".to_string(),
            })
        );

        let unicode_field = Model::new("App")
            .struct_type("Payload", &[("café", "String", "Not a proto identifier.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Payload", "Empty"),
            );
        let service = unicode_field
            .service_named("App")
            .expect("service is modeled");
        assert_eq!(
            render_proto(&unicode_field, service),
            Err(RenderError::InvalidProtoIdentifier {
                kind: "protobuf field",
                name: "café".to_string(),
            })
        );

        let contextual_keyword = Model::new("App")
            .struct_type("Payload", &[("repeated", "String", "A legal field name.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("send", "Send.", "Payload", "Empty"),
            );
        let service = contextual_keyword
            .service_named("App")
            .expect("service is modeled");
        assert!(
            render_proto(&contextual_keyword, service)
                .expect("contextual keywords render")
                .contains("string repeated = 1;")
        );
    }
}
