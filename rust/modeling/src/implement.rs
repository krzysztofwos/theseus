//! Authored bodies: read and write the handlers and adapter methods.
//!
//! [`handler_source`] returns an operation's current handler text and
//! [`implement`] writes one into the source implementing the service trait.
//! [`adapter_source`] and [`implement_adapter`] do the same for a port method's
//! adapter — the impl of the port's trait in the crate's authored adapters
//! file, named by adapter when the file holds more than one. Every signature
//! renders from the model with absolute paths, so a written method needs no
//! imports, and every method is located by its exact source span, so the rest
//! of the authored file is preserved byte for byte.

use std::ops::Range;

use syn::spanned::Spanned;

use crate::{
    codegen::{adapter_signature, handler_signature, pascal_case},
    coverage::service_trait_name,
    model::{Method, Model, Operation, Port, Service},
};

/// Why a handler could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ImplementError {
    /// The model has no operation by that name.
    #[error("no operation named `{0}`")]
    UnknownOperation(String),
    /// The source has no block implementing the service trait.
    #[error("no `impl {0} for …` block found in the authored impl")]
    NoImplBlock(String),
    /// The authored impl did not parse as Rust.
    #[error("parsing the authored impl: {0}")]
    Parse(String),
    /// The model has no port by that name.
    #[error("no port named `{0}`")]
    UnknownPort(String),
    /// The port has no method by that name.
    #[error("port `{port}` has no method named `{method}`")]
    UnknownPortMethod { port: String, method: String },
    /// The authored file holds more than one adapter for the port's trait.
    #[error("adapters {adapters} implement `{trait_name}` here; name one with `adapter`")]
    AmbiguousAdapter {
        trait_name: String,
        adapters: String,
    },
    /// The named adapter does not implement the port's trait in the file.
    #[error("no adapter `{adapter}` implements `{trait_name}` in the authored file")]
    UnknownAdapter { trait_name: String, adapter: String },
}

/// The current adapter method text for a port method: the authored method when
/// one exists, otherwise the signature it would have, marked as falling through
/// to the trait default. `adapter` names the implementing type when the file
/// holds more than one adapter for the port's trait.
pub fn adapter_source(
    model: &Model,
    impl_source: &str,
    port_name: &str,
    method_name: &str,
    adapter: Option<&str>,
) -> Result<String, ImplementError> {
    let (port, method) = locate_port_method(model, port_name, method_name)?;
    let trait_name = pascal_case(&port.name);
    let target = choose_adapter(impl_source, &trait_name, method_name, adapter)?;
    match target.method {
        Some(range) => Ok(impl_source[range].to_string()),
        None => {
            let signature = adapter_signature(method, model, "");
            Ok(format!(
                "{signature} {{
    // unimplemented — falls through to the trait default
}}"
            ))
        }
    }
}

/// Write an adapter method for a port into `impl_source`, returning the new
/// source. The method is inserted when the adapter leaves it on the trait
/// default and replaced in place when it is authored. `adapter` names the
/// implementing type when the file holds more than one adapter for the port's
/// trait, and `request_path` prefixes the model's own types so the written
/// method compiles without imports.
pub fn implement_adapter(
    model: &Model,
    impl_source: &str,
    port_name: &str,
    method_name: &str,
    adapter: Option<&str>,
    body: &str,
    request_path: &str,
) -> Result<String, ImplementError> {
    let (port, method) = locate_port_method(model, port_name, method_name)?;
    let trait_name = pascal_case(&port.name);
    let signature = adapter_signature(method, model, request_path);
    let target = choose_adapter(impl_source, &trait_name, method_name, adapter)?;
    match target.method {
        Some(range) => {
            let method = format!(
                "{signature} {{
{body}
    }}"
            );
            let mut out = String::with_capacity(impl_source.len() + method.len());
            out.push_str(&impl_source[..range.start]);
            out.push_str(&method);
            out.push_str(&impl_source[range.end..]);
            Ok(out)
        }
        None => {
            let method = format!(
                "
    {signature} {{
{body}
    }}
"
            );
            let mut out = String::with_capacity(impl_source.len() + method.len());
            out.push_str(&impl_source[..target.body_start]);
            out.push_str(&method);
            out.push_str(&impl_source[target.body_start..]);
            Ok(out)
        }
    }
}

