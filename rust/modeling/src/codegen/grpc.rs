//! The gRPC server surface: the service glue over the build's server trait,
//! with its conversions and status map.

use super::{
    proto::{
        proto_package, proto_request_message, proto_response_message,
        proto_scalar_requires_presence,
    },
    *,
};

/// Render an inbound gRPC adapter: the proto module the build compiles, the
/// service glue implementing the transport's generated server trait over the
/// service contract, and the reply's status map. The status derives from the
/// outcome's structure — OK a result, UNIMPLEMENTED an operation with no
/// authored handler, PERMISSION_DENIED a refused write, INTERNAL any other
/// error.
pub(super) fn render_grpc_module(
    inbound: &Inbound,
    service: &Service,
    model: &Model,
) -> Result<TokenStream, RenderError> {
    render_proto(model, service)?;
    validate_grpc_conversions(service, model)?;
    let ContractPaths {
        prefix,
        service_trait: trait_path,
        unimplemented: unimplemented_path,
        refused: refused_path,
    } = contract_paths(&inbound.crate_name, service, model)?;
    let package = proto_package(model, service);
    let server_mod = format_ident!("{}_server", proto_snake_case(&service.name));
    let server_trait = format_ident!("{}", pascal_case(&service.name));
    let glue = format_ident!("Grpc{}", pascal_case(&service.name));

    let methods: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| -> Result<TokenStream, RenderError> {
            let method = format_ident!("{}", op.name);
            let request_msg = format_ident!("{}", proto_request_message(op, model)?.0);
            let response_msg = format_ident!("{}", proto_response_message(op, model)?.0);
            let call = match request_type(op, model) {
                Some(def) => {
                    let ty = syn_type(&format!("{prefix}{}", def.name))?;
                    let TypeShape::Struct(fields) = &def.shape else {
                        return Err(RenderError::InvalidRequestType {
                            service: service.name.clone(),
                            operation: op.name.clone(),
                            request: op.request.clone(),
                        });
                    };
                    let inits: Vec<TokenStream> = fields
                        .iter()
                        .map(|field| grpc_field_conversion(field, model))
                        .collect::<Result<_, _>>()?;
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
            let respond = match response_kind(op, model) {
                ResponseKind::Empty => {
                    quote! { Ok(_) => Ok(tonic::Response::new(proto::#response_msg {})), }
                }
                ResponseKind::Text => {
                    quote! { Ok(value) => Ok(tonic::Response::new(proto::#response_msg { value })), }
                }
                // A foreign-typed response carries its JSON rendering.
                ResponseKind::Json => quote! {
                    Ok(value) => match serde_json::to_string(&value) {
                        Ok(json) => Ok(tonic::Response::new(proto::#response_msg { json })),
                        Err(error) => Err(tonic::Status::internal(error.to_string())),
                    },
                },
            };
            Ok(quote! {
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
            })
        })
        .collect::<Result<_, _>>()?;

    let conversions = render_grpc_enum_conversions(service, model)?;
    let doc_proto = doc("The wire types and service glue the build compiles from the proto.");
    let doc_glue_a = doc("The service glue: the transport's generated server trait, implemented");
    let doc_glue_b = doc("over any implementation of the service contract.");
    Ok(quote! {
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
    })
}

/// The conversion one request field needs from its wire form to the contract's:
/// a map collects into the contract's ordered map, an enum-typed field converts
/// through its generated conversion, a required scalar is unwrapped from proto
/// presence, and a defaulting or optional scalar passes through.
pub(super) fn grpc_field_conversion(
    field: &Field,
    model: &Model,
) -> Result<TokenStream, RenderError> {
    let name = format_ident!("{}", field.name);
    let base = base_label(&field.ty);
    let unwrapped = optional_inner(&field.ty).unwrap_or(&field.ty);
    if unwrapped.starts_with("BTreeMap<") {
        return Ok(quote! { #name: request.#name.into_iter().collect() });
    }
    match model.type_def(base).map(|def| &def.shape) {
        Some(TypeShape::Enum { .. }) => {
            let convert = format_ident!("{}_from_proto", proto_snake_case(base));
            if field.ty.starts_with("Vec<") {
                Ok(quote! {
                    #name: request
                        .#name
                        .into_iter()
                        .map(#convert)
                        .collect::<Result<Vec<_>, tonic::Status>>()?
                })
            } else {
                let missing = format!("field `{}` is required", field.name);
                Ok(quote! {
                    #name: #convert(
                        request
                            .#name
                            .ok_or_else(|| tonic::Status::invalid_argument(#missing))?,
                    )?
                })
            }
        }
        Some(TypeShape::Struct(_)) => Err(RenderError::UnsupportedNestedStructType {
            field: field.name.clone(),
            ty: field.ty.clone(),
        }),
        _ if proto_scalar_requires_presence(&field.ty) => {
            let missing = format!("field `{}` is required", field.name);
            Ok(quote! {
                #name: request
                    .#name
                    .ok_or_else(|| tonic::Status::invalid_argument(#missing))?
            })
        }
        _ => Ok(quote! { #name: request.#name }),
    }
}

/// Render a conversion per rich enum the service's request fields carry: the
/// wire's `oneof` message becomes the contract's enum, variant by variant. A
/// message with no verb set is an invalid argument.
pub(super) fn render_grpc_enum_conversions(
    service: &Service,
    model: &Model,
) -> Result<TokenStream, RenderError> {
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
        .map(
            |(def, variants, path)| -> Result<TokenStream, RenderError> {
                let enum_mod = format_ident!("{}", proto_snake_case(&def.name));
                let enum_msg = format_ident!("{}", def.name);
                let convert = format_ident!("{}_from_proto", proto_snake_case(&def.name));
                let rust_path = syn_type(path)?;
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
                                } else if proto_scalar_requires_presence(&field.ty) {
                                    let missing = format!("field `{}` is required", field.name);
                                    quote! {
                                        #name: data
                                            .#name
                                            .ok_or_else(|| tonic::Status::invalid_argument(#missing))?
                                    }
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
                Ok(quote! {
                    #doc_line
                    fn #convert(value: proto::#enum_msg) -> Result<#rust_path, tonic::Status> {
                        match value.verb {
                            #(#arms)*
                            None => Err(tonic::Status::invalid_argument(#missing)),
                        }
                    }
                })
            },
        )
        .collect::<Result<_, _>>()?;
    Ok(quote! { #(#conversions)* })
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let proto = render_proto(&model, service).expect("calculator proto renders");
        assert!(proto.contains("package app.calculator;"), "{proto}");
        assert!(proto.contains("message Operands {"));
        assert!(proto.contains("optional double a = 1;"));
        assert!(proto.contains("message CalcResult {\n  string value = 1;\n}"));
        assert!(proto.contains("rpc Add (Operands) returns (CalcResult);"));

        let rendered =
            render_module_for_crate(&model, "calc-grpc").expect("calculator gRPC module renders");
        assert!(rendered.contains("pub struct GrpcCalculator"), "{rendered}");
        assert!(rendered.contains("include_proto"));
        assert!(rendered.contains("calc::Unimplemented"));
        assert!(rendered.contains("unimplemented") && rendered.contains("permission_denied"));
        assert!(rendered.contains("field `a` is required"), "{rendered}");
    }

    #[test]
    fn required_grpc_scalars_track_presence_across_server_and_client() {
        let model = Model::new("App")
            .crate_node("app", "app", 0, &[])
            .crate_node("app-grpc", "app-grpc", 1, &["app"])
            .crate_node("app-grpc-client", "app-grpc-client", 1, &["app"])
            .struct_type(
                "Retention",
                &[
                    ("keep", "u32", "Number to retain."),
                    ("label", "String", "Retention label."),
                    ("ratio", "f64", "Retention ratio."),
                    ("dry_run", "bool", "Preview without applying."),
                    ("ceiling", "Option<u32>", "Optional ceiling."),
                ],
            )
            .service(Service::new("App").crate_name("app").operation(
                "prune",
                "Prune retained values.",
                "Retention",
                "String",
            ))
            .inbound("grpc", Transport::Grpc, "App", "app-grpc")
            .client("grpc-client", Transport::Grpc, "App", "app-grpc-client");
        let service = model.service_named("App").expect("modeled");

        let proto = render_proto(&model, service).expect("presence-aware proto renders");
        assert!(proto.contains("optional uint32 keep = 1;"), "{proto}");
        assert!(proto.contains("optional string label = 2;"), "{proto}");
        assert!(proto.contains("optional double ratio = 3;"), "{proto}");
        assert!(proto.contains("bool dry_run = 4;"), "{proto}");
        assert!(proto.contains("optional uint32 ceiling = 5;"), "{proto}");

        let server =
            render_module_for_crate(&model, "app-grpc").expect("gRPC server module renders");
        assert!(server.contains("field `keep` is required"), "{server}");
        assert!(server.contains("field `label` is required"), "{server}");
        assert!(server.contains("field `ratio` is required"), "{server}");
        assert!(!server.contains("field `dry_run` is required"), "{server}");
        assert!(!server.contains("field `ceiling` is required"), "{server}");

        let client =
            render_module_for_crate(&model, "app-grpc-client").expect("gRPC client module renders");
        assert!(client.contains("keep: Some(request.keep)"), "{client}");
        assert!(client.contains("label: Some(request.label)"), "{client}");
        assert!(client.contains("ratio: Some(request.ratio)"), "{client}");
        assert!(client.contains("dry_run: request.dry_run"), "{client}");
        assert!(client.contains("ceiling: request.ceiling"), "{client}");
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

        let proto = render_proto(&model, service).expect("app proto renders");
        assert!(proto.contains("package app;"), "{proto}");
        assert!(proto.contains("message Edit {"));
        assert!(proto.contains("oneof verb {"));
        assert!(proto.contains("Add add = 1;"));
        assert!(proto.contains("optional string name = 1;"), "{proto}");
        assert!(proto.contains("map<string, string> attrs"));
        assert!(
            !proto.contains("optional map"),
            "a map is never optional: {proto}"
        );
        assert!(proto.contains("message PatchResult {\n  string json = 1;\n}"));

        let rendered =
            render_module_for_crate(&model, "app-grpc").expect("app gRPC module renders");
        assert!(rendered.contains("fn edit_from_proto"), "{rendered}");
        assert!(rendered.contains("Verb::Add(data)"));
        assert!(rendered.contains("into_iter().collect()"));
        assert!(rendered.contains("field `name` is required"), "{rendered}");
        assert!(rendered.contains("serde_json::to_string"));
        assert!(rendered.contains("carries one verb"));
    }
}
