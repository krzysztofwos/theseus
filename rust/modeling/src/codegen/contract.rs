//! The service crate's own half of the projection: the contract trait with
//! its boundary errors, the outbound port traits, the composition root, and
//! the request structs.

use super::*;

/// Render one outbound port as a trait. The hand-written adapter implements it.
pub(super) fn render_port_trait(port: &Port, model: &Model) -> Result<TokenStream, RenderError> {
    let trait_name = format_ident!("{}", pascal_case(&port.name));
    let trait_doc = doc(&port.summary);
    let methods: Vec<TokenStream> = port
        .methods
        .iter()
        .map(|method| -> Result<TokenStream, RenderError> {
            let method_doc = doc(&method.summary);
            let method_name = format_ident!("{}", method.name);
            let param = request_param(&method.request, model)?;
            let response = response_type(&method.response, model)?;
            // Each method defaults to the typed unimplemented error, the gate
            // the service trait holds: an adapter authors what it implements,
            // and a method it leaves on the default reports at the boundary.
            let name = format!("{}.{}", port.name, method.name);
            Ok(quote! {
                #method_doc
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    Err(Unimplemented(#name).into())
                }
            })
        })
        .collect::<Result<_, _>>()?;
    // A borrowed adapter serves the port its target serves. The forwarding
    // renders with the trait, so a port grown by a patch reaches through a
    // borrow — the session's gated composition — without an authored edit.
    let forwards: Vec<TokenStream> = port
        .methods
        .iter()
        .map(|method| -> Result<TokenStream, RenderError> {
            let method_name = format_ident!("{}", method.name);
            let param = bound_request_param(&method.request, model)?;
            let response = response_type(&method.response, model)?;
            let args = if method.request == "Empty" {
                quote! {}
            } else {
                quote! { request }
            };
            Ok(quote! {
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    (**self).#method_name(#args).await
                }
            })
        })
        .collect::<Result<_, _>>()?;
    let forward_doc = doc("A borrowed adapter serves the port its target serves, so a wrapper");
    let forward_doc_b = doc("generic over the port holds a borrow as readily as an owned adapter.");
    Ok(quote! {
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
    })
}

/// Render a port's write gate, for a port with a gated method: a wrapper
/// carrying the permission, refusing each gated method without it and passing
/// each ungated one through. The policy renders from the model, so which
/// methods a gate guards is a modeled fact, and a method the port grows lands
/// in the gate on the next render.
pub(super) fn render_port_gate(port: &Port, model: &Model) -> Result<TokenStream, RenderError> {
    if port.methods.iter().all(|method| !method.gated) {
        return Ok(quote! {});
    }
    let trait_name = format_ident!("{}", pascal_case(&port.name));
    let gate_name = format_ident!("Gated{}", pascal_case(&port.name));
    let field = format_ident!("{}", snake_case(&port.name));
    let methods: Vec<TokenStream> = port
        .methods
        .iter()
        .map(|method| -> Result<TokenStream, RenderError> {
            let method_name = format_ident!("{}", method.name);
            let param = bound_request_param(&method.request, model)?;
            let response = response_type(&method.response, model)?;
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
            Ok(quote! {
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    #guard
                    self.#field.#method_name(#args).await
                }
            })
        })
        .collect::<Result<_, _>>()?;
    let doc_a = doc(&format!(
        "The `{}` port carrying a write permission: a gated method is refused",
        port.name
    ));
    let doc_b = doc("without it, and an ungated one passes through. It wraps an owned");
    let doc_c = doc("adapter or a borrowed one, the same gate either way.");
    Ok(quote! {
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
    })
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
) -> Result<TokenStream, RenderError> {
    let params: Vec<proc_macro2::Ident> = ports
        .iter()
        .map(|port| format_ident!("{}Adapter", pascal_case(&port.name)))
        .collect();
    let bounds: Vec<TokenStream> = ports
        .iter()
        .zip(&params)
        .map(|(port, param)| -> Result<TokenStream, RenderError> {
            let bound = port_trait_path(port, model, current_crate)?;
            Ok(quote! { #param: #bound })
        })
        .collect::<Result<_, _>>()?;
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
        .map(|service| -> Result<TokenStream, RenderError> {
            let trait_name = format_ident!("{}Service", pascal_case(&service.name));
            let methods: Vec<TokenStream> = service
                .operations
                .iter()
                .map(|op| -> Result<TokenStream, RenderError> {
                    let method = format_ident!("{}", op.name);
                    let response = response_type(&op.response, model)?;
                    Ok(match request_type(op, model) {
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
                    })
                })
                .collect::<Result<_, _>>()?;
            Ok(quote! {
                #[async_trait::async_trait]
                impl<#(#bounds),*> #trait_name for Standalone<#(#params),*> {
                    #(#methods)*
                }
            })
        })
        .collect::<Result<_, _>>()?;

    let doc_a = doc("An immutable, owned composition root for one-shot calls.");
    let doc_b = doc("It borrows its adapters through a fresh `Ctx` per call, but does not");
    let doc_c = doc("carry model mutations between calls; mutable inbounds need a stateful root.");
    let ctx_impl = if services
        .iter()
        .any(|service| !service.operations.is_empty())
    {
        quote! {
            impl<#(#bounds),*> Standalone<#(#params),*> {
                #[doc = " The borrowed composition root one call runs over."]
                fn ctx(&self) -> Ctx<'_> {
                    Ctx {
                        model: &self.model,
                        #(#ctx_fields),*
                    }
                }
            }
        }
    } else {
        quote! {}
    };
    Ok(quote! {
        #doc_a
        #doc_b
        #doc_c
        pub struct Standalone<#(#bounds),*> {
            pub model: theseus_modeling::Model,
            #(#fields,)*
        }

        #ctx_impl

        #(#impls)*
    })
}

