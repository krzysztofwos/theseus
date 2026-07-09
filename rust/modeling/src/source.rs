//! Source projection: render a model back to the Rust that defines it.
//!
//! An adopter's model of record is a `theseus_model()`-style function built with
//! the [`dsl`](crate::dsl) builders. This module renders a [`Model`] back to that
//! function's source, so a model edit reprojects the whole file rather than
//! splicing text. The render is a fixed point: the source compiles to a model
//! whose render is itself, which is exactly the drift gate's `verify`-projection
//! check. The body is formatted with `prettyplease`, so the output is canonical
//! and stable across renders.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::{
    codegen::{RenderError, validate_render_inputs},
    model::{
        Client, CrateNode, Field, Inbound, Method, Model, Operation, Port, Service, TypeDef,
        TypeShape, Variant,
    },
};

/// Render a model as the source of its authoring function.
///
/// `header` is the file's leading comment block, kept verbatim above the code.
/// `function` names the builder function the body defines.
pub fn render_model_source(
    model: &Model,
    header: &str,
    function: &str,
) -> Result<String, RenderError> {
    validate_render_inputs(model)?;
    let function_ident =
        syn::parse_str::<syn::Ident>(function).map_err(|error| RenderError::InvalidIdentifier {
            kind: "model function",
            name: function.to_string(),
            message: error.to_string(),
        })?;
    let imports = render_imports(model);
    let chain = render_model_chain(model);
    let tokens = quote! {
        #imports
        pub fn #function_ident() -> Model {
            #chain
        }
    };
    let file = syn::parse2(tokens).map_err(|error| RenderError::InvalidModelSource {
        function: function.to_string(),
        message: error.to_string(),
    })?;
    let body = crate::codegen::space_items(&prettyplease::unparse(&file));
    Ok(format!("{header}{body}"))
}