/// The port and method a `port.method` pair resolves to.
fn locate_port_method<'a>(
    model: &'a Model,
    port_name: &str,
    method_name: &str,
) -> Result<(&'a Port, &'a Method), ImplementError> {
    let port = model
        .ports()
        .find(|port| port.name == port_name)
        .ok_or_else(|| ImplementError::UnknownPort(port_name.to_string()))?;
    let method = port
        .methods
        .iter()
        .find(|method| method.name == method_name)
        .ok_or_else(|| ImplementError::UnknownPortMethod {
            port: port_name.to_string(),
            method: method_name.to_string(),
        })?;
    Ok((port, method))
}

/// One adapter impl of a port's trait: the implementing type's name, the
/// position just inside its opening brace, and the target method's span when
/// the adapter authors it.
struct AdapterImpl {
    self_type: String,
    body_start: usize,
    method: Option<Range<usize>>,
}

/// The adapter impl a splice targets: the named one, or the only one.
fn choose_adapter(
    source: &str,
    trait_name: &str,
    method: &str,
    adapter: Option<&str>,
) -> Result<AdapterImpl, ImplementError> {
    let mut impls = adapter_impls(source, trait_name, method)?;
    match adapter {
        Some(name) => impls
            .into_iter()
            .find(|block| block.self_type == name)
            .ok_or_else(|| ImplementError::UnknownAdapter {
                trait_name: trait_name.to_string(),
                adapter: name.to_string(),
            }),
        None => match impls.len() {
            0 => Err(ImplementError::NoImplBlock(trait_name.to_string())),
            1 => Ok(impls.remove(0)),
            _ => Err(ImplementError::AmbiguousAdapter {
                trait_name: trait_name.to_string(),
                adapters: impls
                    .iter()
                    .map(|block| block.self_type.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            }),
        },
    }
}

/// Every impl of `trait_name` in `source`, with the `method` span each authors.
fn adapter_impls(
    source: &str,
    trait_name: &str,
    method: &str,
) -> Result<Vec<AdapterImpl>, ImplementError> {
    let file = syn::parse_file(source).map_err(|error| ImplementError::Parse(error.to_string()))?;
    let mut impls = Vec::new();
    for block in trait_impls(&file, trait_name) {
        let self_type = impl_self_type(block);
        let found = block.items.iter().find_map(|impl_item| {
            if let syn::ImplItem::Fn(function) = impl_item
                && function.sig.ident == method
            {
                Some(function.span().byte_range())
            } else {
                None
            }
        });
        impls.push(AdapterImpl {
            self_type,
            body_start: block.brace_token.span.open().byte_range().end,
            method: found,
        });
    }
    Ok(impls)
}

/// The current handler text for `op_name`: the authored method when one exists,
/// otherwise the signature it would have, marked as falling through to the default.
pub fn handler_source(
    model: &Model,
    impl_source: &str,
    op_name: &str,
) -> Result<String, ImplementError> {
    let (service, operation) = locate(model, op_name)?;
    let trait_name = service_trait_name(service);
    match method_range(impl_source, &trait_name, op_name)? {
        Some(range) => Ok(impl_source[range].to_string()),
        None => {
            let signature = handler_signature(operation, model, "");
            Ok(format!(
                "{signature} {{\n    // unimplemented — falls through to the trait default\n}}"
            ))
        }
    }
}

/// Write a handler for `op_name` into `impl_source`, returning the new source.
///
/// The method is inserted when the operation has no handler yet and replaced in
/// place when it does. `request_path` prefixes the request type so the written
/// method compiles without imports, e.g. `crate::generated::`.
pub fn implement(
    model: &Model,
    impl_source: &str,
    op_name: &str,
    body: &str,
    request_path: &str,
) -> Result<String, ImplementError> {
    let (service, operation) = locate(model, op_name)?;
    let trait_name = service_trait_name(service);
    let signature = handler_signature(operation, model, request_path);

    match method_range(impl_source, &trait_name, op_name)? {
        Some(range) => {
            let method = format!("{signature} {{\n{body}\n    }}");
            let mut out = String::with_capacity(impl_source.len() + method.len());
            out.push_str(&impl_source[..range.start]);
            out.push_str(&method);
            out.push_str(&impl_source[range.end..]);
            Ok(out)
        }
        None => {
            let method = format!("    {signature} {{\n{body}\n    }}\n");
            splice_after_header(impl_source, &trait_name, &method)
        }
    }
}

/// The service and operation an operation name resolves to.
fn locate<'a>(
    model: &'a Model,
    op_name: &str,
) -> Result<(&'a Service, &'a Operation), ImplementError> {
    model
        .services
        .iter()
        .find_map(|service| {
            service
                .operations
                .iter()
                .find(|op| op.name == op_name)
                .map(|op| (service, op))
        })
        .ok_or_else(|| ImplementError::UnknownOperation(op_name.to_string()))
}