/// The ports whose operations a stateful session must intercept to keep its
/// working and persisted models coherent: writes that reproject to disk and
/// checkpoints that snapshot the tree. An operation reaching either is authored
/// against the session state; every other operation forwards to a per-call
/// borrowed root.
const SESSION_MANAGED_PORTS: &[&str] = &["workspace", "checkpoint"];

/// Whether an operation carries stateful behavior: its declared flow reaches a
/// session-managed port, so its body lives in an authored `_locked` hook rather
/// than a pure forward.
fn is_stateful_behavior(op: &Operation) -> bool {
    op.uses
        .iter()
        .any(|port| SESSION_MANAGED_PORTS.contains(&port.as_str()))
}

/// Render the serialized composition root's contract: `StatefulSession`'s
/// `TheseusService` impl. A pure operation locks the working model and forwards
/// to a fresh borrowed `Ctx`; an operation that reaches a session-managed port
/// forwards to an authored `<op>_locked` inherent method, where the working and
/// persisted models are reconciled. The struct, its constructors, `ctx`, and
/// the `_locked` hooks stay authored; this impl regenerates with the contract,
/// so a new pure operation reaches the serialized root with no authored edit.
pub(super) fn render_stateful_session(
    services: &[&Service],
    ports: &[&Port],
    model: &Model,
    current_crate: &str,
) -> Result<TokenStream, RenderError> {
    // The serialized session stores its project context concretely and carries
    // one generic adapter per remaining port, in declaration order.
    let adapter_ports: Vec<&&Port> = ports.iter().filter(|p| p.name != "project").collect();
    let params: Vec<proc_macro2::Ident> = adapter_ports
        .iter()
        .map(|port| format_ident!("{}Adapter", pascal_case(&port.name)))
        .collect();
    let bounds: Vec<TokenStream> = adapter_ports
        .iter()
        .zip(&params)
        .map(|(port, param)| -> Result<TokenStream, RenderError> {
            let bound = port_trait_path(port, model, current_crate)?;
            Ok(quote! { #param: #bound })
        })
        .collect::<Result<_, _>>()?;

    let impls: Vec<TokenStream> = services
        .iter()
        .map(|service| -> Result<TokenStream, RenderError> {
            let trait_name = format_ident!("{}Service", pascal_case(&service.name));
            let methods: Vec<TokenStream> = service
                .operations
                .iter()
                .map(|op| -> Result<TokenStream, RenderError> {
                    let method = format_ident!("{}", op.name);
                    let response = response_type(&op.response, model)?;
                    let behavior = is_stateful_behavior(op);
                    let locked = format_ident!("{}_locked", op.name);
                    Ok(match request_type(op, model) {
                        Some(def) => {
                            let request = format_ident!("{}", def.name);
                            if behavior {
                                quote! {
                                    async fn #method(&self, request: #request) -> anyhow::Result<#response> {
                                        self.#locked(request).await
                                    }
                                }
                            } else {
                                quote! {
                                    async fn #method(&self, request: #request) -> anyhow::Result<#response> {
                                        let state = self.state.lock().await;
                                        self.ctx(&state.working).#method(request).await
                                    }
                                }
                            }
                        }
                        None => {
                            if behavior {
                                quote! {
                                    async fn #method(&self) -> anyhow::Result<#response> {
                                        self.#locked().await
                                    }
                                }
                            } else {
                                quote! {
                                    async fn #method(&self) -> anyhow::Result<#response> {
                                        let state = self.state.lock().await;
                                        self.ctx(&state.working).#method().await
                                    }
                                }
                            }
                        }
                    })
                })
                .collect::<Result<_, _>>()?;
            Ok(quote! {
                #[async_trait::async_trait]
                impl<#(#bounds),*> #trait_name for crate::StatefulSession<#(#params),*> {
                    #(#methods)*
                }
            })
        })
        .collect::<Result<_, _>>()?;

    Ok(quote! {
        #(#impls)*
    })
}

