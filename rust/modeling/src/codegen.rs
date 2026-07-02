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
use serde::Serialize;

use crate::model::{
    Field, Inbound, Model, Operation, Port, Service, Transport, TypeDef, TypeShape, Variant,
};

/// A file rendered from the model, addressed relative to the workspace root. An
/// adopter sets the path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
    // A service driven by an agent or MCP inbound carries a tool catalog and the
    // dispatch behind it, both rendered from each exposed operation's contract,
    // so every catalog entry has a dispatch arm.
    let agent_service = services.iter().find(|service| {
        model.inbounds.iter().any(|inbound| {
            inbound.service == service.name
                && matches!(inbound.transport, Transport::Agent | Transport::Mcp)
        })
    });
    let tool_operations: Vec<&Operation> = services
        .iter()
        .flat_map(|service| service.operations.iter())
        .filter(|op| op.tool.is_some())
        .collect();
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
    // A CLI inbound adapter hosted in this crate renders the command surface,
    // request parsers, parsed invocation, and dispatch for the service it drives,
    // even when that service lives in another crate. A non-CLI inbound (an agent
    // loop, an MCP server) runs in its own authored binary and renders no surface.
    let inbound_modules: Vec<TokenStream> = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.crate_name == crate_name && inbound.transport == Transport::Cli)
        .filter_map(|inbound| {
            model
                .service_named(&inbound.service)
                .map(|service| render_inbound_module(inbound, service, model))
        })
        .collect();

    // The command surface and its parsers carry the only command-line dependency.
    // A crate without an inbound adapter imports none of it.
    let command_import = if inbound_modules.is_empty() {
        quote! {}
    } else {
        quote! { use clap::{Arg, ArgAction, ArgMatches, Command}; }
    };

    let tokens = quote! {
        #command_import

        #(#port_traits)*
        #composition_root
        #requests
        #(#service_traits)*
        #tool_catalog
        #(#inbound_modules)*
    };

    let file = syn::parse2(tokens).expect("generated code is valid Rust syntax");
    let body = space_items(&prettyplease::unparse(&file));

    let mut out = String::from("// @generated by `theseus generate` — do not edit by hand.\n");
    out.push_str(&format!(
        "//! Theseus's generated scaffolding: {}.\n\n",
        module_doc_summary(&services, &ports, &inbound_modules, has_tool_catalog)
    ));
    out.push_str(&body);
    out
}

/// Summarize what a crate's generated file holds, naming only the parts present.
/// A service-hosting crate carries the request types and the service contract,
/// and with outbound dependencies the port traits and the composition root. A
/// crate hosting a CLI inbound carries the command surface, the request parsers,
/// the parsed invocation, and dispatch.
fn module_doc_summary(
    services: &[&Service],
    ports: &[&Port],
    inbound_modules: &[TokenStream],
    has_tool_catalog: bool,
) -> String {
    let mut concerns: Vec<&str> = Vec::new();
    if !services.is_empty() {
        concerns.push("the request types and service contract");
    }
    if !ports.is_empty() {
        concerns.push("the outbound port traits and composition root");
    }
    if has_tool_catalog {
        concerns.push("the agent tool catalog and dispatch");
    }
    if !inbound_modules.is_empty() {
        concerns.push("the command surface, request parsers, invocation, and dispatch");
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
    const KEYWORDS: [&str; 20] = [
        "use ",
        "pub use ",
        "fn ",
        "pub fn ",
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

/// The element type of an `Option<…>` label, when the label is one.
fn optional_inner(ty: &str) -> Option<&str> {
    ty.strip_prefix("Option<")?.strip_suffix('>')
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
                fn #method_name(&self #param) -> anyhow::Result<#response>;
            }
        })
        .collect();
    quote! {
        #trait_doc
        pub trait #trait_name {
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
    } else if let Some(inner) = field
        .ty
        .strip_prefix("Vec<")
        .and_then(|ty| ty.strip_suffix('>'))
    {
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
            let inner = syn_type(inner);
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
                    let parser = format_ident!("parse_{}_input", def.name.to_lowercase());
                    quote! { service.#method(#parser(input)?)? }
                }
                None => quote! { service.#method()? },
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
        pub fn dispatch_tool(
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
            let fn_name = format_ident!("parse_{}_input", def.name.to_lowercase());
            let ty = format_ident!("{}", def.name);
            let inits: Vec<TokenStream> = fields.iter().map(tool_field_init).collect();
            quote! {
                fn #fn_name(input: &serde_json::Value) -> anyhow::Result<#ty> {
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
    if let Some(inner) = label
        .strip_prefix("Vec<")
        .and_then(|ty| ty.strip_suffix('>'))
    {
        let items = render_type_schema(inner, model);
        return quote! { { "type": "array", "items": #items } };
    }
    if let Some(value) = map_value_type(label) {
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

/// The value type of a `BTreeMap<K, V>` label, when the label is one. Keys are
/// strings, so only the value type shapes the schema.
fn map_value_type(label: &str) -> Option<&str> {
    let inner = label.strip_prefix("BTreeMap<")?.strip_suffix('>')?;
    inner.split_once(',').map(|(_, value)| value.trim())
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
            let unimplemented = format!("unimplemented operation: {}", op.name);
            quote! {
                #method_doc
                fn #method_name(&self #param) -> anyhow::Result<#response> {
                    anyhow::bail!(#unimplemented)
                }
            }
        })
        .collect();
    quote! {
        #doc_a
        #doc_b
        pub trait #trait_name {
            #(#methods)*
        }
    }
}

/// The crate-path prefix request types and the service trait take inside an
/// inbound adapter: empty when the adapter shares the service's crate, otherwise
/// the service's crate module path followed by `::`, so a standalone adapter names
/// types it imports from elsewhere.
fn service_path_prefix(inbound: &Inbound, service: &Service, model: &Model) -> String {
    if inbound.crate_name == service.crate_name {
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
    format_ident!("parse_{}", def.name.to_lowercase())
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
                    _ => unreachable!("arg_required_else_help guarantees a subcommand"),
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
                    quote! { service.#method(request)? },
                ),
                None => (
                    quote! { Invocation::#variant },
                    quote! { service.#method()? },
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
        pub fn dispatch(service: &impl #trait_path, invocation: Invocation) -> anyhow::Result<()> {
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
    format!("fn {}(&self{param}) -> anyhow::Result<{response}>", op.name)
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
    if let Some(inner) = label
        .strip_prefix("Vec<")
        .and_then(|ty| ty.strip_suffix('>'))
    {
        return format!("Vec<{}>", resolve_field_type(inner, model));
    }
    if let Some(inner) = optional_inner(label) {
        return format!("Option<{}>", resolve_field_type(inner, model));
    }
    rust_type(label, model)
}

/// Parse a rendered type string into a token type, e.g. `()` or `Option<String>`.
fn syn_type(text: &str) -> syn::Type {
    syn::parse_str(text).expect("rendered type is valid Rust")
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
        assert!(rendered.contains("ping"), "catalog lacks ping: {rendered}");
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
        assert!(rendered.contains("pub fn dispatch_tool"), "{rendered}");
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
        assert!(rendered.contains("fn send(&self, request: &Payload)"));
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