/// The `use` line for the builder vocabulary the chain references.
fn render_imports(model: &Model) -> TokenStream {
    let mut names: Vec<&str> = vec!["Model"];
    if !model.services.is_empty() {
        names.push("Service");
    }
    if !model.inbounds.is_empty() || !model.clients.is_empty() {
        names.push("Transport");
    }
    let has_ports = model.services.iter().any(|s| !s.outbound.is_empty())
        || model.inbounds.iter().any(|i| !i.outbound.is_empty());
    if has_ports {
        names.push("Port");
    }
    let has_foreign_enum = model
        .types
        .iter()
        .any(|def| matches!(&def.shape, TypeShape::Enum { rust: Some(_), .. }));
    if has_foreign_enum {
        names.push("Variant");
    }
    names.sort_unstable();
    names.dedup();
    let idents = names.iter().map(|name| format_ident!("{}", name));
    quote! { use theseus_modeling::{#(#idents),*}; }
}

/// Render the builder chain `Model::new(..).crate_node(..)..service(..)`.
fn render_model_chain(model: &Model) -> TokenStream {
    let name = &model.name;
    let crates = model.crates.iter().map(render_crate_node);
    let types = model.types.iter().map(render_type_def);
    let services = model.services.iter().map(render_service);
    let inbounds = model.inbounds.iter().map(render_inbound);
    let clients = model.clients.iter().map(render_client);
    quote! {
        Model::new(#name)
            #(#crates)*
            #(#types)*
            #(#services)*
            #(#inbounds)*
            #(#clients)*
    }
}

fn render_inbound(inbound: &Inbound) -> TokenStream {
    let name = &inbound.name;
    let transport = format_ident!("{}", format!("{:?}", inbound.transport));
    let service = &inbound.service;
    let crate_name = &inbound.crate_name;
    let turns = match inbound.turns {
        Some(turns) => {
            let turns = proc_macro2::Literal::u32_unsuffixed(turns);
            quote! { .turns(#turns) }
        }
        None => quote! {},
    };
    let ports = inbound.outbound.iter().map(|port| {
        let port = port_expression(port);
        quote! { .inbound_port(#port) }
    });
    quote! { .inbound(#name, Transport::#transport, #service, #crate_name) #turns #(#ports)* }
}

fn render_client(client: &Client) -> TokenStream {
    let name = &client.name;
    let transport = format_ident!("{}", format!("{:?}", client.transport));
    let service = &client.service;
    let crate_name = &client.crate_name;
    quote! { .client(#name, Transport::#transport, #service, #crate_name) }
}

fn render_crate_node(node: &CrateNode) -> TokenStream {
    let name = &node.name;
    let dir = &node.dir;
    let layer = proc_macro2::Literal::u32_unsuffixed(node.layer);
    let deps = node.depends_on.iter();
    quote! { .crate_node(#name, #dir, #layer, &[#(#deps),*]) }
}

fn render_type_def(def: &TypeDef) -> TokenStream {
    let name = &def.name;
    match &def.shape {
        TypeShape::Newtype(inner) => quote! { .newtype(#name, #inner) },
        TypeShape::Foreign(path) => quote! { .foreign_type(#name, #path) },
        TypeShape::Struct(fields) => {
            let entries = fields.iter().map(render_field);
            quote! { .struct_type(#name, &[#(#entries),*]) }
        }
        TypeShape::Enum {
            variants,
            rust: Some(path),
        } => {
            let variants = variants.iter().map(render_variant);
            quote! { .foreign_enum(#name, #path, &[#(#variants),*]) }
        }
        TypeShape::Enum {
            variants,
            rust: None,
        } => {
            let names = variants.iter().map(|variant| &variant.name);
            quote! { .enum_type(#name, &[#(#names),*]) }
        }
    }
}

/// Render one enum variant as its builder call: a unit variant as `Variant::unit`,
/// a data variant as `Variant::data` with its `(field, type, doc)` fields.
fn render_variant(variant: &Variant) -> TokenStream {
    let name = &variant.name;
    if variant.fields.is_empty() {
        quote! { Variant::unit(#name) }
    } else {
        let fields = variant.fields.iter().map(render_field);
        quote! { Variant::data(#name, &[#(#fields),*]) }
    }
}

fn render_field(field: &Field) -> TokenStream {
    let name = &field.name;
    let ty = &field.ty;
    let doc = &field.doc;
    quote! { (#name, #ty, #doc) }
}

fn render_service(service: &Service) -> TokenStream {
    let name = &service.name;
    let crate_name = if service.crate_name.is_empty() {
        quote! {}
    } else {
        let name = &service.crate_name;
        quote! { .crate_name(#name) }
    };
    let operations = service.operations.iter().map(render_operation);
    let ports = service.outbound.iter().map(render_port);
    quote! {
        .service(
            Service::new(#name)
                #crate_name
                #(#operations)*
                #(#ports)*
        )
    }
}

fn render_operation(op: &Operation) -> TokenStream {
    let name = &op.name;
    let summary = &op.summary;
    let request = &op.request;
    let response = &op.response;
    let uses = if op.uses.is_empty() {
        quote! {}
    } else {
        let ports = op.uses.iter();
        quote! { .uses(&[#(#ports),*]) }
    };
    let tool = match &op.tool {
        Some(description) => quote! { .tool(#description) },
        None => quote! {},
    };
    quote! { .operation(#name, #summary, #request, #response) #uses #tool }
}

fn render_port(port: &Port) -> TokenStream {
    let expression = port_expression(port);
    quote! { .port(#expression) }
}

/// The `Port::new(..)` builder expression a port renders as, wherever it hangs.
fn port_expression(port: &Port) -> TokenStream {
    let name = &port.name;
    let summary = &port.summary;
    let targeting = match &port.target {
        Some(service) => quote! { .targeting(#service) },
        None => quote! {},
    };
    let methods = port.methods.iter().map(render_method);
    quote! { Port::new(#name, #summary) #targeting #(#methods)* }
}

fn render_method(method: &Method) -> TokenStream {
    let name = &method.name;
    let summary = &method.summary;
    let request = &method.request;
    let response = &method.response;
    let gated = if method.gated {
        quote! { .gated() }
    } else {
        quote! {}
    };
    quote! { .method(#name, #summary, #request, #response) #gated }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    #[test]
    fn render_emits_the_builder_chain() {
        let source = render_model_source(&sample_model(), "// header\n", "sample_model")
            .expect("sample model renders");
        assert!(source.contains("pub fn sample_model() -> Model"));
        assert!(source.contains("Model::new(\"Sample\")"));
        for op in sample_model().operations() {
            assert!(
                source.contains(&format!(".operation({:?}", op.name)),
                "operation `{}` missing from the rendered source",
                op.name
            );
        }
    }

    #[test]
    fn render_emits_the_uses_edges() {
        use crate::model::Service;
        let model = crate::model::Model::new("Sample").service(
            Service::new("Sample")
                .operation("run", "Run.", "Empty", "String")
                .uses(&["workspace", "toolchain"]),
        );
        let source = render_model_source(&model, "", "sample_model").expect("sample model renders");
        assert!(
            source.contains(r#".uses(&["workspace", "toolchain"])"#),
            "{source}"
        );
    }

    #[test]
    fn render_emits_the_inbound_interior() {
        use crate::model::{Port, Service, Transport};
        let model = crate::model::Model::new("Sample")
            .struct_type("Turn", &[("prompt", "String", "The prompt.")])
            .foreign_type("Reply", "String")
            .service(Service::new("Sample"))
            .inbound("agent", Transport::Agent, "Sample", "sample-agent")
            .turns(32)
            .inbound_port(Port::new("llm", "Completes one turn.").method(
                "complete",
                "Complete one turn.",
                "Turn",
                "Reply",
            ));
        let source = render_model_source(&model, "", "sample_model").expect("sample model renders");
        assert!(source.contains(".turns(32)"), "{source}");
        assert!(source.contains(".inbound_port("), "{source}");
        assert!(source.contains("Port::new(\"llm\""), "{source}");
        assert!(
            source.contains("use theseus_modeling::{Model, Port, Service, Transport};"),
            "{source}"
        );
    }

    #[test]
    fn render_keeps_the_header_verbatim() {
        let source = render_model_source(&sample_model(), "// @generated\n", "sample_model")
            .expect("sample model renders");
        assert!(source.starts_with("// @generated\n"));
    }

    #[test]
    fn an_invalid_model_function_is_a_render_error() {
        assert!(matches!(
            render_model_source(&sample_model(), "", "not a function"),
            Err(RenderError::InvalidIdentifier {
                kind: "model function",
                ..
            })
        ));
    }

    #[test]
    fn invalid_model_tokens_are_refused_before_source_projection() {
        let model =
            Model::new("Sample").service(Service::new("Sample").crate_name("sample").operation(
                "match",
                "Reserved.",
                "Empty",
                "Empty",
            ));

        assert!(matches!(
            render_model_source(&model, "", "sample_model"),
            Err(RenderError::InvalidIdentifier {
                kind: "operation",
                ..
            })
        ));
    }
}