/// Render the composition root: the model plus one field per wired port.
pub(super) fn render_composition_root(
    ports: &[&Port],
    model: &Model,
    current_crate: &str,
) -> Result<TokenStream, RenderError> {
    let fields: Vec<TokenStream> = ports
        .iter()
        .map(|port| -> Result<TokenStream, RenderError> {
            let field = format_ident!("{}", snake_case(&port.name));
            let trait_path = port_trait_path(port, model, current_crate)?;
            Ok(quote! { pub #field: &'a dyn #trait_path, })
        })
        .collect::<Result<_, _>>()?;
    let doc = doc("Composition root: the model plus the wired outbound ports.");
    Ok(quote! {
        #doc
        pub struct Ctx<'a> {
            pub model: &'a theseus_modeling::Model,
            #(#fields)*
        }
    })
}

/// The trait a port's composition-root field is typed against. A method-bearing
/// port uses its own trait. A service-targeting port uses the target service's
/// trait, qualified by the target's crate path when it lives in another crate.
pub(super) fn port_trait_path(
    port: &Port,
    model: &Model,
    current_crate: &str,
) -> Result<TokenStream, RenderError> {
    let Some(service_name) = &port.target else {
        let own = format_ident!("{}", pascal_case(&port.name));
        return Ok(quote! { #own });
    };
    let trait_name = format_ident!("{}Service", pascal_case(service_name));
    let service =
        model
            .service_named(service_name)
            .ok_or_else(|| RenderError::ServiceNotModeled {
                service: service_name.clone(),
            })?;
    Ok(match Some(service) {
        Some(service) if service.crate_name != current_crate && !service.crate_name.is_empty() => {
            let package = service.crate_name.replace('-', "_");
            validate_identifier("crate module", &package)?;
            let pkg = format_ident!("{}", package);
            quote! { #pkg::#trait_name }
        }
        _ => quote! { #trait_name },
    })
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
) -> Result<TokenStream, RenderError> {
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
            structs.push(render_request_struct(def, model)?);
        }
    }
    Ok(quote! { #(#structs)* })
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
pub(super) fn render_request_struct(
    def: &TypeDef,
    model: &Model,
) -> Result<TokenStream, RenderError> {
    let TypeShape::Struct(fields) = &def.shape else {
        return Ok(quote! {});
    };
    let name = format_ident!("{}", def.name);
    let struct_doc = doc(&format!("The `{}` request.", def.name));

    let field_defs: Vec<TokenStream> = fields
        .iter()
        .map(|field| -> Result<TokenStream, RenderError> {
            let field_doc = doc(&field.doc);
            let field_name = format_ident!("{}", field.name);
            let field_type = syn_type(&resolve_field_type(&field.ty, model))?;
            Ok(quote! {
                #field_doc
                pub #field_name: #field_type,
            })
        })
        .collect::<Result<_, _>>()?;

    Ok(quote! {
        #struct_doc
        #[derive(Debug, Clone)]
        pub struct #name {
            #(#field_defs)*
        }
    })
}

/// Render the inbound service trait: one method per operation, each defaulting to
/// an `unimplemented` error. The authored impl overrides the operations it
/// implements. An operation left on its default still compiles, and `verify`'s
/// coverage check reports it.
pub(super) fn render_service_trait(
    service: &Service,
    model: &Model,
) -> Result<TokenStream, RenderError> {
    let trait_name = format_ident!("{}Service", pascal_case(&service.name));
    let doc_a = doc("The inbound service contract: one method per operation, each defaulting");
    let doc_b = doc("to `unimplemented`. The authored impl overrides what it implements.");
    let methods: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| -> Result<TokenStream, RenderError> {
            let method_doc = doc(&op.summary);
            let method_name = format_ident!("{}", op.name);
            let param = match request_type(op, model) {
                Some(def) => {
                    let request = format_ident!("{}", def.name);
                    quote! { , _request: #request }
                }
                None => quote! {},
            };
            let response = response_type(&op.response, model)?;
            let name = op.name.as_str();
            Ok(quote! {
                #method_doc
                async fn #method_name(&self #param) -> anyhow::Result<#response> {
                    Err(Unimplemented(#name).into())
                }
            })
        })
        .collect::<Result<_, _>>()?;
    Ok(quote! {
        #doc_a
        #doc_b
        #[async_trait::async_trait]
        pub trait #trait_name: Send + Sync {
            #(#methods)*
        }
    })
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
        let rendered = render_module_for_crate(&model, "app").expect("app module renders");
        // The binding emits no port trait of its own. The composition-root field is
        // typed against the target service's trait at its crate path.
        assert!(!rendered.contains("trait Calculator"));
        assert!(rendered.contains("dyn calc::CalcService"));
    }

    #[test]
    fn a_service_without_an_inbound_renders_a_trait_but_no_command() {
        let model = Model::new("Calc")
            .service(Service::new("Calculator").operation("add", "Add.", "Empty", "Empty"));
        let rendered = render_cli_module(&model).expect("CLI renders");
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
        let plain = render_module_for_crate(&model, "calc").expect("calc module renders");
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
        let rendered = render_module_for_crate(&model, "app").expect("app module renders");
        // One generic parameter per port, bounded by the port trait, and one
        // delegation per operation through a fresh borrowed root.
        assert!(rendered.contains("pub struct Standalone<SinkAdapter: Sink>"));
        assert!(rendered.contains("An immutable, owned composition root for one-shot calls."));
        assert!(rendered.contains("mutable inbounds need a stateful root"));
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
        let rendered = render_module_for_crate(&model, "loop").expect("loop module renders");
        // The loop's crate carries the port trait, its request struct, the typed
        // default, and the budget. The service's crate carries none of it.
        assert!(rendered.contains("pub trait Llm"));
        assert!(rendered.contains("Err(Unimplemented(\"llm.complete\").into())"));
        assert!(rendered.contains("pub struct Turn"));
        assert!(rendered.contains("crate::agent::Reply"));
        assert!(rendered.contains("pub const TURN_BUDGET: usize = 32;"));
        let service_side = render_module_for_crate(&model, "app").expect("service module renders");
        assert!(!service_side.contains("trait Llm"));
        assert!(!service_side.contains("TURN_BUDGET"));
    }

    #[test]
    fn two_loops_in_one_crate_carry_named_budgets() {
        let model = Model::new("App")
            .service(Service::new("App").crate_name("app"))
            .inbound("agent", crate::model::Transport::Agent, "App", "loops")
            .turns(32)
            .inbound("scout", crate::model::Transport::Agent, "App", "loops")
            .turns(8);
        let rendered = render_module_for_crate(&model, "loops").expect("loops module renders");
        assert!(rendered.contains("pub const AGENT_TURN_BUDGET: usize = 32;"));
        assert!(rendered.contains("pub const SCOUT_TURN_BUDGET: usize = 8;"));
        assert!(!rendered.contains("pub const TURN_BUDGET"));
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
        let rendered = render_module_for_crate(&model, "app").expect("app module renders");
        assert!(rendered.contains("pub struct GatedStore<A>"));
        // The gated method guards; the ungated one passes through.
        assert!(rendered.contains("async fn write(&self, request: &str)"));
        assert!(rendered.contains("Err(Refused.into())"));
        assert!(rendered.contains("self.store.read(request).await"));

        // A port with no gated method renders no gate.
        let ungated = Model::new("App").service(Service::new("App").crate_name("app").port(
            Port::new("store", "Reads records.").method("read", "Read.", "String", "String"),
        ));
        assert!(
            !render_module_for_crate(&ungated, "app")
                .expect("ungated module renders")
                .contains("GatedStore")
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
        let rendered = render_module_for_crate(&model, "app").expect("app module renders");
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
