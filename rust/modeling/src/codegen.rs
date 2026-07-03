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
        Client, Field, Inbound, Model, Operation, Port, Service, Transport, TypeDef, TypeShape,
        Variant,
    },
};

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
    // A service-targeting port reuses its target service's trait, so only a
    // method-bearing port contributes a trait of its own.
    let port_traits: Vec<TokenStream> = ports
        .iter()
        .filter(|port| port.target.is_none())
        .map(|port| render_port_trait(port, model))
        .collect();
    let composition_root = if ports.is_empty() {
        quote! {}
    } else {
        render_composition_root(&ports, model, crate_name)
    };

    let service_traits: Vec<TokenStream> = services
        .iter()
        .map(|service| render_service_trait(service, model))
        .collect();
    // The typed error the trait defaults return, rendered once beside the traits
    // so the generated contract stays self-contained.
    let unimplemented = if service_traits.is_empty() {
        quote! {}
    } else {
        render_unimplemented()
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
    let requests = render_request_structs(&services, model);
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
        #composition_root
        #requests
        #unimplemented
        #(#service_traits)*
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
            }
        )
    ));
    out.push_str(&body);
    out
}

/// Summarize what a crate's generated file holds, naming only the parts present.
/// A service-hosting crate carries the request types and the service contract,
/// and with outbound dependencies the port traits and the composition root. A
/// crate hosting a CLI inbound carries the command surface, the request parsers,
/// the parsed invocation, and dispatch.
/// The wire surfaces a crate hosts, gathered for the module doc summary.
struct Surfaces {
    cli: bool,
    http: bool,
    grpc: bool,
    http_client: bool,
    grpc_client: bool,
    tool_catalog: bool,
}

