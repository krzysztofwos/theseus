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
            // Each method defaults to the typed unimplemented error, the gate
            // the service trait holds: an adapter authors what it implements,
            // and a method it leaves on the default reports at the boundary.
            let name = format!("{}.{}", port.name, method.name);
            quote! {
                #method_doc
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    Err(Unimplemented(#name).into())
                }
            }
        })
        .collect();
    // A borrowed adapter serves the port its target serves. The forwarding
    // renders with the trait, so a port grown by a patch reaches through a
    // borrow — the session's gated composition — without an authored edit.
    let forwards: Vec<TokenStream> = port
        .methods
        .iter()
        .map(|method| {
            let method_name = format_ident!("{}", method.name);
            let param = bound_request_param(&method.request, model);
            let response = response_type(&method.response, model);
            let args = if method.request == "Empty" {
                quote! {}
            } else {
                quote! { request }
            };
            quote! {
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    (**self).#method_name(#args).await
                }
            }
        })
        .collect();
    let forward_doc = doc("A borrowed adapter serves the port its target serves, so a wrapper");
    let forward_doc_b = doc("generic over the port holds a borrow as readily as an owned adapter.");
    quote! {
        #trait_doc
        #[async_trait::async_trait]
        pub trait #trait_name: Send + Sync {
            #(#methods)*
        }

        #forward_doc
        #forward_doc_b
        #[async_trait::async_trait]
        impl<T: #trait_name + ?Sized> #trait_name for &T {
            #(#forwards)*
        }
    }
}

/// Render a port's write gate, for a port with a gated method: a wrapper
/// carrying the permission, refusing each gated method without it and passing
/// each ungated one through. The policy renders from the model, so which
/// methods a gate guards is a modeled fact, and a method the port grows lands
/// in the gate on the next render.
pub(super) fn render_port_gate(port: &Port, model: &Model) -> TokenStream {
    if port.methods.iter().all(|method| !method.gated) {
        return quote! {};
    }
    let trait_name = format_ident!("{}", pascal_case(&port.name));
    let gate_name = format_ident!("Gated{}", pascal_case(&port.name));
    let field = format_ident!("{}", snake_case(&port.name));
    let methods: Vec<TokenStream> = port
        .methods
        .iter()
        .map(|method| {
            let method_name = format_ident!("{}", method.name);
            let param = bound_request_param(&method.request, model);
            let response = response_type(&method.response, model);
            let args = if method.request == "Empty" {
                quote! {}
            } else {
                quote! { request }
            };
            let guard = if method.gated {
                quote! {
                    if !self.allow_writes {
                        return Err(Refused.into());
                    }
                }
            } else {
                quote! {}
            };
            quote! {
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    #guard
                    self.#field.#method_name(#args).await
                }
            }
        })
        .collect();
    let doc_a = doc(&format!(
        "The `{}` port carrying a write permission: a gated method is refused",
        port.name
    ));
    let doc_b = doc("without it, and an ungated one passes through. It wraps an owned");
    let doc_c = doc("adapter or a borrowed one, the same gate either way.");
    quote! {
        #doc_a
        #doc_b
        #doc_c
        pub struct #gate_name<A> {
            pub #field: A,
            pub allow_writes: bool,
        }

        #[async_trait::async_trait]
        impl<A: #trait_name> #trait_name for #gate_name<A> {
            #(#methods)*
        }
    }
}

