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

use crate::model::{
    CrateNode, Field, Inbound, Method, Model, Operation, Port, Service, TypeDef, TypeShape,
};

/// Render a model as the source of its authoring function.
///
/// `header` is the file's leading comment block, kept verbatim above the code.
/// `function` names the builder function the body defines.
pub fn render_model_source(model: &Model, header: &str, function: &str) -> String {
    let function = format_ident!("{}", function);
    let imports = render_imports(model);
    let chain = render_model_chain(model);
    let tokens = quote! {
        #imports
        pub fn #function() -> Model {
            #chain
        }
    };
    let file = syn::parse2(tokens).expect("rendered model source is valid Rust");
    let body = prettyplease::unparse(&file);
    format!("{header}{body}")
}

/// The `use` line for the builder vocabulary the chain references.
fn render_imports(model: &Model) -> TokenStream {
    let mut names: Vec<&str> = vec!["Model"];
    if !model.services.is_empty() {
        names.push("Service");
        names.push("Transport");
    }
    if !model.inbounds.is_empty() {
        names.push("Transport");
    }
    if model.services.iter().any(|s| !s.outbound.is_empty()) {
        names.push("Port");
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
    quote! {
        Model::new(#name)
            #(#crates)*
            #(#types)*
            #(#services)*
            #(#inbounds)*
    }
}

fn render_inbound(inbound: &Inbound) -> TokenStream {
    let name = &inbound.name;
    let transport = format_ident!("{}", format!("{:?}", inbound.transport));
    let service = &inbound.service;
    let crate_name = &inbound.crate_name;
    quote! { .inbound(#name, Transport::#transport, #service, #crate_name) }
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
        TypeShape::Enum(variants) => {
            let variants = variants.iter();
            quote! { .enum_type(#name, &[#(#variants),*]) }
        }
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
    quote! { .operation(#name, #summary, #request, #response) }
}

fn render_port(port: &Port) -> TokenStream {
    let name = &port.name;
    let summary = &port.summary;
    let targeting = match &port.target {
        Some(service) => quote! { .targeting(#service) },
        None => quote! {},
    };
    let methods = port.methods.iter().map(render_method);
    quote! { .port(Port::new(#name, #summary) #targeting #(#methods)*) }
}

fn render_method(method: &Method) -> TokenStream {
    let name = &method.name;
    let summary = &method.summary;
    let request = &method.request;
    let response = &method.response;
    quote! { .method(#name, #summary, #request, #response) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    #[test]
    fn render_emits_the_builder_chain() {
        let source = render_model_source(&sample_model(), "// header\n", "sample_model");
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
    fn render_keeps_the_header_verbatim() {
        let source = render_model_source(&sample_model(), "// @generated\n", "sample_model");
        assert!(source.starts_with("// @generated\n"));
    }
}