fn module_doc_summary(services: &[&Service], ports: &[&Port], surfaces: &Surfaces) -> String {
    let mut concerns: Vec<&str> = Vec::new();
    if !services.is_empty() {
        concerns.push("the request types and service contract");
    }
    if !ports.is_empty() {
        concerns.push("the outbound port traits and composition root");
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
fn doc(text: &str) -> TokenStream {
    let line = format!(" {text}");
    quote! { #[doc = #line] }
}

/// Render one operation as a subcommand, its request fields as arguments.
fn render_subcommand(op: &Operation, model: &Model) -> TokenStream {
    let name = &op.name;
    let summary = &op.summary;
    let args: Vec<TokenStream> = request_fields(op, model).iter().map(render_arg).collect();
    quote! {
        .subcommand(Command::new(#name).about(#summary) #(#args)*)
    }
}

/// The fields of an operation's request type. They become its CLI arguments. An
/// `Empty` or undefined request contributes none.
fn request_fields<'a>(op: &Operation, model: &'a Model) -> &'a [Field] {
    match model.type_def(&op.request).map(|def| &def.shape) {
        Some(TypeShape::Struct(fields)) => fields,
        _ => &[],
    }
}

/// Render one request field as a command-line argument. The field type decides
/// the shape: `bool` is a flag, `Vec<T>` a repeatable value, `Option<T>` an
/// optional value, anything else a required value. A non-`String` value type is
/// parsed and validated as that type.
fn render_arg(field: &Field) -> TokenStream {
    let flag = field.name.replace('_', "-");
    let help = &field.doc;
    if field.ty == "bool" {
        quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::SetTrue).help(#help)) }
    } else if field.ty.starts_with("Vec<") {
        quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::Append).help(#help)) }
    } else if let Some(inner) = optional_inner(&field.ty) {
        let parser = value_parser(inner);
        quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::Set) #parser .help(#help)) }
    } else {
        let parser = value_parser(&field.ty);
        quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::Set).required(true) #parser .help(#help)) }
    }
}

/// The `.value_parser(...)` fragment for a typed argument. A `String` value needs
/// none. Any other value type is parsed and validated as that type.
fn value_parser(ty: &str) -> TokenStream {
    if ty == "String" {
        quote! {}
    } else {
        let ty = syn_type(ty);
        quote! { .value_parser(clap::value_parser!(#ty)) }
    }
}

/// Render one outbound port as a trait. The hand-written adapter implements it.
fn render_port_trait(port: &Port, model: &Model) -> TokenStream {
    let trait_name = format_ident!("{}", pascal_case(&port.name));
    let trait_doc = doc(&port.summary);
    let methods: Vec<TokenStream> = port
        .methods
        .iter()
        .map(|method| {
            let method_doc = doc(&method.summary);
            let method_name = format_ident!("{}", method.name);
            let param = request_param(&method.request, model);
            let response = response_type(&method.response, model);
            quote! {
                #method_doc
                async fn #method_name(&self #param) -> anyhow::Result<#response>;
            }
        })
        .collect();
    quote! {
        #trait_doc
        #[async_trait::async_trait]
        pub trait #trait_name: Send + Sync {
            #(#methods)*
        }
    }
}

/// Render the composition root: the model plus one field per wired port.
fn render_composition_root(ports: &[&Port], model: &Model, current_crate: &str) -> TokenStream {
    let fields: Vec<TokenStream> = ports
        .iter()
        .map(|port| {
            let field = format_ident!("{}", snake_case(&port.name));
            let trait_path = port_trait_path(port, model, current_crate);
            quote! { pub #field: &'a dyn #trait_path, }
        })
        .collect();
    let doc = doc("Composition root: the model plus the wired outbound ports.");
    quote! {
        #doc
        pub struct Ctx<'a> {
            pub model: &'a theseus_modeling::Model,
            #(#fields)*
        }
    }
}

/// The trait a port's composition-root field is typed against. A method-bearing
/// port uses its own trait. A service-targeting port uses the target service's
/// trait, qualified by the target's crate path when it lives in another crate.
fn port_trait_path(port: &Port, model: &Model, current_crate: &str) -> TokenStream {
    let Some(service_name) = &port.target else {
        let own = format_ident!("{}", pascal_case(&port.name));
        return quote! { #own };
    };
    let trait_name = format_ident!("{}Service", pascal_case(service_name));
    match model.services.iter().find(|s| &s.name == service_name) {
        Some(service) if service.crate_name != current_crate && !service.crate_name.is_empty() => {
            let pkg = format_ident!("{}", service.crate_name.replace('-', "_"));
            quote! { #pkg::#trait_name }
        }
        _ => quote! { #trait_name },
    }
}

/// Render each distinct struct type the given services reference at their
/// boundaries — operation requests and the requests and responses of their port
/// methods — as a plain record. The parser that builds one from a transport's
/// input is the inbound adapter's wire-to-domain conversion, rendered with that
/// adapter, so the struct itself stays transport-neutral.
fn render_request_structs(services: &[&Service], model: &Model) -> TokenStream {
    let mut seen: Vec<&str> = Vec::new();
    let mut structs: Vec<TokenStream> = Vec::new();
    for label in referenced_labels(services) {
        if seen.contains(&label) {
            continue;
        }
        seen.push(label);
        if let Some(def) = model.type_def(label)
            && matches!(def.shape, TypeShape::Struct(_))
        {
            structs.push(render_request_struct(def, model));
        }
    }
    quote! { #(#structs)* }
}

/// Every type label a service references at its boundaries: each operation's
/// request, and each port method's request and response.
fn referenced_labels<'a>(services: &[&'a Service]) -> Vec<&'a str> {
    let mut labels = Vec::new();
    for service in services {
        for op in &service.operations {
            labels.push(op.request.as_str());
        }
        for port in &service.outbound {
            for method in &port.methods {
                labels.push(method.request.as_str());
                labels.push(method.response.as_str());
            }
        }
    }
    labels
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

/// Render a request struct as a plain record.
fn render_request_struct(def: &TypeDef, model: &Model) -> TokenStream {
    let TypeShape::Struct(fields) = &def.shape else {
        return quote! {};
    };
    let name = format_ident!("{}", def.name);
    let struct_doc = doc(&format!("The `{}` request.", def.name));

    let field_defs: Vec<TokenStream> = fields
        .iter()
        .map(|field| {
            let field_doc = doc(&field.doc);
            let field_name = format_ident!("{}", field.name);
            let field_type = syn_type(&resolve_field_type(&field.ty, model));
            quote! {
                #field_doc
                pub #field_name: #field_type,
            }
        })
        .collect();

    quote! {
        #struct_doc
        #[derive(Debug, Clone)]
        pub struct #name {
            #(#field_defs)*
        }
    }
}

/// The expression that reads one request field from the parsed arguments: a flag
/// for `bool`, the collected values for `Vec<T>`, an optional value for
/// `Option<T>`, otherwise a required value. A non-`String` value is read as its
/// parsed type.
fn field_parse(field: &Field, model: &Model) -> TokenStream {
    let flag = field.name.replace('_', "-");
    if field.ty == "bool" {
        quote! { matches.get_flag(#flag) }
    } else if let Some(inner) = vec_inner(&field.ty) {
        if inner == "String" {
            quote! { matches.get_many::<String>(#flag).map(|values| values.cloned().collect()).unwrap_or_default() }
        } else {
            // A `Vec` of a structured type: each repeatable value is a compact
            // string the element's `FromStr` decodes.
            let element = syn_type(&rust_type(inner, model));
            quote! {
                matches
                    .get_many::<String>(#flag)
                    .map(|values| values.map(|value| value.parse::<#element>()).collect::<Result<Vec<_>, _>>())
                    .transpose()?
                    .unwrap_or_default()
            }
        }
    } else if let Some(inner) = optional_inner(&field.ty) {
        if inner == "String" {
            quote! { arg(#flag) }
        } else {
            let inner = syn_type(&rust_type(inner, model));
            quote! { matches.get_one::<#inner>(#flag).cloned() }
        }
    } else if field.ty == "String" {
        quote! { arg(#flag).unwrap_or_default() }
    } else {
        let ty = syn_type(&field.ty);
        quote! { matches.get_one::<#ty>(#flag).cloned().unwrap_or_default() }
    }
}

/// Render the agent tool catalog: one tool-use definition per exposed operation,
/// its `input_schema` derived from the operation's request contract. The agent
/// loop and the MCP server both serve it, so they expose one tool surface.
fn render_tool_catalog(operations: &[&Operation], model: &Model) -> TokenStream {
    let tools: Vec<TokenStream> = operations
        .iter()
        .map(|op| {
            let name = op.name.as_str();
            let description = op.tool.as_deref().unwrap_or_default();
            let schema = render_tool_schema(op, model);
            quote! {
                serde_json::json!({
                    "name": #name,
                    "description": #description,
                    "input_schema": #schema
                })
            }
        })
        .collect();
    let doc_line = doc("Theseus's agent tool catalog, one tool-use definition per exposed");
    let doc_more = doc("operation. Served by the agent loop and the MCP server alike.");
    quote! {
        #doc_line
        #doc_more
        pub fn tool_catalog() -> Vec<serde_json::Value> {
            vec![#(#tools),*]
        }
    }
}

/// Render the agent tool dispatch: one arm per exposed operation, parsing the
/// request from the call's JSON input, running the trait method, and rendering
/// the result — text for a `String` response, otherwise JSON. The catalog and
/// this dispatch render from the same contract, so every catalog entry has an
/// arm here.
fn render_tool_dispatch(
    operations: &[&Operation],
    service: &Service,
    model: &Model,
) -> TokenStream {
    let trait_name = format_ident!("{}Service", pascal_case(&service.name));
    let parsers = render_tool_parsers(operations, model);
    let arms: Vec<TokenStream> = operations
        .iter()
        .map(|op| {
            let name = op.name.as_str();
            let method = format_ident!("{}", op.name);
            let call = match request_type(op, model) {
                Some(def) => {
                    let parser = format_ident!("parse_{}_input", proto_snake_case(&def.name));
                    quote! { service.#method(#parser(input)?).await? }
                }
                None => quote! { service.#method().await? },
            };
            let render = if rust_type(&op.response, model) == "String" {
                quote! { Ok(#call) }
            } else {
                quote! { Ok(serde_json::to_string(&#call)?) }
            };
            quote! { #name => #render, }
        })
        .collect();
    let known = operations
        .iter()
        .map(|op| op.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let unknown = format!("unknown tool `{{other}}`; tools are {known}");
    let input = if operations
        .iter()
        .any(|op| request_type(op, model).is_some())
    {
        format_ident!("input")
    } else {
        format_ident!("_input")
    };
    let doc_a = doc("Dispatch one tool call to the service: parse the request from the");
    let doc_b = doc("call's JSON input, run the operation, and render the result — text");
    let doc_c = doc("for a string, otherwise JSON. The catalog and this dispatch render");
    let doc_d = doc("from the same contract, so every catalog entry has an arm here.");
    quote! {
        #parsers
        #doc_a
        #doc_b
        #doc_c
        #doc_d
        pub async fn dispatch_tool(
            service: &impl #trait_name,
            name: &str,
            #input: &serde_json::Value,
        ) -> anyhow::Result<String> {
            match name {
                #(#arms)*
                other => anyhow::bail!(#unknown),
            }
        }
    }
}

/// Render a parser per distinct request struct the exposed operations take,
/// building the request from a tool call's JSON input. The wire-to-domain
/// conversion is rendered with the dispatch, so the struct itself stays
/// transport-neutral.
fn render_tool_parsers(operations: &[&Operation], model: &Model) -> TokenStream {
    let mut seen: Vec<&str> = Vec::new();
    let parsers: Vec<TokenStream> = operations
        .iter()
        .filter_map(|op| request_type(op, model))
        .filter(|def| {
            let fresh = !seen.contains(&def.name.as_str());
            if fresh {
                seen.push(&def.name);
            }
            fresh
        })
        .map(|def| {
            let TypeShape::Struct(fields) = &def.shape else {
                return quote! {};
            };
            let fn_name = format_ident!("parse_{}_input", proto_snake_case(&def.name));
            let ty = format_ident!("{}", def.name);
            let inits: Vec<TokenStream> = fields.iter().map(tool_field_init).collect();
            quote! {
                pub(crate) fn #fn_name(input: &serde_json::Value) -> anyhow::Result<#ty> {
                    Ok(#ty { #(#inits),* })
                }
            }
        })
        .collect();
    quote! { #(#parsers)* }
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

/// Render an inbound HTTP adapter: the operation handlers over the service trait,
/// with request parsers from a call's JSON body and the reply's status map. The
/// status derives from the outcome's structure — 200 a result, 400 a request that
/// does not parse, 404 an unknown operation, 501 an operation with no authored
/// handler, 403 a refused write, and 500 any other error.
fn render_http_module(inbound: &Inbound, service: &Service, model: &Model) -> TokenStream {
    let prefix = service_path_prefix(inbound, service, model);
    let trait_path = syn_type(&format!("{prefix}{}Service", pascal_case(&service.name)));
    let unimplemented_path = syn_type(&format!("{prefix}Unimplemented"));
    let refused_path = syn_type(&format!("{prefix}Refused"));

    // A parser per distinct request struct, building the request from the call's
    // JSON body — the same wire conversion the tool dispatch renders.
    let mut seen: Vec<&str> = Vec::new();
    let parsers: Vec<TokenStream> = service
        .operations
        .iter()
        .filter_map(|op| request_type(op, model))
        .filter(|def| {
            let fresh = !seen.contains(&def.name.as_str());
            if fresh {
                seen.push(&def.name);
            }
            fresh
        })
        .map(|def| {
            let TypeShape::Struct(fields) = &def.shape else {
                return quote! {};
            };
            let fn_name = format_ident!("parse_{}_http", proto_snake_case(&def.name));
            let ty = syn_type(&format!("{prefix}{}", def.name));
            let inits: Vec<TokenStream> = fields.iter().map(tool_field_init).collect();
            quote! {
                fn #fn_name(input: &serde_json::Value) -> anyhow::Result<#ty> {
                    Ok(#ty { #(#inits),* })
                }
            }
        })
        .collect();

    let arms: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let name = op.name.as_str();
            let method = format_ident!("{}", op.name);
            let render = if rust_type(&op.response, model) == "String" {
                format_ident!("reply_text")
            } else {
                format_ident!("reply_json")
            };
            match request_type(op, model) {
                Some(def) => {
                    let parser = format_ident!("parse_{}_http", proto_snake_case(&def.name));
                    quote! {
                        #name => match #parser(input) {
                            Ok(request) => #render(service.#method(request).await),
                            Err(error) => error_body(400, &error),
                        },
                    }
                }
                None => quote! { #name => #render(service.#method().await), },
            }
        })
        .collect();
    let input = if service
        .operations
        .iter()
        .any(|op| request_type(op, model).is_some())
    {
        format_ident!("input")
    } else {
        format_ident!("_input")
    };

    let doc_reply = doc("One HTTP reply: the status code and the rendered body.");
    let doc_a = doc("Handle one operation call: parse the request from the call's JSON body,");
    let doc_b = doc("run the operation, and render the reply. The status derives from the");
    let doc_c = doc("outcome's structure: 200 a result, 400 a request that does not parse,");
    let doc_d = doc("404 an unknown operation, 501 an operation with no authored handler,");
    let doc_e = doc("403 a refused write, and 500 any other error.");
    quote! {
        #doc_reply
        pub struct HttpReply {
            pub status: u16,
            pub content_type: &'static str,
            pub body: String,
        }

        #(#parsers)*

        #doc_a
        #doc_b
        #doc_c
        #doc_d
        #doc_e
        pub async fn handle(
            service: &impl #trait_path,
            name: &str,
            #input: &serde_json::Value,
        ) -> HttpReply {
            match name {
                #(#arms)*
                other => HttpReply {
                    status: 404,
                    content_type: "application/json",
                    body: serde_json::json!({
                        "error": format!("unknown operation `{other}`")
                    })
                    .to_string(),
                },
            }
        }

        fn reply_text(result: anyhow::Result<String>) -> HttpReply {
            match result {
                Ok(body) => HttpReply {
                    status: 200,
                    content_type: "text/plain; charset=utf-8",
                    body,
                },
                Err(error) => error_reply(&error),
            }
        }

        fn reply_json<T: serde::Serialize>(result: anyhow::Result<T>) -> HttpReply {
            match result {
                Ok(value) => match serde_json::to_string(&value) {
                    Ok(body) => HttpReply {
                        status: 200,
                        content_type: "application/json",
                        body,
                    },
                    Err(error) => error_reply(&anyhow::Error::new(error)),
                },
                Err(error) => error_reply(&error),
            }
        }

        #[doc = " The status an operation error maps to, read from the error's type: an"]
        #[doc = " operation on its trait default is 501, a write the gate refused is 403,"]
        #[doc = " and anything else is 500."]
        fn error_reply(error: &anyhow::Error) -> HttpReply {
            let status = if error.downcast_ref::<#unimplemented_path>().is_some() {
                501
            } else if error.downcast_ref::<#refused_path>().is_some() {
                403
            } else {
                500
            };
            error_body(status, error)
        }

        fn error_body(status: u16, error: &anyhow::Error) -> HttpReply {
            HttpReply {
                status,
                content_type: "application/json",
                body: serde_json::json!({ "error": error.to_string() }).to_string(),
            }
        }
    }
}

/// The proto package a service's contract lives in: the model and service names
/// lowercased, collapsed to one segment when they match.
fn proto_package(model: &Model, service: &Service) -> String {
    let model_name = model.name.to_lowercase();
    let service_name = service.name.to_lowercase();
    if model_name == service_name {
        model_name
    } else {
        format!("{model_name}.{service_name}")
    }
}

/// Render the proto contract a gRPC inbound serves: one message per request
/// struct the service's operations take, a message per contract type those
/// fields reference — a rich enum becomes a message holding a `oneof` over its
/// variants — a wrapper message per response label, and the service with one
/// rpc per operation. A response the model holds as a foreign type carries its
/// JSON rendering in a `json` field. The build compiles the file, so the wire
/// schema is a projection of the model like every other surface.
pub fn render_proto(model: &Model, service: &Service) -> String {
    let package = proto_package(model, service);
    let mut out = String::from("// @generated by `theseus generate` — do not edit by hand.\n");
    out.push_str("syntax = \"proto3\";\n\n");
    out.push_str(&format!("package {package};\n"));

    let mut rendered: Vec<String> = Vec::new();
    let mut referenced: Vec<String> = Vec::new();
    for op in &service.operations {
        let request = proto_request_message(op, model);
        if !rendered.contains(&request.0) {
            out.push_str(&format!("\n{}", request.1));
            rendered.push(request.0);
        }
        if let Some(def) = request_type(op, model)
            && let TypeShape::Struct(fields) = &def.shape
        {
            referenced.extend(referenced_message_labels(fields, model));
        }
        let response = proto_response_message(op, model);
        if !rendered.contains(&response.0) {
            out.push_str(&format!("\n{}", response.1));
            rendered.push(response.0);
        }
    }

    // Each contract type the request fields reference renders as its own
    // message, and a struct's fields may reference further ones.
    while let Some(label) = referenced.pop() {
        if rendered.contains(&label) {
            continue;
        }
        let Some(def) = model.type_def(&label) else {
            continue;
        };
        match &def.shape {
            TypeShape::Enum { variants, .. } => {
                out.push_str(&format!(
                    "\n{}",
                    proto_enum_message(&def.name, variants, model)
                ));
                rendered.push(label);
                for variant in variants {
                    referenced.extend(referenced_message_labels(&variant.fields, model));
                }
            }
            TypeShape::Struct(fields) => {
                let mut body = format!("message {} {{\n", def.name);
                push_proto_fields(&mut body, fields, model, "  ");
                body.push_str("}\n");
                out.push_str(&format!("\n{body}"));
                rendered.push(label);
                referenced.extend(referenced_message_labels(fields, model));
            }
            _ => {}
        }
    }

    out.push_str(&format!("\nservice {} {{\n", pascal_case(&service.name)));
    for op in &service.operations {
        let rpc = pascal_case(&op.name);
        let request = proto_request_message(op, model).0;
        let response = proto_response_message(op, model).0;
        out.push_str(&format!("  // {}\n", op.summary));
        out.push_str(&format!("  rpc {rpc} ({request}) returns ({response});\n"));
    }
    out.push_str("}\n");
    out
}

/// The contract types a set of fields references that render as messages of
/// their own: the base label of each field, stripped of its containers, when it
/// names a defined struct or enum.
fn referenced_message_labels(fields: &[Field], model: &Model) -> Vec<String> {
    fields
        .iter()
        .filter_map(|field| {
            let base = base_label(&field.ty);
            model
                .type_def(base)
                .filter(|def| matches!(def.shape, TypeShape::Struct(_) | TypeShape::Enum { .. }))
                .map(|def| def.name.clone())
        })
        .collect()
}

/// Render a rich enum as a proto message: one nested message per variant, and a
/// `oneof` over them named for the enum's tag, so an edit is one message
/// carrying exactly one verb.
fn proto_enum_message(name: &str, variants: &[Variant], model: &Model) -> String {
    let mut body = format!("message {name} {{\n");
    for variant in variants {
        body.push_str(&format!("  message {} {{\n", pascal_case(&variant.name)));
        push_proto_fields(&mut body, &variant.fields, model, "    ");
        body.push_str("  }\n");
    }
    body.push_str("\n  oneof verb {\n");
    for (index, variant) in variants.iter().enumerate() {
        body.push_str(&format!(
            "    {} {} = {};\n",
            pascal_case(&variant.name),
            variant.name,
            index + 1
        ));
    }
    body.push_str("  }\n}\n");
    body
}

/// Append a set of fields to a proto message body at the given indent.
fn push_proto_fields(body: &mut String, fields: &[Field], model: &Model, indent: &str) {
    for (index, field) in fields.iter().enumerate() {
        if !field.doc.is_empty() {
            body.push_str(&format!("{indent}// {}\n", field.doc));
        }
        body.push_str(&format!(
            "{indent}{} {} = {};\n",
            proto_type(&field.ty, model),
            field.name,
            index + 1
        ));
    }
}

/// The proto message an operation's request maps to: its struct rendered field
/// by field, or `Empty` for an operation that takes none. Returns the message
/// name and its definition.
fn proto_request_message(op: &Operation, model: &Model) -> (String, String) {
    match request_type(op, model) {
        Some(def) => {
            let TypeShape::Struct(fields) = &def.shape else {
                return empty_message();
            };
            let mut body = format!("message {} {{\n", def.name);
            push_proto_fields(&mut body, fields, model, "  ");
            body.push_str("}\n");
            (def.name.clone(), body)
        }
        None => empty_message(),
    }
}

/// The proto message an operation's response maps to: a wrapper carrying one
/// `value` for a label that resolves to a string, `Empty` for none, the struct
/// rendered field by field, or — for a label the model holds as a foreign type —
/// a wrapper carrying the response's JSON rendering.
fn proto_response_message(op: &Operation, model: &Model) -> (String, String) {
    if op.response == "Empty" {
        return empty_message();
    }
    match rust_type(&op.response, model).as_str() {
        "String" => (
            op.response.clone(),
            format!("message {} {{\n  string value = 1;\n}}\n", op.response),
        ),
        _ => match model.type_def(&op.response).map(|def| &def.shape) {
            Some(TypeShape::Struct(fields)) => {
                let mut body = format!("message {} {{\n", op.response);
                push_proto_fields(&mut body, fields, model, "  ");
                body.push_str("}\n");
                (op.response.clone(), body)
            }
            _ => (
                op.response.clone(),
                format!(
                    "// The JSON rendering of the `{}` response.\nmessage {} {{\n  string json = 1;\n}}\n",
                    op.response, op.response
                ),
            ),
        },
    }
}

fn empty_message() -> (String, String) {
    ("Empty".to_string(), "message Empty {}\n".to_string())
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

/// The proto type a contract type label maps to. A map field is a proto map —
/// never optional, absent as empty — matching a contract field that defaults.
fn proto_type(label: &str, model: &Model) -> String {
    if let Some(inner) = optional_inner(label) {
        let inner = proto_type(inner, model);
        if inner.starts_with("map<") {
            return inner;
        }
        return format!("optional {inner}");
    }
    if let Some(inner) = vec_inner(label) {
        return format!("repeated {}", proto_type(inner, model));
    }
    if let Some(value) = map_value(label) {
        return format!("map<string, {}>", proto_type(value, model));
    }
    match label {
        "String" => "string".to_string(),
        "bool" => "bool".to_string(),
        "f64" => "double".to_string(),
        "f32" => "float".to_string(),
        "i32" | "i8" | "i16" => "int32".to_string(),
        "i64" | "isize" => "int64".to_string(),
        "u32" | "u8" | "u16" => "uint32".to_string(),
        "u64" | "usize" => "uint64".to_string(),
        other => match model.type_def(other).map(|def| &def.shape) {
            Some(TypeShape::Struct(_) | TypeShape::Enum { .. }) => other.to_string(),
            _ => panic!("the gRPC renderer does not yet cover the field type `{other}`"),
        },
    }
}

/// Render an inbound gRPC adapter: the proto module the build compiles, the
/// service glue implementing the transport's generated server trait over the
/// service contract, and the reply's status map. The status derives from the
/// outcome's structure — OK a result, UNIMPLEMENTED an operation with no
/// authored handler, PERMISSION_DENIED a refused write, INTERNAL any other
/// error.
fn render_grpc_module(inbound: &Inbound, service: &Service, model: &Model) -> TokenStream {
    let prefix = service_path_prefix(inbound, service, model);
    let trait_path = syn_type(&format!("{prefix}{}Service", pascal_case(&service.name)));
    let unimplemented_path = syn_type(&format!("{prefix}Unimplemented"));
    let refused_path = syn_type(&format!("{prefix}Refused"));
    let package = proto_package(model, service);
    let server_mod = format_ident!("{}_server", proto_snake_case(&service.name));
    let server_trait = format_ident!("{}", pascal_case(&service.name));
    let glue = format_ident!("Grpc{}", pascal_case(&service.name));

    let methods: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let method = format_ident!("{}", op.name);
            let request_msg = format_ident!("{}", proto_request_message(op, model).0);
            let response_msg = format_ident!("{}", proto_response_message(op, model).0);
            let call = match request_type(op, model) {
                Some(def) => {
                    let ty = syn_type(&format!("{prefix}{}", def.name));
                    let TypeShape::Struct(fields) = &def.shape else {
                        panic!("a request type is a struct");
                    };
                    let inits: Vec<TokenStream> = fields
                        .iter()
                        .map(|field| grpc_field_conversion(field, model))
                        .collect();
                    quote! {
                        let request = request.into_inner();
                        let outcome = self.0.#method(#ty { #(#inits),* }).await;
                    }
                }
                None => quote! { let outcome = self.0.#method().await; },
            };
            let request_param = match request_type(op, model) {
                Some(_) => format_ident!("request"),
                None => format_ident!("_request"),
            };
            let respond = if op.response == "Empty" {
                quote! { Ok(_) => Ok(tonic::Response::new(proto::#response_msg {})), }
            } else if rust_type(&op.response, model) == "String" {
                quote! { Ok(value) => Ok(tonic::Response::new(proto::#response_msg { value })), }
            } else {
                // A foreign-typed response carries its JSON rendering.
                quote! {
                    Ok(value) => match serde_json::to_string(&value) {
                        Ok(json) => Ok(tonic::Response::new(proto::#response_msg { json })),
                        Err(error) => Err(tonic::Status::internal(error.to_string())),
                    },
                }
            };
            quote! {
                async fn #method(
                    &self,
                    #request_param: tonic::Request<proto::#request_msg>,
                ) -> Result<tonic::Response<proto::#response_msg>, tonic::Status> {
                    #call
                    match outcome {
                        #respond
                        Err(error) => Err(status(&error)),
                    }
                }
            }
        })
        .collect();

    let conversions = render_grpc_enum_conversions(service, model);
    let doc_proto = doc("The wire types and service glue the build compiles from the proto.");
    let doc_glue_a = doc("The service glue: the transport's generated server trait, implemented");
    let doc_glue_b = doc("over any implementation of the service contract.");
    quote! {
        #doc_proto
        pub mod proto {
            tonic::include_proto!(#package);
        }

        #doc_glue_a
        #doc_glue_b
        pub struct #glue<S>(pub S);

        #[tonic::async_trait]
        impl<S: #trait_path + Send + Sync + 'static> proto::#server_mod::#server_trait
        for #glue<S> {
            #(#methods)*
        }

        #conversions

        #[doc = " The status an operation error maps to, read from the error's type: an"]
        #[doc = " operation on its trait default is UNIMPLEMENTED, a write the gate refused"]
        #[doc = " is PERMISSION_DENIED, and anything else is INTERNAL."]
        fn status(error: &anyhow::Error) -> tonic::Status {
            if error.downcast_ref::<#unimplemented_path>().is_some() {
                tonic::Status::unimplemented(error.to_string())
            } else if error.downcast_ref::<#refused_path>().is_some() {
                tonic::Status::permission_denied(error.to_string())
            } else {
                tonic::Status::internal(error.to_string())
            }
        }
    }
}

/// The conversion one request field needs from its wire form to the contract's:
/// a map collects into the contract's ordered map, an enum-typed field converts
/// through its generated conversion, and a scalar passes through.
fn grpc_field_conversion(field: &Field, model: &Model) -> TokenStream {
    let name = format_ident!("{}", field.name);
    let base = base_label(&field.ty);
    let unwrapped = optional_inner(&field.ty).unwrap_or(&field.ty);
    if unwrapped.starts_with("BTreeMap<") {
        return quote! { #name: request.#name.into_iter().collect() };
    }
    match model.type_def(base).map(|def| &def.shape) {
        Some(TypeShape::Enum { .. }) => {
            let convert = format_ident!("{}_from_proto", proto_snake_case(base));
            if field.ty.starts_with("Vec<") {
                quote! {
                    #name: request
                        .#name
                        .into_iter()
                        .map(#convert)
                        .collect::<Result<Vec<_>, tonic::Status>>()?
                }
            } else {
                let missing = format!("the call needs a `{}`", field.name);
                quote! {
                    #name: #convert(
                        request
                            .#name
                            .ok_or_else(|| tonic::Status::invalid_argument(#missing))?,
                    )?
                }
            }
        }
        Some(TypeShape::Struct(_)) => panic!(
            "the gRPC renderer does not yet convert the struct-typed field `{}`",
            field.name
        ),
        _ => quote! { #name: request.#name },
    }
}

/// Render a conversion per rich enum the service's request fields carry: the
/// wire's `oneof` message becomes the contract's enum, variant by variant. A
/// message with no verb set is an invalid argument.
fn render_grpc_enum_conversions(service: &Service, model: &Model) -> TokenStream {
    let mut seen: Vec<&str> = Vec::new();
    let conversions: Vec<TokenStream> = service
        .operations
        .iter()
        .filter_map(|op| request_type(op, model))
        .filter_map(|def| match &def.shape {
            TypeShape::Struct(fields) => Some(fields),
            _ => None,
        })
        .flatten()
        .filter_map(|field| model.type_def(base_label(&field.ty)))
        .filter_map(|def| match &def.shape {
            TypeShape::Enum {
                variants,
                rust: Some(path),
            } if !seen.contains(&def.name.as_str()) => {
                seen.push(&def.name);
                Some((def, variants, path))
            }
            _ => None,
        })
        .map(|(def, variants, path)| {
            let enum_mod = format_ident!("{}", proto_snake_case(&def.name));
            let enum_msg = format_ident!("{}", def.name);
            let convert = format_ident!("{}_from_proto", proto_snake_case(&def.name));
            let rust_path = syn_type(path);
            let arms: Vec<TokenStream> = variants
                .iter()
                .map(|variant| {
                    let wire = format_ident!("{}", pascal_case(&variant.name));
                    let inits: Vec<TokenStream> = variant
                        .fields
                        .iter()
                        .map(|field| {
                            let name = format_ident!("{}", field.name);
                            let unwrapped = optional_inner(&field.ty).unwrap_or(&field.ty);
                            if unwrapped.starts_with("BTreeMap<") {
                                quote! { #name: data.#name.into_iter().collect() }
                            } else {
                                quote! { #name: data.#name }
                            }
                        })
                        .collect();
                    quote! {
                        Some(proto::#enum_mod::Verb::#wire(data)) => {
                            Ok(#rust_path::#wire { #(#inits),* })
                        }
                    }
                })
                .collect();
            let missing = format!("a {} carries one verb", def.name);
            let doc_line = doc(&format!(
                "Convert the wire's {} to the contract's, verb by verb.",
                def.name
            ));
            quote! {
                #doc_line
                fn #convert(value: proto::#enum_msg) -> Result<#rust_path, tonic::Status> {
                    match value.verb {
                        #(#arms)*
                        None => Err(tonic::Status::invalid_argument(#missing)),
                    }
                }
            }
        })
        .collect();
    quote! { #(#conversions)* }
}

/// Render an HTTP client adapter: the target service's contract implemented
/// over the wire — each call posts its request as a JSON body and maps the
/// reply's status back onto the contract's error classes, so the classes the
/// server mapped onto the wire survive the crossing back.
fn render_http_client_module(client: &Client, service: &Service, model: &Model) -> TokenStream {
    let prefix = host_path_prefix(&client.crate_name, service, model);
    let trait_path = syn_type(&format!("{prefix}{}Service", pascal_case(&service.name)));
    let unimplemented_path = syn_type(&format!("{prefix}Unimplemented"));
    let refused_path = syn_type(&format!("{prefix}Refused"));
    let name = format_ident!("Http{}Client", pascal_case(&service.name));

    let methods: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let method = format_ident!("{}", op.name);
            let op_name = op.name.as_str();
            let (param, body) = match request_type(op, model) {
                Some(def) => {
                    let ty = syn_type(&format!("{prefix}{}", def.name));
                    let TypeShape::Struct(fields) = &def.shape else {
                        panic!("a request type is a struct");
                    };
                    let entries: Vec<TokenStream> = fields
                        .iter()
                        .map(|field| {
                            let key = field.name.as_str();
                            let ident = format_ident!("{}", field.name);
                            quote! { #key: request.#ident }
                        })
                        .collect();
                    (
                        quote! { , request: #ty },
                        quote! { serde_json::json!({ #(#entries),* }) },
                    )
                }
                None => (quote! {}, quote! { serde_json::json!({}) }),
            };
            let response = response_type(&op.response, model);
            let finish = if op.response == "Empty" {
                quote! {
                    checked(#op_name, status, &body)?;
                    Ok(())
                }
            } else if rust_type(&op.response, model) == "String" {
                quote! {
                    checked(#op_name, status, &body)?;
                    Ok(body)
                }
            } else {
                quote! {
                    checked(#op_name, status, &body)?;
                    parsed(#op_name, &body)
                }
            };
            quote! {
                async fn #method(&self #param) -> anyhow::Result<#response> {
                    let (status, body) = self.post(#op_name, #body).await?;
                    #finish
                }
            }
        })
        .collect();

    let parsed = render_client_parsed(service, model);
    let doc_a = doc("The service contract carried over HTTP: each call posts its request as");
    let doc_b = doc("a JSON body, and the reply's status maps back onto the contract's error");
    let doc_c = doc("classes. A composition root wires it where an in-process adapter would");
    let doc_d = doc("stand.");
    quote! {
        #doc_a
        #doc_b
        #doc_c
        #doc_d
        pub struct #name {
            base_url: String,
            http: reqwest::Client,
        }

        impl #name {
            #[doc = " A client against `base_url`, e.g. `http://127.0.0.1:4870`."]
            pub fn new(base_url: impl Into<String>) -> Self {
                Self {
                    base_url: base_url.into(),
                    http: reqwest::Client::new(),
                }
            }

            #[doc = " Post one operation call and read the reply."]
            async fn post(
                &self,
                operation: &str,
                body: serde_json::Value,
            ) -> anyhow::Result<(u16, String)> {
                let response = self
                    .http
                    .post(format!("{}/{operation}", self.base_url))
                    .json(&body)
                    .send()
                    .await?;
                let status = response.status().as_u16();
                let body = response.text().await?;
                Ok((status, body))
            }
        }

        #[async_trait::async_trait]
        impl #trait_path for #name {
            #(#methods)*
        }

        #[doc = " Map a reply's status back onto the contract: 200 passes, 501 is the"]
        #[doc = " typed unimplemented default, 403 the typed refusal, anything else the"]
        #[doc = " reply's error body."]
        fn checked(operation: &'static str, status: u16, body: &str) -> anyhow::Result<()> {
            match status {
                200 => Ok(()),
                501 => Err(#unimplemented_path(operation).into()),
                403 => Err(#refused_path.into()),
                _ => anyhow::bail!("`{operation}` replied {status}: {body}"),
            }
        }

        #parsed
    }
}

/// Render a gRPC client adapter: the target service's contract implemented over
/// the wire's generated stub — each call converts its request to the proto
/// message, and a status maps back onto the contract's error classes.
fn render_grpc_client_module(client: &Client, service: &Service, model: &Model) -> TokenStream {
    let prefix = host_path_prefix(&client.crate_name, service, model);
    let trait_path = syn_type(&format!("{prefix}{}Service", pascal_case(&service.name)));
    let unimplemented_path = syn_type(&format!("{prefix}Unimplemented"));
    let refused_path = syn_type(&format!("{prefix}Refused"));
    let name = format_ident!("Grpc{}Client", pascal_case(&service.name));
    let package = proto_package(model, service);
    let client_mod = format_ident!("{}_client", proto_snake_case(&service.name));
    let stub_type = format_ident!("{}Client", pascal_case(&service.name));

    let methods: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let method = format_ident!("{}", op.name);
            let op_name = op.name.as_str();
            let request_msg = format_ident!("{}", proto_request_message(op, model).0);
            let (param, message) = match request_type(op, model) {
                Some(def) => {
                    let ty = syn_type(&format!("{prefix}{}", def.name));
                    let TypeShape::Struct(fields) = &def.shape else {
                        panic!("a request type is a struct");
                    };
                    let inits: Vec<TokenStream> = fields
                        .iter()
                        .map(|field| grpc_client_field_conversion(field, model))
                        .collect();
                    (
                        quote! { , request: #ty },
                        quote! { proto::#request_msg { #(#inits),* } },
                    )
                }
                None => (quote! {}, quote! { proto::#request_msg {} }),
            };
            let response = response_type(&op.response, model);
            let finish = if op.response == "Empty" {
                quote! {
                    reply.into_inner();
                    Ok(())
                }
            } else if rust_type(&op.response, model) == "String" {
                quote! { Ok(reply.into_inner().value) }
            } else {
                quote! { parsed(#op_name, &reply.into_inner().json) }
            };
            quote! {
                async fn #method(&self #param) -> anyhow::Result<#response> {
                    let reply = self
                        .stub
                        .clone()
                        .#method(#message)
                        .await
                        .map_err(|status| failed(#op_name, status))?;
                    #finish
                }
            }
        })
        .collect();

    let conversions = render_grpc_client_enum_conversions(service, model);
    let parsed = render_client_parsed(service, model);
    let doc_proto = doc("The wire types and client stub the build compiles from the proto.");
    let doc_a = doc("The service contract carried over gRPC: each call converts its request");
    let doc_b = doc("to the wire's message, and a status maps back onto the contract's error");
    let doc_c = doc("classes. A composition root wires it where an in-process adapter would");
    let doc_d = doc("stand.");
    quote! {
        #doc_proto
        pub mod proto {
            tonic::include_proto!(#package);
        }

        #doc_a
        #doc_b
        #doc_c
        #doc_d
        pub struct #name {
            stub: proto::#client_mod::#stub_type<tonic::transport::Channel>,
        }

        impl #name {
            #[doc = " Connect to `endpoint`, e.g. `http://127.0.0.1:4873`."]
            pub async fn connect(endpoint: String) -> anyhow::Result<Self> {
                Ok(Self {
                    stub: proto::#client_mod::#stub_type::connect(endpoint).await?,
                })
            }
        }

        #[async_trait::async_trait]
        impl #trait_path for #name {
            #(#methods)*
        }

        #conversions

        #[doc = " Map a status back onto the contract: UNIMPLEMENTED is the typed"]
        #[doc = " unimplemented default, PERMISSION_DENIED the typed refusal, anything"]
        #[doc = " else the status itself."]
        fn failed(operation: &'static str, status: tonic::Status) -> anyhow::Error {
            match status.code() {
                tonic::Code::Unimplemented => #unimplemented_path(operation).into(),
                tonic::Code::PermissionDenied => #refused_path.into(),
                _ => anyhow::anyhow!("`{operation}` failed: {status}"),
            }
        }

        #parsed
    }
}

/// Render the JSON reply parser a client uses for foreign-typed responses, when
/// the contract has any — a label past `Empty` and the string wrappers.
fn render_client_parsed(service: &Service, model: &Model) -> TokenStream {
    let has_json_response = service
        .operations
        .iter()
        .any(|op| op.response != "Empty" && rust_type(&op.response, model) != "String");
    if !has_json_response {
        return quote! {};
    }
    quote! {
        fn parsed<T: serde::de::DeserializeOwned>(
            operation: &'static str,
            body: &str,
        ) -> anyhow::Result<T> {
            serde_json::from_str(body)
                .map_err(|error| anyhow::anyhow!("`{operation}` reply did not parse: {error}"))
        }
    }
}

/// The conversion one request field needs from the contract's form to the
/// wire's: a map spreads into the proto map, an enum-typed field converts
/// through its generated conversion, and a scalar passes through.
fn grpc_client_field_conversion(field: &Field, model: &Model) -> TokenStream {
    let name = format_ident!("{}", field.name);
    let base = base_label(&field.ty);
    let unwrapped = optional_inner(&field.ty).unwrap_or(&field.ty);
    if unwrapped.starts_with("BTreeMap<") {
        return quote! { #name: request.#name.into_iter().collect() };
    }
    match model.type_def(base).map(|def| &def.shape) {
        Some(TypeShape::Enum { .. }) => {
            let convert = format_ident!("{}_to_proto", proto_snake_case(base));
            if field.ty.starts_with("Vec<") {
                quote! { #name: request.#name.into_iter().map(#convert).collect() }
            } else {
                quote! { #name: Some(#convert(request.#name)) }
            }
        }
        Some(TypeShape::Struct(_)) => panic!(
            "the gRPC client renderer does not yet convert the struct-typed field `{}`",
            field.name
        ),
        _ => quote! { #name: request.#name },
    }
}

/// Render a conversion per rich enum the service's request fields carry: the
/// contract's enum becomes the wire's `oneof` message, verb by verb — the
/// mirror of the server side's conversion.
fn render_grpc_client_enum_conversions(service: &Service, model: &Model) -> TokenStream {
    let mut seen: Vec<&str> = Vec::new();
    let conversions: Vec<TokenStream> = service
        .operations
        .iter()
        .filter_map(|op| request_type(op, model))
        .filter_map(|def| match &def.shape {
            TypeShape::Struct(fields) => Some(fields),
            _ => None,
        })
        .flatten()
        .filter_map(|field| model.type_def(base_label(&field.ty)))
        .filter_map(|def| match &def.shape {
            TypeShape::Enum {
                variants,
                rust: Some(path),
            } if !seen.contains(&def.name.as_str()) => {
                seen.push(&def.name);
                Some((def, variants, path))
            }
            _ => None,
        })
        .map(|(def, variants, path)| {
            let enum_mod = format_ident!("{}", proto_snake_case(&def.name));
            let enum_msg = format_ident!("{}", def.name);
            let convert = format_ident!("{}_to_proto", proto_snake_case(&def.name));
            let rust_path = syn_type(path);
            let arms: Vec<TokenStream> = variants
                .iter()
                .map(|variant| {
                    let wire = format_ident!("{}", pascal_case(&variant.name));
                    let names: Vec<proc_macro2::Ident> = variant
                        .fields
                        .iter()
                        .map(|field| format_ident!("{}", field.name))
                        .collect();
                    let inits: Vec<TokenStream> = variant
                        .fields
                        .iter()
                        .map(|field| {
                            let name = format_ident!("{}", field.name);
                            let unwrapped = optional_inner(&field.ty).unwrap_or(&field.ty);
                            if unwrapped.starts_with("BTreeMap<") {
                                quote! { #name: #name.into_iter().collect() }
                            } else {
                                quote! { #name }
                            }
                        })
                        .collect();
                    quote! {
                        #rust_path::#wire { #(#names),* } => proto::#enum_msg {
                            verb: Some(proto::#enum_mod::Verb::#wire(proto::#enum_mod::#wire {
                                #(#inits),*
                            })),
                        },
                    }
                })
                .collect();
            let doc_line = doc(&format!(
                "Convert the contract's {} to the wire's, verb by verb.",
                def.name
            ));
            quote! {
                #doc_line
                fn #convert(value: #rust_path) -> proto::#enum_msg {
                    match value {
                        #(#arms)*
                    }
                }
            }
        })
        .collect();
    quote! { #(#conversions)* }
}

/// Render an operation's JSON-schema `input_schema` from its request contract. An
/// `Empty` or fieldless request is an empty object. A field's type sets its schema
/// type, and a field required unless it is a `bool` or an `Option`.
fn render_tool_schema(op: &Operation, model: &Model) -> TokenStream {
    let fields = request_fields(op, model);
    let object = render_object_schema(fields, model);
    quote! { #object }
}

/// Render an object schema from a set of fields: a property per field, and a
/// `required` list of the fields that are neither `Option` nor `bool`.
fn render_object_schema(fields: &[Field], model: &Model) -> TokenStream {
    let properties: Vec<TokenStream> = fields
        .iter()
        .map(|field| render_schema_property(field, model))
        .collect();
    let required: Vec<&str> = fields
        .iter()
        .filter(|field| schema_required(&field.ty))
        .map(|field| field.name.as_str())
        .collect();
    let required_entry = if required.is_empty() {
        quote! {}
    } else {
        quote! { , "required": [#(#required),*] }
    };
    quote! { { "type": "object", "properties": { #(#properties),* } #required_entry } }
}

/// Render one field as a `"name": <schema>` property.
fn render_schema_property(field: &Field, model: &Model) -> TokenStream {
    let key = field.name.as_str();
    let schema = render_type_schema(&field.ty, model);
    quote! { #key: #schema }
}

/// The JSON schema for a contract type label. A `Vec<T>` is an array of its
/// element schema, a `BTreeMap<_, V>` an object of `V`-typed properties, an enum a
/// `oneOf` over its variants, and anything else its scalar type. `Option<T>` has
/// `T`'s schema — optionality is carried by the enclosing `required` list.
fn render_type_schema(label: &str, model: &Model) -> TokenStream {
    let label = optional_inner(label).unwrap_or(label);
    if let Some(inner) = vec_inner(label) {
        let items = render_type_schema(inner, model);
        return quote! { { "type": "array", "items": #items } };
    }
    if let Some(value) = map_value(label) {
        let value_schema = render_type_schema(value, model);
        return quote! { { "type": "object", "additionalProperties": #value_schema } };
    }
    if let Some(TypeShape::Enum { variants, .. }) = model.type_def(label).map(|def| &def.shape) {
        let branches = variants
            .iter()
            .map(|variant| render_variant_schema(variant, model));
        return quote! { { "oneOf": [#(#branches),*] } };
    }
    let ty = json_schema_type(label);
    quote! { { "type": #ty } }
}

/// One `oneOf` branch for an enum variant: the `verb` tag pinned to the variant's
/// name, then the variant's fields as properties.
fn render_variant_schema(variant: &Variant, model: &Model) -> TokenStream {
    let verb = variant.name.as_str();
    let properties: Vec<TokenStream> = variant
        .fields
        .iter()
        .map(|field| render_schema_property(field, model))
        .collect();
    let mut required: Vec<&str> = vec!["verb"];
    required.extend(
        variant
            .fields
            .iter()
            .filter(|field| schema_required(&field.ty))
            .map(|field| field.name.as_str()),
    );
    quote! {
        {
            "type": "object",
            "properties": { "verb": { "const": #verb } #(, #properties)* },
            "required": [#(#required),*]
        }
    }
}

/// The JSON-schema type for a contract type label.
fn json_schema_type(ty: &str) -> &'static str {
    match ty {
        "bool" => "boolean",
        "f64" | "f32" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
        | "isize" => "number",
        _ => "string",
    }
}

/// Whether a request field is a required schema property: not a `bool` (which
/// defaults false) and not an `Option` (which is absent when unset).
fn schema_required(ty: &str) -> bool {
    ty != "bool" && optional_inner(ty).is_none()
}

/// Render the inbound service trait: one method per operation, each defaulting to
/// an `unimplemented` error. The authored impl overrides the operations it
/// implements. An operation left on its default still compiles, and `verify`'s
/// coverage check reports it.
fn render_service_trait(service: &Service, model: &Model) -> TokenStream {
    let trait_name = format_ident!("{}Service", pascal_case(&service.name));
    let doc_a = doc("The inbound service contract: one method per operation, each defaulting");
    let doc_b = doc("to `unimplemented`. The authored impl overrides what it implements.");
    let methods: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let method_doc = doc(&op.summary);
            let method_name = format_ident!("{}", op.name);
            let param = match request_type(op, model) {
                Some(def) => {
                    let request = format_ident!("{}", def.name);
                    quote! { , _request: #request }
                }
                None => quote! {},
            };
            let response = response_type(&op.response, model);
            let name = op.name.as_str();
            quote! {
                #method_doc
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    Err(Unimplemented(#name).into())
                }
            }
        })
        .collect();
    quote! {
        #doc_a
        #doc_b
        #[async_trait::async_trait]
        pub trait #trait_name: Send + Sync {
            #(#methods)*
        }
    }
}

/// Render the typed errors a contract's boundary reports: `Unimplemented`, the
/// trait default's error, and `Refused`, a write gate's error. Both render with
/// the contract, so a transport adapter downcasts them to map an outcome in its
/// own vocabulary, and a wire client reconstructs them from the status coming
/// back.
fn render_unimplemented() -> TokenStream {
    let doc_a = doc("An operation with no authored handler, the trait default's error. A");
    let doc_b = doc("transport adapter downcasts it to map the outcome in its own vocabulary.");
    let doc_c = doc("A write refused by a permission gate. A transport adapter downcasts it");
    let doc_d = doc("to map the refusal in its own vocabulary.");
    quote! {
        #doc_a
        #doc_b
        #[derive(Debug)]
        pub struct Unimplemented(pub &'static str);

        impl std::fmt::Display for Unimplemented {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "unimplemented operation: {}", self.0)
            }
        }

        impl std::error::Error for Unimplemented {}

        #doc_c
        #doc_d
        #[derive(Debug)]
        pub struct Refused;

        impl std::fmt::Display for Refused {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    f,
                    "writes are not permitted; grant write permission to apply this edit"
                )
            }
        }

        impl std::error::Error for Refused {}
    }
}

/// The crate-path prefix request types and the service trait take inside an
/// inbound adapter: empty when the adapter shares the service's crate, otherwise
/// the service's crate module path followed by `::`, so a standalone adapter names
/// types it imports from elsewhere.
fn service_path_prefix(inbound: &Inbound, service: &Service, model: &Model) -> String {
    host_path_prefix(&inbound.crate_name, service, model)
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

/// The parser function name for a request struct, e.g. `parse_operands`.
fn parser_fn(def: &TypeDef) -> proc_macro2::Ident {
    format_ident!("parse_{}", proto_snake_case(&def.name))
}

/// Render an inbound CLI adapter: the command surface, the request parsers, the
/// parsed invocation, and the dispatch for the service it drives.
/// Request types are qualified by the service's crate path, so the adapter may
/// live in a crate other than the one that defines them.
fn render_inbound_module(inbound: &Inbound, service: &Service, model: &Model) -> TokenStream {
    let bin = &inbound.name;
    let prefix = service_path_prefix(inbound, service, model);
    let trait_path = syn_type(&format!("{prefix}{}Service", pascal_case(&service.name)));

    let subcommands: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| render_subcommand(op, model))
        .collect();
    let about = format!("The {} service.", service.name);
    let command = quote! {
        #[doc = " Build the command surface from the model."]
        pub fn command() -> Command {
            Command::new(#bin)
                .about(#about)
                .arg_required_else_help(true)
                .subcommand_required(true)
                #(#subcommands)*
        }
    };

    let parsers = render_inbound_parsers(service, model, &prefix);

    let variants: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let variant = format_ident!("{}", pascal_case(&op.name));
            match request_type(op, model) {
                Some(def) => {
                    let ty = syn_type(&format!("{prefix}{}", def.name));
                    quote! { #variant(#ty), }
                }
                None => quote! { #variant, },
            }
        })
        .collect();
    let arms: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let name = &op.name;
            let variant = format_ident!("{}", pascal_case(&op.name));
            match request_type(op, model) {
                Some(def) => {
                    let parser = parser_fn(def);
                    quote! { Some((#name, sub)) => Ok(Invocation::#variant(#parser(sub)?)), }
                }
                None => quote! { Some((#name, _)) => Ok(Invocation::#variant), },
            }
        })
        .collect();
    let invocation = quote! {
        pub enum Invocation {
            #(#variants)*
        }
        impl Invocation {
            #[doc = " Parse the invocation from the matched command line."]
            pub fn from_matches(matches: &ArgMatches) -> anyhow::Result<Self> {
                match matches.subcommand() {
                    #(#arms)*
                    _ => unreachable!("subcommand_required guarantees a subcommand"),
                }
            }
        }
    };

    let dispatch_arms: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let variant = format_ident!("{}", pascal_case(&op.name));
            let method = format_ident!("{}", op.name);
            let (pattern, call) = match request_type(op, model) {
                Some(_) => (
                    quote! { Invocation::#variant(request) },
                    quote! { service.#method(request).await? },
                ),
                None => (
                    quote! { Invocation::#variant },
                    quote! { service.#method().await? },
                ),
            };
            let render = if rust_type(&op.response, model) == "String" {
                quote! { println!("{}", #call) }
            } else {
                quote! { println!("{}", serde_json::to_string_pretty(&#call)?) }
            };
            quote! { #pattern => #render, }
        })
        .collect();
    let dispatch = quote! {
        #[doc = " Dispatch a parsed invocation to the service and render its result:"]
        #[doc = " text for a string, otherwise pretty JSON. The authored entry point"]
        #[doc = " overrides the operations that need bespoke output and delegates here."]
        pub async fn dispatch(service: &impl #trait_path, invocation: Invocation) -> anyhow::Result<()> {
            match invocation {
                #(#dispatch_arms)*
            }
            Ok(())
        }
    };

    quote! {
        #command
        #parsers
        #invocation
        #dispatch
    }
}

/// Render a free-function parser per distinct request struct the service's
/// operations take, building the request type at its crate-qualified path.
fn render_inbound_parsers(service: &Service, model: &Model, prefix: &str) -> TokenStream {
    let mut seen: Vec<&str> = Vec::new();
    let parsers: Vec<TokenStream> = service
        .operations
        .iter()
        .filter_map(|op| request_type(op, model))
        .filter(|def| {
            let fresh = !seen.contains(&def.name.as_str());
            if fresh {
                seen.push(&def.name);
            }
            fresh
        })
        .filter_map(|def| {
            let TypeShape::Struct(fields) = &def.shape else {
                return None;
            };
            let fn_name = parser_fn(def);
            let ty = syn_type(&format!("{prefix}{}", def.name));
            let arg_closure = if fields
                .iter()
                .any(|f| f.ty == "String" || f.ty == "Option<String>")
            {
                quote! { let arg = |name: &str| matches.get_one::<String>(name).cloned(); }
            } else {
                quote! {}
            };
            let inits: Vec<TokenStream> = fields
                .iter()
                .map(|field| {
                    let field_name = format_ident!("{}", field.name);
                    let parse = field_parse(field, model);
                    quote! { #field_name: #parse, }
                })
                .collect();
            Some(quote! {
                fn #fn_name(matches: &ArgMatches) -> anyhow::Result<#ty> {
                    #arg_closure
                    Ok(#ty {
                        #(#inits)*
                    })
                }
            })
        })
        .collect();
    quote! { #(#parsers)* }
}

/// The response type a method returns, as a parsed Rust type.
fn response_type(label: &str, model: &Model) -> TokenStream {
    let ty = syn_type(&rust_type(label, model));
    quote! { #ty }
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
/// `String` requests borrow as `&str`, the idiomatic borrowed form.
fn request_param(label: &str, model: &Model) -> TokenStream {
    if label == "Empty" {
        return quote! {};
    }
    let ty = rust_type(label, model);
    if ty == "String" {
        quote! { , request: &str }
    } else {
        let ty = syn_type(&ty);
        quote! { , request: &#ty }
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
    use crate::model::{Model, Port, Service, Transport};

    #[test]
    fn case_helpers() {
        assert_eq!(pascal_case("source-store"), "SourceStore");
        assert_eq!(snake_case("source-store"), "source_store");
    }

    #[test]
    fn a_service_targeting_port_reuses_the_target_service_trait() {
        let model = Model::new("App")
            .crate_node("app", "app", 1, &["calc"])
            .crate_node("calc", "calc", 0, &[])
            .service(
                Service::new("Calc")
                    .crate_name("calc")
                    .operation("add", "Add.", "Empty", "Empty"),
            )
            .service(
                Service::new("App")
                    .crate_name("app")
                    .port(Port::new("calculator", "Calls the calculator.").targeting("Calc")),
            );
        let rendered = render_module_for_crate(&model, "app");
        // The binding emits no port trait of its own. The composition-root field is
        // typed against the target service's trait at its crate path.
        assert!(!rendered.contains("trait Calculator"));
        assert!(rendered.contains("dyn calc::CalcService"));
    }

    #[test]
    fn a_service_without_an_inbound_renders_a_trait_but_no_command() {
        let model = Model::new("Calc")
            .service(Service::new("Calculator").operation("add", "Add.", "Empty", "Empty"));
        let rendered = render_cli_module(&model);
        // The service trait is present. With no inbound, nothing builds a command.
        assert!(rendered.contains("trait CalculatorService"));
        assert!(!rendered.contains("Command::new"));
    }

    #[test]
    fn a_request_struct_is_plain_and_its_parser_lives_with_the_inbound() {
        let model = Model::new("Calc")
            .struct_type("Operands", &[("a", "f64", "Left operand.")])
            .service(
                Service::new("Calculator")
                    .crate_name("calc")
                    .operation("add", "Add.", "Operands", "Empty"),
            );
        // Without an inbound the request is a plain struct and no parser is rendered.
        let plain = render_module_for_crate(&model, "calc");
        assert!(plain.contains("pub struct Operands"));
        assert!(plain.contains("pub a: f64"));
        assert!(!plain.contains("parse_operands"));
        assert!(!plain.contains("use clap"));
    }

    #[test]
    fn a_patched_in_tool_exposure_reaches_the_rendered_catalog() {
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("greet", "Greet.", "Empty", "Empty")
                    .tool("Say hello."),
            )
            .inbound("loop", Transport::Agent, "App", "app-agent");
        let edit = crate::patch::Edit::Add {
            parent: "service:app:App".to_string(),
            kind: "operation".to_string(),
            name: "ping".to_string(),
            attrs: [
                ("summary".to_string(), "Ping.".to_string()),
                ("tool".to_string(), "Ping the service.".to_string()),
            ]
            .into(),
        };
        let (outcome, patched) = crate::patch::apply_edit(&model, &edit);
        assert!(outcome.ok, "edit refused: {:?}", outcome.diagnostics);
        let rendered = render_module_for_crate(&patched.unwrap(), "app");
        // The patched-in operation joins the catalog beside the authored one.
        assert!(
            rendered.contains(r#""ping" =>"#),
            "the dispatch lacks a ping arm: {rendered}"
        );
        assert!(
            rendered.contains("Ping the service."),
            "catalog lacks the tool description: {rendered}"
        );
    }

    #[test]
    fn the_tool_dispatch_renders_an_arm_per_exposed_operation() {
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .struct_type("Payload", &[("body", "String", "The body.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("greet", "Greet.", "Empty", "String")
                    .tool("Say hello.")
                    .operation("send", "Send.", "Payload", "Empty")
                    .tool("Send a payload.")
                    .operation("hidden", "Hidden.", "Empty", "Empty"),
            )
            .inbound("loop", Transport::Agent, "App", "app-agent");
        let rendered = render_module_for_crate(&model, "app");
        assert!(
            rendered.contains("pub async fn dispatch_tool"),
            "{rendered}"
        );
        assert!(rendered.contains(r#""greet" =>"#));
        assert!(rendered.contains(r#""send" =>"#));
        assert!(
            !rendered.contains(r#""hidden" =>"#),
            "an unexposed operation has no dispatch arm"
        );
        assert!(
            rendered.contains("fn parse_payload_input"),
            "a struct request renders a parser"
        );
    }

    #[test]
    fn an_http_inbound_renders_handlers_with_a_status_map() {
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .crate_node("app-http", "app-http", 1, &["app"])
            .struct_type("Payload", &[("body", "String", "The body.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("greet", "Greet.", "Empty", "String")
                    .operation("send", "Send.", "Payload", "Empty"),
            )
            .inbound("http", Transport::Http, "App", "app-http");
        let rendered = render_module_for_crate(&model, "app-http");
        assert!(rendered.contains("pub async fn handle"), "{rendered}");
        assert!(rendered.contains(r#""greet" =>"#));
        assert!(rendered.contains("fn parse_payload_http"));
        assert!(
            rendered.contains("app::Unimplemented"),
            "the 501 arm downcasts the service crate's type: {rendered}"
        );
        assert!(rendered.contains("501") && rendered.contains("403") && rendered.contains("404"));
        assert!(
            !rendered.contains("use clap"),
            "an HTTP-only crate imports no command-line surface"
        );
    }

    #[test]
    fn a_grpc_inbound_renders_the_proto_and_the_service_glue() {
        let model = Model::new("App")
            .crate_node("calc", "calc", 0, &[])
            .crate_node("calc-grpc", "calc-grpc", 1, &["calc"])
            .struct_type(
                "Operands",
                &[
                    ("a", "f64", "Left operand."),
                    ("b", "f64", "Right operand."),
                ],
            )
            .foreign_type("CalcResult", "String")
            .service(Service::new("Calculator").crate_name("calc").operation(
                "add",
                "Add the operands.",
                "Operands",
                "CalcResult",
            ))
            .inbound("grpc", Transport::Grpc, "Calculator", "calc-grpc");
        let service = model.service_named("Calculator").expect("modeled");

        let proto = render_proto(&model, service);
        assert!(proto.contains("package app.calculator;"), "{proto}");
        assert!(proto.contains("message Operands {"));
        assert!(proto.contains("double a = 1;"));
        assert!(proto.contains("message CalcResult {\n  string value = 1;\n}"));
        assert!(proto.contains("rpc Add (Operands) returns (CalcResult);"));

        let rendered = render_module_for_crate(&model, "calc-grpc");
        assert!(rendered.contains("pub struct GrpcCalculator"), "{rendered}");
        assert!(rendered.contains("include_proto"));
        assert!(rendered.contains("calc::Unimplemented"));
        assert!(rendered.contains("unimplemented") && rendered.contains("permission_denied"));
    }

    #[test]
    fn the_grpc_renderer_covers_rich_enums_and_foreign_responses() {
        use crate::model::Variant;
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .crate_node("app-grpc", "app-grpc", 1, &["app"])
            .foreign_enum(
                "Edit",
                "theseus_modeling::Edit",
                &[
                    Variant::data(
                        "add",
                        &[
                            ("name", "String", "Name of the new node."),
                            (
                                "attrs",
                                "Option<BTreeMap<String, String>>",
                                "Scalar attributes.",
                            ),
                        ],
                    ),
                    Variant::data("remove", &[("target", "String", "Handle to remove.")]),
                ],
            )
            .struct_type(
                "PatchRequest",
                &[
                    ("edit", "Vec<Edit>", "The edits to apply in order."),
                    ("write", "bool", "Apply by reprojecting."),
                ],
            )
            .foreign_type("PatchResult", "theseus_modeling::PatchOutcome")
            .service(Service::new("App").crate_name("app").operation(
                "patch",
                "Propose an edit.",
                "PatchRequest",
                "PatchResult",
            ))
            .inbound("grpc", Transport::Grpc, "App", "app-grpc");
        let service = model.service_named("App").expect("modeled");

        let proto = render_proto(&model, service);
        assert!(proto.contains("package app;"), "{proto}");
        assert!(proto.contains("message Edit {"));
        assert!(proto.contains("oneof verb {"));
        assert!(proto.contains("Add add = 1;"));
        assert!(proto.contains("map<string, string> attrs"));
        assert!(
            !proto.contains("optional map"),
            "a map is never optional: {proto}"
        );
        assert!(proto.contains("message PatchResult {\n  string json = 1;\n}"));

        let rendered = render_module_for_crate(&model, "app-grpc");
        assert!(rendered.contains("fn edit_from_proto"), "{rendered}");
        assert!(rendered.contains("Verb::Add(data)"));
        assert!(rendered.contains("into_iter().collect()"));
        assert!(rendered.contains("serde_json::to_string"));
        assert!(rendered.contains("carries one verb"));
    }

    #[test]
    fn a_client_renders_the_contract_over_its_transport() {
        use crate::model::Variant;
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .crate_node("app-http-client", "app-http-client", 1, &["app"])
            .crate_node("app-grpc-client", "app-grpc-client", 1, &["app"])
            .foreign_enum(
                "Edit",
                "theseus_modeling::Edit",
                &[Variant::data("add", &[("name", "String", "The name.")])],
            )
            .struct_type(
                "PatchRequest",
                &[
                    ("edit", "Vec<Edit>", "The edits."),
                    ("write", "bool", "Apply."),
                ],
            )
            .foreign_type("PatchResult", "theseus_modeling::PatchOutcome")
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("patch", "Propose an edit.", "PatchRequest", "PatchResult")
                    .operation("model", "Describe.", "Empty", "String"),
            )
            .client("http-client", Transport::Http, "App", "app-http-client")
            .client("grpc-client", Transport::Grpc, "App", "app-grpc-client");

        let http = render_module_for_crate(&model, "app-http-client");
        assert!(http.contains("pub struct HttpAppClient"), "{http}");
        assert!(http.contains("for HttpAppClient"));
        assert!(http.contains("501") && http.contains("Refused"));
        assert!(http.contains("app::Unimplemented"));

        let grpc = render_module_for_crate(&model, "app-grpc-client");
        assert!(grpc.contains("pub struct GrpcAppClient"), "{grpc}");
        assert!(grpc.contains("fn edit_to_proto"));
        assert!(grpc.contains("Verb::Add("));
        assert!(grpc.contains("map(edit_to_proto)"));
        assert!(grpc.contains("PermissionDenied"));
    }

    #[test]
    fn a_port_method_carries_a_model_defined_struct() {
        let model = Model::new("App")
            .struct_type("Payload", &[("body", "String", "The body.")])
            .service(
                Service::new("App").crate_name("app").port(
                    Port::new("sink", "Receives a payload.")
                        .method("send", "Send it.", "Payload", "Empty"),
                ),
            );
        let rendered = render_module_for_crate(&model, "app");
        // The struct renders locally, and the port trait names it directly rather
        // than reaching for a twin in the engine crate.
        assert!(rendered.contains("pub struct Payload"));
        assert!(rendered.contains("async fn send(&self, request: &Payload)"));
        assert!(!rendered.contains("theseus_modeling::Payload"));
    }

    #[test]
    fn an_inbound_renders_a_typed_parser_for_the_field() {
        let model = Model::new("Calc")
            .crate_node("calc", "calc", 0, &[])
            .struct_type("Operands", &[("a", "f64", "Left operand.")])
            .service(
                Service::new("Calc")
                    .crate_name("calc")
                    .operation("add", "Add.", "Operands", "Empty"),
            )
            .inbound("calc", Transport::Cli, "Calc", "calc");
        let rendered = render_cli_module(&model);
        // The inbound's parser validates the argument as its type and reads it back.
        assert!(rendered.contains("value_parser"));
        assert!(rendered.contains("get_one::<f64>"));
        assert!(rendered.contains("fn parse_operands"));
    }
}
