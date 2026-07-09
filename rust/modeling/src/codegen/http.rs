//! The HTTP surface: the operation handlers with their structural status map.

use super::*;

/// Render an inbound HTTP adapter: the operation handlers over the service trait,
/// with request parsers from a call's JSON body and the reply's status map. The
/// status derives from the outcome's structure — 200 a result, 400 a request that
/// does not parse, 404 an unknown operation, 501 an operation with no authored
/// handler, 403 a refused write, and 500 any other error.
pub(super) fn render_http_module(
    inbound: &Inbound,
    service: &Service,
    model: &Model,
) -> TokenStream {
    let ContractPaths {
        prefix,
        service_trait: trait_path,
        unimplemented: unimplemented_path,
        refused: refused_path,
    } = contract_paths(&inbound.crate_name, service, model);

    // A parser per distinct request struct, building the request from the call's
    // JSON body — the one wire conversion the tool dispatch renders too.
    let operations: Vec<&Operation> = service.operations.iter().collect();
    let parsers = render_json_parsers(&operations, model, &prefix, "http", false);

    let arms: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| {
            let name = op.name.as_str();
            let method = format_ident!("{}", op.name);
            let render = match response_kind(op, model) {
                ResponseKind::Text => format_ident!("reply_text"),
                ResponseKind::Empty | ResponseKind::Json => format_ident!("reply_json"),
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
    let input = request_binding(&operations, model);

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

        #parsers

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
