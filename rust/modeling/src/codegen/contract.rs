//! The service crate's own half of the projection: the contract trait with
//! its boundary errors, the outbound port traits, the composition root, and
//! the request structs.

use super::*;

/// Render one outbound port as a trait. The hand-written adapter implements it.
pub(super) fn render_port_trait(port: &Port, model: &Model) -> TokenStream {
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
pub(super) fn render_composition_root(
    ports: &[&Port],
    model: &Model,
    current_crate: &str,
) -> TokenStream {
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
pub(super) fn port_trait_path(port: &Port, model: &Model, current_crate: &str) -> TokenStream {
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
pub(super) fn render_request_structs(services: &[&Service], model: &Model) -> TokenStream {
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
pub(super) fn referenced_labels<'a>(services: &[&'a Service]) -> Vec<&'a str> {
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

/// Render a request struct as a plain record.
pub(super) fn render_request_struct(def: &TypeDef, model: &Model) -> TokenStream {
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

/// Render the inbound service trait: one method per operation, each defaulting to
/// an `unimplemented` error. The authored impl overrides the operations it
/// implements. An operation left on its default still compiles, and `verify`'s
/// coverage check reports it.
pub(super) fn render_service_trait(service: &Service, model: &Model) -> TokenStream {
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
pub(super) fn render_unimplemented() -> TokenStream {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
