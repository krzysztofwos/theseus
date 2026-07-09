//! The agent tool surface: the catalog with its JSON schemas, the tool
//! dispatch, and the tool parsers.

use super::*;

/// Render the agent tool catalog: one tool-use definition per exposed operation,
/// its `input_schema` derived from the operation's request contract. The agent
/// loop and the MCP server both serve it, so they expose one tool surface.
pub(super) fn render_tool_catalog(operations: &[&Operation], model: &Model) -> TokenStream {
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
pub(super) fn render_tool_dispatch(
    operations: &[&Operation],
    service: &Service,
    model: &Model,
) -> Result<TokenStream, RenderError> {
    let trait_name = format_ident!("{}Service", pascal_case(&service.name));
    let parsers = render_tool_parsers(operations, model)?;
    let arms: Vec<TokenStream> = operations
        .iter()
        .map(|op| {
            let name = op.name.as_str();
            let method = format_ident!("{}", op.name);
            let call = match request_type(op, model) {
                Some(def) => {
                    let parser = format_ident!("parse_{}_input", proto_snake_case(&def.name));
                    quote! { service.#method(#parser(input)?).await? }
                }
                None => quote! { service.#method().await? },
            };
            let render = match response_kind(op, model) {
                ResponseKind::Text => quote! { Ok(#call) },
                ResponseKind::Empty | ResponseKind::Json => {
                    quote! { Ok(serde_json::to_string(&#call)?) }
                }
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
    let input = request_binding(operations, model);
    let doc_a = doc("Dispatch one tool call to the service: parse the request from the");
    let doc_b = doc("call's JSON input, run the operation, and render the result — text");
    let doc_c = doc("for a string, otherwise JSON. The catalog and this dispatch render");
    let doc_d = doc("from the same contract, so every catalog entry has an arm here.");
    Ok(quote! {
        #parsers
        #doc_a
        #doc_b
        #doc_c
        #doc_d
        pub async fn dispatch_tool(
            service: &impl #trait_name,
            name: &str,
            #input: &serde_json::Value,
        ) -> anyhow::Result<String> {
            match name {
                #(#arms)*
                other => anyhow::bail!(#unknown),
            }
        }
    })
}

/// Render a parser per distinct request struct the exposed operations take,
/// building the request from a tool call's JSON input. The wire-to-domain
/// conversion is rendered with the dispatch, so the struct itself stays
/// transport-neutral.
pub(super) fn render_tool_parsers(
    operations: &[&Operation],
    model: &Model,
) -> Result<TokenStream, RenderError> {
    render_json_parsers(operations, model, "", "input", true)
}

/// Render an operation's JSON-schema `input_schema` from its request contract. An
/// `Empty` or fieldless request is an empty object. A field's type sets its schema
/// type, and a field required unless it is a `bool` or an `Option`.
pub(super) fn render_tool_schema(op: &Operation, model: &Model) -> TokenStream {
    let fields = request_fields(op, model);
    let object = render_object_schema(fields, model);
    quote! { #object }
}

/// Render an object schema from a set of fields: a property per field, and a
/// `required` list of the fields that are neither `Option` nor `bool`.
pub(super) fn render_object_schema(fields: &[Field], model: &Model) -> TokenStream {
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
pub(super) fn render_schema_property(field: &Field, model: &Model) -> TokenStream {
    let key = field.name.as_str();
    let schema = render_type_schema(&field.ty, model);
    quote! { #key: #schema }
}

/// The JSON schema for a contract type label. A `Vec<T>` is an array of its
/// element schema, a `BTreeMap<_, V>` an object of `V`-typed properties, an enum a
/// `oneOf` over its variants, and anything else its scalar type. `Option<T>` has
/// `T`'s schema — optionality is carried by the enclosing `required` list.
pub(super) fn render_type_schema(label: &str, model: &Model) -> TokenStream {
    let label = optional_inner(label).unwrap_or(label);
    if let Some(inner) = vec_inner(label) {
        let items = render_type_schema(inner, model);
        return quote! { { "type": "array", "items": #items } };
    }
    if let Some(value) = map_value(label) {
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
pub(super) fn render_variant_schema(variant: &Variant, model: &Model) -> TokenStream {
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

/// The JSON-schema type for a contract type label.
pub(super) fn json_schema_type(ty: &str) -> &'static str {
    match ty {
        "bool" => "boolean",
        "f64" | "f32" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize"
        | "isize" => "number",
        _ => "string",
    }
}

/// Whether a request field is a required schema property: not a `bool` (which
/// defaults false) and not an `Option` (which is absent when unset).
pub(super) fn schema_required(ty: &str) -> bool {
    ty != "bool" && optional_inner(ty).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let rendered =
            render_module_for_crate(&patched.unwrap(), "app").expect("tool module renders");
        // The patched-in operation joins the catalog beside the authored one.
        assert!(
            rendered.contains(r#""ping" =>"#),
            "the dispatch lacks a ping arm: {rendered}"
        );
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
        let rendered = render_module_for_crate(&model, "app").expect("tool module renders");
        assert!(
            rendered.contains("pub async fn dispatch_tool"),
            "{rendered}"
        );
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
}