/// Every `impl <trait_name> for …` block in a parsed file. The one scan that
/// coverage, flow extraction, and both splice paths locate trait impls through,
/// so they agree on which impls count.
pub(crate) fn trait_impls<'a>(file: &'a syn::File, trait_name: &str) -> Vec<&'a syn::ItemImpl> {
    file.items
        .iter()
        .filter_map(|item| match item {
            syn::Item::Impl(block) => Some(block),
            _ => None,
        })
        .filter(|block| {
            block
                .trait_
                .as_ref()
                .and_then(|(_, path, _)| path.segments.last())
                .is_some_and(|segment| segment.ident == trait_name)
        })
        .collect()
}

/// The implementing type's name of a trait impl: the last path segment of its
/// self type, or the whole self type rendered when it is not a plain path.
pub(crate) fn impl_self_type(block: &syn::ItemImpl) -> String {
    match block.self_ty.as_ref() {
        syn::Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default(),
        other => quote::quote!(#other).to_string(),
    }
}

/// The byte range of the `method` in the `impl <trait_name> for …` block, or
/// `None` when the block has no such method. The range covers the whole item,
/// including any attributes and doc comments on it, so a replacement rewrites
/// them along with the body.
fn method_range(
    source: &str,
    trait_name: &str,
    method: &str,
) -> Result<Option<Range<usize>>, ImplementError> {
    let file = syn::parse_file(source).map_err(|error| ImplementError::Parse(error.to_string()))?;
    for block in trait_impls(&file, trait_name) {
        for impl_item in &block.items {
            if let syn::ImplItem::Fn(function) = impl_item
                && function.sig.ident == method
            {
                return Ok(Some(function.span().byte_range()));
            }
        }
    }
    Ok(None)
}

/// Insert `method` just inside the `impl <trait_name> for …` block, located by
/// its exact brace span.
fn splice_after_header(
    source: &str,
    trait_name: &str,
    method: &str,
) -> Result<String, ImplementError> {
    let file = syn::parse_file(source).map_err(|error| ImplementError::Parse(error.to_string()))?;
    let brace_at = trait_impls(&file, trait_name)
        .first()
        .map(|block| block.brace_token.span.open().byte_range().end)
        .ok_or_else(|| ImplementError::NoImplBlock(trait_name.to_string()))?;

    let mut out = String::with_capacity(source.len() + method.len() + 1);
    out.push_str(&source[..brace_at]);
    out.push('\n');
    out.push_str(method);
    out.push_str(&source[brace_at..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    const IMPL: &str = "impl SampleService for Ctx<'_> {\n    fn greet(&self) -> anyhow::Result<()> {\n        Ok(())\n    }\n}\n";

    /// The rendered source must remain valid Rust.
    fn parses(source: &str) -> bool {
        syn::parse_file(source).is_ok()
    }

    #[test]
    fn inserts_a_handler_for_an_unimplemented_operation() {
        let out = implement(
            &sample_model(),
            IMPL,
            "status",
            "Ok(())",
            "crate::generated::",
        )
        .unwrap();
        assert!(parses(&out));
        assert!(out.contains("fn status(&self) -> anyhow::Result<()>"));
        // The existing handler is preserved, the new one lands inside the block.
        assert!(out.contains("fn greet"));
        assert!(out.find("fn status").unwrap() < out.find("fn greet").unwrap());
    }

    #[test]
    fn replaces_an_existing_handler_in_place() {
        let out = implement(&sample_model(), IMPL, "greet", "todo!(\"new body\")", "").unwrap();
        assert!(parses(&out), "replacement must stay valid Rust:\n{out}");
        assert!(out.contains("todo!(\"new body\")"));
        // The old body is gone and no second `greet` appeared.
        assert!(!out.contains("Ok(())"));
        assert_eq!(out.matches("fn greet").count(), 1);
    }

    #[test]
    fn views_an_implemented_handler() {
        let view = handler_source(&sample_model(), IMPL, "greet").unwrap();
        assert!(view.contains("fn greet"));
        assert!(view.contains("Ok(())"));
    }

    #[test]
    fn views_an_unimplemented_operation_as_its_signature() {
        let view = handler_source(&sample_model(), IMPL, "status").unwrap();
        assert!(view.contains("fn status(&self) -> anyhow::Result<()>"));
        assert!(view.contains("falls through to the trait default"));
    }

    #[test]
    fn an_unknown_operation_is_refused() {
        assert!(matches!(
            implement(&sample_model(), IMPL, "nope", "Ok(())", "").unwrap_err(),
            ImplementError::UnknownOperation(_)
        ));
        assert!(matches!(
            handler_source(&sample_model(), IMPL, "nope").unwrap_err(),
            ImplementError::UnknownOperation(_)
        ));
    }

    /// Two adapters of the `store` port's trait, one method authored in each.
    const ADAPTERS: &str = "#[async_trait::async_trait]\nimpl Store for FsStore {\n    async fn read(&self) -> anyhow::Result<String> {\n        Ok(String::new())\n    }\n}\n\n#[async_trait::async_trait]\nimpl Store for NoopStore {\n    async fn read(&self) -> anyhow::Result<String> {\n        Ok(String::new())\n    }\n}\n";

    /// The sample model with two methods on its `store` port.
    fn port_model() -> Model {
        let mut model = sample_model();
        let port = &mut model.services[0].outbound[0];
        port.methods.push(crate::model::Method {
            gated: false,
            name: "read".to_string(),
            summary: "Read.".to_string(),
            request: "Empty".to_string(),
            response: "String".to_string(),
        });
        port.methods.push(crate::model::Method {
            gated: false,
            name: "write".to_string(),
            summary: "Write.".to_string(),
            request: "String".to_string(),
            response: "Empty".to_string(),
        });
        model
    }

    #[test]
    fn an_adapter_method_replaces_in_the_named_adapter() {
        let out = implement_adapter(
            &port_model(),
            ADAPTERS,
            "store",
            "read",
            Some("NoopStore"),
            "        todo!(\"noop\")",
            "crate::generated::",
        )
        .unwrap();
        assert!(parses(&out), "{out}");
        // The named adapter changed and the other kept its body.
        let fs_at = out.find("impl Store for FsStore").unwrap();
        let noop_at = out.find("impl Store for NoopStore").unwrap();
        assert!(out[noop_at..].contains("todo!(\"noop\")"));
        assert!(out[fs_at..noop_at].contains("Ok(String::new())"));
    }

    #[test]
    fn an_unauthored_adapter_method_is_inserted_with_its_signature() {
        let out = implement_adapter(
            &port_model(),
            ADAPTERS,
            "store",
            "write",
            Some("FsStore"),
            "        Ok(())",
            "crate::generated::",
        )
        .unwrap();
        assert!(parses(&out), "{out}");
        assert!(out.contains("async fn write(&self, request: &str) -> anyhow::Result<()>"));
    }

    #[test]
    fn two_adapters_need_a_name() {
        let error = implement_adapter(
            &port_model(),
            ADAPTERS,
            "store",
            "read",
            None,
            "        Ok(String::new())",
            "",
        )
        .unwrap_err();
        assert!(matches!(error, ImplementError::AmbiguousAdapter { .. }));
        assert!(error.to_string().contains("FsStore, NoopStore"), "{error}");
    }

    #[test]
    fn adapter_lookups_are_refused_with_the_reason() {
        let model = port_model();
        assert!(matches!(
            implement_adapter(&model, ADAPTERS, "nope", "read", None, "", "").unwrap_err(),
            ImplementError::UnknownPort(_)
        ));
        assert!(matches!(
            implement_adapter(&model, ADAPTERS, "store", "nope", None, "", "").unwrap_err(),
            ImplementError::UnknownPortMethod { .. }
        ));
        assert!(matches!(
            implement_adapter(&model, ADAPTERS, "store", "read", Some("Ghost"), "", "")
                .unwrap_err(),
            ImplementError::UnknownAdapter { .. }
        ));
    }

    #[test]
    fn views_an_unauthored_adapter_method_as_its_signature() {
        let view =
            adapter_source(&port_model(), ADAPTERS, "store", "write", Some("FsStore")).unwrap();
        assert!(view.contains("async fn write(&self, request: &str)"));
        assert!(view.contains("falls through to the trait default"));
    }

    #[test]
    fn a_missing_impl_block_is_reported() {
        let error =
            implement(&sample_model(), "// no impl here\n", "status", "Ok(())", "").unwrap_err();
        assert!(matches!(error, ImplementError::NoImplBlock(_)));
    }
}
