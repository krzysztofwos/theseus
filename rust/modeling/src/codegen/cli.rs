//! The CLI surface: the command tree, the argument parsers, the parsed
//! invocation, and the dispatch.

use super::*;

/// Render one operation as a subcommand, its request fields as arguments.
pub(super) fn render_subcommand(op: &Operation, model: &Model) -> Result<TokenStream, RenderError> {
    let name = &op.name;
    let summary = &op.summary;
    let args: Vec<TokenStream> = request_fields(op, model)
        .iter()
        .map(|field| render_arg(field, model))
        .collect::<Result<_, _>>()?;
    Ok(quote! {
        .subcommand(Command::new(#name).about(#summary) #(#args)*)
    })
}

/// Render one request field as a command-line argument. The field type decides
/// the shape: `bool` is a flag, `Vec<T>` a repeatable value, `Option<T>` an
/// optional value, anything else a required value. A non-`String` value type is
/// parsed and validated as that type.
pub(super) fn render_arg(field: &Field, model: &Model) -> Result<TokenStream, RenderError> {
    let flag = field.name.replace('_', "-");
    let help = &field.doc;
    if field.ty == "bool" {
        Ok(quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::SetTrue).help(#help)) })
    } else if field.ty.starts_with("Vec<") {
        Ok(quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::Append).help(#help)) })
    } else if let Some(inner) = optional_inner(&field.ty) {
        let parser = value_parser(&resolve_field_type(inner, model))?;
        Ok(quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::Set) #parser .help(#help)) })
    } else {
        let parser = value_parser(&resolve_field_type(&field.ty, model))?;
        Ok(
            quote! { .arg(Arg::new(#flag).long(#flag).action(ArgAction::Set).required(true) #parser .help(#help)) },
        )
    }
}

/// The `.value_parser(...)` fragment for a typed argument. A `String` value needs
/// none. Any other value type is parsed and validated as that type.
pub(super) fn value_parser(ty: &str) -> Result<TokenStream, RenderError> {
    if ty == "String" {
        Ok(quote! {})
    } else {
        let ty = syn_type(ty)?;
        Ok(quote! { .value_parser(clap::value_parser!(#ty)) })
    }
}

/// The expression that reads one request field from the parsed arguments: a flag
/// for `bool`, the collected values for `Vec<T>`, an optional value for
/// `Option<T>`, otherwise a required value. A non-`String` value is read as its
/// parsed type.
pub(super) fn field_parse(field: &Field, model: &Model) -> Result<TokenStream, RenderError> {
    let flag = field.name.replace('_', "-");
    if field.ty == "bool" {
        Ok(quote! { matches.get_flag(#flag) })
    } else if let Some(inner) = vec_inner(&field.ty) {
        if inner == "String" {
            Ok(
                quote! { matches.get_many::<String>(#flag).map(|values| values.cloned().collect()).unwrap_or_default() },
            )
        } else {
            // A `Vec` of a structured type: each repeatable value is a compact
            // string the element's `FromStr` decodes.
            let element = syn_type(&rust_type(inner, model))?;
            Ok(quote! {
                matches
                    .get_many::<String>(#flag)
                    .map(|values| values.map(|value| value.parse::<#element>()).collect::<Result<Vec<_>, _>>())
                    .transpose()?
                    .unwrap_or_default()
            })
        }
    } else if let Some(inner) = optional_inner(&field.ty) {
        if inner == "String" {
            Ok(quote! { arg(#flag) })
        } else {
            let inner = syn_type(&rust_type(inner, model))?;
            Ok(quote! { matches.get_one::<#inner>(#flag).cloned() })
        }
    } else if field.ty == "String" {
        Ok(quote! { arg(#flag).unwrap_or_default() })
    } else {
        let ty = syn_type(&resolve_field_type(&field.ty, model))?;
        Ok(quote! { matches.get_one::<#ty>(#flag).cloned().unwrap_or_default() })
    }
}

/// The parser function name for a request struct, e.g. `parse_operands`.
pub(super) fn parser_fn(def: &TypeDef) -> proc_macro2::Ident {
    format_ident!("parse_{}", proto_snake_case(&def.name))
}

/// Render an inbound CLI adapter: the command surface, the request parsers, the
/// parsed invocation, and the dispatch for the service it drives.
/// Request types are qualified by the service's crate path, so the adapter may
/// live in a crate other than the one that defines them.
pub(super) fn render_inbound_module(
    inbound: &Inbound,
    service: &Service,
    model: &Model,
) -> Result<TokenStream, RenderError> {
    let bin = &inbound.name;
    let ContractPaths {
        prefix,
        service_trait: trait_path,
        ..
    } = contract_paths(&inbound.crate_name, service, model)?;

    let subcommands: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| render_subcommand(op, model))
        .collect::<Result<_, _>>()?;
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

    let parsers = render_inbound_parsers(service, model, &prefix)?;

    let variants: Vec<TokenStream> = service
        .operations
        .iter()
        .map(|op| -> Result<TokenStream, RenderError> {
            let variant = format_ident!("{}", pascal_case(&op.name));
            match request_type(op, model) {
                Some(def) => {
                    let ty = syn_type(&format!("{prefix}{}", def.name))?;
                    Ok(quote! { #variant(#ty), })
                }
                None => Ok(quote! { #variant, }),
            }
        })
        .collect::<Result<_, _>>()?;
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
            let render = match response_kind(op, model) {
                ResponseKind::Text => quote! { println!("{}", #call) },
                ResponseKind::Empty | ResponseKind::Json => {
                    quote! { println!("{}", serde_json::to_string_pretty(&#call)?) }
                }
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

    Ok(quote! {
        #command
        #parsers
        #invocation
        #dispatch
    })
}

/// Render a free-function parser per distinct request struct the service's
/// operations take, building the request type at its crate-qualified path.
pub(super) fn render_inbound_parsers(
    service: &Service,
    model: &Model,
    prefix: &str,
) -> Result<TokenStream, RenderError> {
    let mut seen: Vec<&str> = Vec::new();
    let mut parsers = Vec::new();
    for def in service
        .operations
        .iter()
        .filter_map(|op| request_type(op, model))
    {
        if seen.contains(&def.name.as_str()) {
            continue;
        }
        seen.push(&def.name);
        let TypeShape::Struct(fields) = &def.shape else {
            continue;
        };
        let fn_name = parser_fn(def);
        let ty = syn_type(&format!("{prefix}{}", def.name))?;
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
            .map(|field| -> Result<TokenStream, RenderError> {
                let field_name = format_ident!("{}", field.name);
                let parse = field_parse(field, model)?;
                Ok(quote! { #field_name: #parse, })
            })
            .collect::<Result<_, _>>()?;
        parsers.push(quote! {
            fn #fn_name(matches: &ArgMatches) -> anyhow::Result<#ty> {
                #arg_closure
                Ok(#ty {
                    #(#inits)*
                })
            }
        });
    }
    Ok(quote! { #(#parsers)* })
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let rendered = render_cli_module(&model).expect("CLI renders");
        // The inbound's parser validates the argument as its type and reads it back.
        assert!(rendered.contains("value_parser"));
        assert!(rendered.contains("get_one::<f64>"));
        assert!(rendered.contains("fn parse_operands"));
    }
}