/// Render the owned composition root: the service over one owned adapter per
/// port, driven through a fresh borrowed `Ctx` per call. A long-lived inbound
/// holds it where a borrowed root cannot live, and its delegations regenerate
/// with the contract, so a new operation reaches every owned composition on
/// the next render.
pub(super) fn render_standalone(
    services: &[&Service],
    ports: &[&Port],
    model: &Model,
    current_crate: &str,
) -> TokenStream {
    let params: Vec<proc_macro2::Ident> = ports
        .iter()
        .map(|port| format_ident!("{}Adapter", pascal_case(&port.name)))
        .collect();
    let bounds: Vec<TokenStream> = ports
        .iter()
        .zip(&params)
        .map(|(port, param)| {
            let bound = port_trait_path(port, model, current_crate);
            quote! { #param: #bound }
        })
        .collect();
    let fields: Vec<TokenStream> = ports
        .iter()
        .zip(&params)
        .map(|(port, param)| {
            let field = format_ident!("{}", snake_case(&port.name));
            quote! { pub #field: #param }
        })
        .collect();
    let ctx_fields: Vec<TokenStream> = ports
        .iter()
        .map(|port| {
            let field = format_ident!("{}", snake_case(&port.name));
            quote! { #field: &self.#field }
        })
        .collect();

    let impls: Vec<TokenStream> = services
        .iter()
        .map(|service| {
            let trait_name = format_ident!("{}Service", pascal_case(&service.name));
            let methods: Vec<TokenStream> = service
                .operations
                .iter()
                .map(|op| {
                    let method = format_ident!("{}", op.name);
                    let response = response_type(&op.response, model);
                    match request_type(op, model) {
                        Some(def) => {
                            let request = format_ident!("{}", def.name);
                            quote! {
                                async fn #method(&self, request: #request) -> anyhow::Result<#response> {
                                    self.ctx().#method(request).await
                                }
                            }
                        }
                        None => quote! {
                            async fn #method(&self) -> anyhow::Result<#response> {
                                self.ctx().#method().await
                            }
                        },
                    }
                })
                .collect();
            quote! {
                #[async_trait::async_trait]
                impl<#(#bounds),*> #trait_name for Standalone<#(#params),*> {
                    #(#methods)*
                }
            }
        })
        .collect();

    let doc_a = doc("An owned composition root: the service over one owned adapter per port,");
    let doc_b = doc("driven through a fresh borrowed `Ctx` per call. A long-lived inbound");
    let doc_c = doc("holds it where a borrowed root cannot live.");
    quote! {
        #doc_a
        #doc_b
        #doc_c
        pub struct Standalone<#(#bounds),*> {
            pub model: theseus_modeling::Model,
            #(#fields,)*
        }

        impl<#(#bounds),*> Standalone<#(#params),*> {
            #[doc = " The borrowed composition root one call runs over."]
            fn ctx(&self) -> Ctx<'_> {
                Ctx {
                    model: &self.model,
                    #(#ctx_fields),*
                }
            }
        }

        #(#impls)*
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
pub(super) fn render_request_structs(
    services: &[&Service],
    extra_ports: &[&Port],
    model: &Model,
) -> TokenStream {
    let mut seen: Vec<&str> = Vec::new();
    let mut structs: Vec<TokenStream> = Vec::new();
    for label in referenced_labels(services, extra_ports) {
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

/// Every type label a service references at its boundaries — each operation's
/// request, and each port method's request and response — plus the boundaries of
/// `extra_ports`, the ports hung on an inbound hosted in the crate.
pub(super) fn referenced_labels<'a>(
    services: &[&'a Service],
    extra_ports: &[&'a Port],
) -> Vec<&'a str> {
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
    for port in extra_ports {
        for method in &port.methods {
            labels.push(method.request.as_str());
            labels.push(method.response.as_str());
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

/// Render `Unimplemented`, the trait default's error. It renders with every
/// contract that defaults a method, so a transport adapter downcasts it to map
/// the outcome in its own vocabulary, and a wire client reconstructs it from
/// the status coming back.
pub(super) fn render_unimplemented() -> TokenStream {
    let doc_a = doc("An operation with no authored handler, the trait default's error. A");
    let doc_b = doc("transport adapter downcasts it to map the outcome in its own vocabulary.");
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
    }
}

/// Render `Refused`, a write gate's error. It renders with a service contract —
/// the boundary a gate guards — so an adapter downcasts it and a wire client
/// reconstructs it from the status coming back.
pub(super) fn render_refused() -> TokenStream {
    let doc_a = doc("A write refused by a permission gate. A transport adapter downcasts it");
    let doc_b = doc("to map the refusal in its own vocabulary.");
    quote! {
        #doc_a
        #doc_b
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
    fn a_crate_with_ports_renders_an_owned_root_delegating_every_operation() {
        let model = Model::new("App")
            .struct_type("Payload", &[("body", "String", "The body.")])
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("run", "Run.", "Payload", "String")
                    .operation("status", "Status.", "Empty", "String")
                    .port(
                        Port::new("sink", "Receives a payload.")
                            .method("send", "Send it.", "Payload", "Empty"),
                    ),
            );
        let rendered = render_module_for_crate(&model, "app");
        // One generic parameter per port, bounded by the port trait, and one
        // delegation per operation through a fresh borrowed root.
        assert!(rendered.contains("pub struct Standalone<SinkAdapter: Sink>"));
        assert!(rendered.contains("fn ctx(&self) -> Ctx<'_>"));
        assert!(
            rendered.contains("impl<SinkAdapter: Sink> AppService for Standalone<SinkAdapter>")
        );
        assert!(rendered.contains("self.ctx().run(request).await"));
        assert!(rendered.contains("self.ctx().status().await"));
    }

    #[test]
    fn an_inbound_interior_renders_into_its_own_crate() {
        let model = Model::new("App")
            .crate_node("app", "app", 1, &[])
            .crate_node("loop", "loop", 2, &["app"])
            .struct_type("Turn", &[("system", "String", "The framing.")])
            .foreign_type("Reply", "crate::agent::Reply")
            .service(
                Service::new("App")
                    .crate_name("app")
                    .operation("run", "Run.", "Empty", "String"),
            )
            .inbound("agent", crate::model::Transport::Agent, "App", "loop")
            .turns(32)
            .inbound_port(Port::new("llm", "Completes one turn.").method(
                "complete",
                "Complete one turn.",
                "Turn",
                "Reply",
            ));
        let rendered = render_module_for_crate(&model, "loop");
        // The loop's crate carries the port trait, its request struct, the typed
        // default, and the budget. The service's crate carries none of it.
        assert!(rendered.contains("pub trait Llm"));
        assert!(rendered.contains("Err(Unimplemented(\"llm.complete\").into())"));
        assert!(rendered.contains("pub struct Turn"));
        assert!(rendered.contains("crate::agent::Reply"));
        assert!(rendered.contains("pub const TURN_BUDGET: usize = 32;"));
        let service_side = render_module_for_crate(&model, "app");
        assert!(!service_side.contains("trait Llm"));
        assert!(!service_side.contains("TURN_BUDGET"));
    }

    #[test]
    fn a_gated_method_renders_its_ports_write_gate() {
        let model = Model::new("App").service(
            Service::new("App").crate_name("app").port(
                Port::new("store", "Writes records.")
                    .method("read", "Read.", "String", "String")
                    .method("write", "Write.", "String", "String")
                    .gated(),
            ),
        );
        let rendered = render_module_for_crate(&model, "app");
        assert!(rendered.contains("pub struct GatedStore<A>"));
        // The gated method guards; the ungated one passes through.
        assert!(rendered.contains("async fn write(&self, request: &str)"));
        assert!(rendered.contains("Err(Refused.into())"));
        assert!(rendered.contains("self.store.read(request).await"));

        // A port with no gated method renders no gate.
        let ungated = Model::new("App").service(Service::new("App").crate_name("app").port(
            Port::new("store", "Reads records.").method("read", "Read.", "String", "String"),
        ));
        assert!(!render_module_for_crate(&ungated, "app").contains("GatedStore"));
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
        // The struct renders locally, and the port trait names it by its local
        // name.
        assert!(rendered.contains("pub struct Payload"));
        assert!(rendered.contains("async fn send(&self, _request: &Payload)"));
        // The borrowed forwarder renders with the trait, one forward per method.
        assert!(rendered.contains("impl<T: Sink + ?Sized> Sink for &T"));
        assert!(rendered.contains("(**self).send(request).await"));
        assert!(!rendered.contains("theseus_modeling::Payload"));
    }
}
