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

/// Render the generated scaffolding for the crate hosting the inbound CLI adapter.
pub fn render_cli_module(model: &Model) -> String {
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
pub fn render_module_for_crate(model: &Model, crate_name: &str) -> String {
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
        .collect();
    // A port with a gated method carries a rendered write gate, so the
    // permission policy is a modeled fact of the method, never a wrapper to
    // keep in step by hand.
    let port_gates: Vec<TokenStream> = ports
        .iter()
        .chain(&interior_ports)
        .filter(|port| port.target.is_none())
        .map(|port| contract::render_port_gate(port, model))
        .collect();
    // A modeled turn budget renders as the loop's constant, so the budget is a
    // patchable fact of the model. A crate hosting one loop names its budget
    // plainly; a crate hosting several prefixes each with its inbound's name,
    // so the constants stay distinct.
    let turned: Vec<&&Inbound> = interior
        .iter()
        .filter(|inbound| inbound.turns.is_some())
        .collect();
    let turn_budgets: Vec<TokenStream> = turned
        .iter()
        .map(|inbound| {
            let turns = inbound.turns.expect("filtered to turned inbounds");
            let name = if turned.len() == 1 {
                format_ident!("TURN_BUDGET")
            } else {
                format_ident!("{}_TURN_BUDGET", snake_case(&inbound.name).to_uppercase())
            };
            let value = proc_macro2::Literal::usize_unsuffixed(turns as usize);
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
        render_composition_root(&ports, model, crate_name)
    };
    let standalone = if ports.is_empty() {
        quote! {}
    } else {
        contract::render_standalone(&services, &ports, model, crate_name)
    };

    let service_traits: Vec<TokenStream> = services
        .iter()
        .map(|service| render_service_trait(service, model))
        .collect();
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
            let dispatch = render_tool_dispatch(&tool_operations, service, model);
            quote! { #catalog #dispatch }
        }
        _ => quote! {},
    };
    let requests = render_request_structs(&services, &interior_ports, model);
    // An inbound adapter hosted in this crate renders its wire surface, even when
    // the service it drives lives in another crate: a CLI inbound the command
    // surface, request parsers, parsed invocation, and dispatch; an HTTP inbound
    // the operation handlers with their status map. An agent or MCP inbound runs
    // in an authored binary over the tool catalog and dispatch above.
    let inbound_modules: Vec<TokenStream> = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.crate_name == crate_name)
        .filter_map(|inbound| {
            let service = model.service_named(&inbound.service)?;
            match inbound.transport {
                Transport::Cli => Some(render_inbound_module(inbound, service, model)),
                Transport::Http => Some(render_http_module(inbound, service, model)),
                Transport::Grpc => Some(render_grpc_module(inbound, service, model)),
                _ => None,
            }
        })
        .collect();
    // A client adapter hosted in this crate renders the target service's contract
    // carried over its transport — the mirror of an inbound's surface.
    let client_modules: Vec<TokenStream> = model
        .clients
        .iter()
        .filter(|client| client.crate_name == crate_name)
        .filter_map(|client| {
            let service = model.service_named(&client.service)?;
            match client.transport {
                Transport::Http => Some(render_http_client_module(client, service, model)),
                Transport::Grpc => Some(render_grpc_client_module(client, service, model)),
                _ => None,
            }
        })
        .collect();
    let hosts = |transport: Transport| {
        model
            .inbounds
            .iter()
            .any(|inbound| inbound.crate_name == crate_name && inbound.transport == transport)
    };
    let has_cli = hosts(Transport::Cli);
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
    let command_import = if has_cli {
        quote! { use clap::{Arg, ArgAction, ArgMatches, Command}; }
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

    let file = syn::parse2(tokens).unwrap_or_else(|error| {
        panic!("generated code for crate `{crate_name}` is not valid Rust: {error}")
    });
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
    out
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

/// Render one field's extraction from a tool call's JSON input. A `bool` defaults
/// false and a `String` is required, mirroring the schema's `required` list. A
/// container defaults empty when absent, and any other type deserializes from the
/// field's value — an `Option` reads absence as `None`.
fn tool_field_init(field: &Field) -> TokenStream {
    let name = format_ident!("{}", field.name);
    let key = field.name.as_str();
    if field.ty == "bool" {
        return quote! {
            #name: input.get(#key).and_then(serde_json::Value::as_bool).unwrap_or_default()
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

fn contract_paths(host_crate: &str, service: &Service, model: &Model) -> ContractPaths {
    let prefix = host_path_prefix(host_crate, service, model);
    ContractPaths {
        service_trait: syn_type(&format!("{prefix}{}Service", pascal_case(&service.name))),
        unimplemented: syn_type(&format!("{prefix}Unimplemented")),
        refused: syn_type(&format!("{prefix}Refused")),
        prefix,
    }
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
) -> TokenStream {
    let parsers: Vec<TokenStream> = distinct_request_types(operations, model)
        .into_iter()
        .map(|def| {
            let TypeShape::Struct(fields) = &def.shape else {
                return quote! {};
            };
            let fn_name = format_ident!("parse_{}_{suffix}", proto_snake_case(&def.name));
            let ty = syn_type(&format!("{prefix}{}", def.name));
            let vis = if public {
                quote! { pub(crate) }
            } else {
                quote! {}
            };
            let inits: Vec<TokenStream> = fields.iter().map(tool_field_init).collect();
            quote! {
                #vis fn #fn_name(input: &serde_json::Value) -> anyhow::Result<#ty> {
                    Ok(#ty { #(#inits),* })
                }
            }
        })
        .collect();
    quote! { #(#parsers)* }
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
fn host_path_prefix(host_crate: &str, service: &Service, model: &Model) -> String {
    if host_crate == service.crate_name {
        String::new()
    } else {
        let module = model
            .crate_named(&service.crate_name)
            .map(|node| node.name.replace('-', "_"))
            .unwrap_or_default();
        format!("{module}::")
    }
}

/// The response type a method returns, as a parsed Rust type.
fn response_type(label: &str, model: &Model) -> TokenStream {
    let ty = syn_type(&rust_type(label, model));
    quote! { #ty }
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
    let response = rust_type(&op.response, model);
    format!(
        "async fn {}(&self{param}) -> anyhow::Result<{response}>",
        op.name
    )
}

/// The `, request: &T` fragment for a method, or empty for an `Empty` request.
/// `String` requests borrow as `&str`, the idiomatic borrowed form. The
/// underscore form suits a defaulted declaration; the bound form binds the
/// value a forwarding body passes on.
fn request_param(label: &str, model: &Model) -> TokenStream {
    typed_request_param(label, model, format_ident!("_request"))
}

/// The bound `, request: &T` fragment for a forwarding method body.
fn bound_request_param(label: &str, model: &Model) -> TokenStream {
    typed_request_param(label, model, format_ident!("request"))
}

fn typed_request_param(label: &str, model: &Model, name: proc_macro2::Ident) -> TokenStream {
    if label == "Empty" {
        return quote! {};
    }
    let ty = rust_type(label, model);
    if ty == "String" {
        quote! { , #name: &str }
    } else {
        let ty = syn_type(&ty);
        quote! { , #name: &#ty }
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
    rust_type(label, model)
}

/// Parse a rendered type string into a token type, e.g. `()` or `Option<String>`.
fn syn_type(text: &str) -> syn::Type {
    syn::parse_str(text).unwrap_or_else(|error| {
        panic!("type label `{text}` does not parse as a Rust type: {error}")
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
}
