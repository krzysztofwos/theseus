//! The wire clients: the service contract carried over HTTP and gRPC, with
//! the status maps inverted back onto the typed error classes.

use super::{
    proto::{proto_package, proto_request_message},
    *,
};

/// Render an HTTP client adapter: the target service's contract implemented
/// over the wire — each call posts its request as a JSON body and maps the
/// reply's status back onto the contract's error classes, so the classes the
/// server mapped onto the wire survive the crossing back.
pub(super) fn render_http_client_module(
    client: &Client,
    service: &Service,
    model: &Model,
) -> TokenStream {
    let ContractPaths {
        prefix,
        service_trait: trait_path,
        unimplemented: unimplemented_path,
        refused: refused_path,
    } = contract_paths(&client.crate_name, service, model);
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
            let finish = match response_kind(op, model) {
                ResponseKind::Empty => quote! {
                    checked(#op_name, status, &body)?;
                    Ok(())
                },
                ResponseKind::Text => quote! {
                    checked(#op_name, status, &body)?;
                    Ok(body)
                },
                ResponseKind::Json => quote! {
                    checked(#op_name, status, &body)?;
                    parsed(#op_name, &body)
                },
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
pub(super) fn render_grpc_client_module(
    client: &Client,
    service: &Service,
    model: &Model,
) -> TokenStream {
    let ContractPaths {
        prefix,
        service_trait: trait_path,
        unimplemented: unimplemented_path,
        refused: refused_path,
    } = contract_paths(&client.crate_name, service, model);
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
            let finish = match response_kind(op, model) {
                ResponseKind::Empty => quote! {
                    reply.into_inner();
                    Ok(())
                },
                ResponseKind::Text => quote! { Ok(reply.into_inner().value) },
                ResponseKind::Json => quote! { parsed(#op_name, &reply.into_inner().json) },
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
pub(super) fn render_client_parsed(service: &Service, model: &Model) -> TokenStream {
    let has_json_response = service
        .operations
        .iter()
        .any(|op| matches!(response_kind(op, model), ResponseKind::Json));
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
pub(super) fn grpc_client_field_conversion(field: &Field, model: &Model) -> TokenStream {
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
pub(super) fn render_grpc_client_enum_conversions(service: &Service, model: &Model) -> TokenStream {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
