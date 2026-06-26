//! Handler bodies: read and write the authored operation handlers.
//!
//! [`handler_source`] returns an operation's current handler text. [`implement`]
//! renders the handler signature from the model, wraps a caller body, and writes
//! the method into the source implementing the service trait — inserting it when
//! the operation is on its `unimplemented` default, replacing it in place when a
//! handler is already there. The signature carries absolute paths, so the written
//! method needs no imports. Both locate a method by its exact source span, so the
//! rest of the authored file is preserved byte for byte.

use std::ops::Range;

use syn::spanned::Spanned;

use crate::{
    codegen::handler_signature,
    coverage::service_trait_name,
    model::{Model, Operation, Service},
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

/// The byte range of the `op_name` method in the `impl <trait_name> for …` block,
/// or `None` when the block has no such method.
fn method_range(
    source: &str,
    trait_name: &str,
    method: &str,
) -> Result<Option<Range<usize>>, ImplementError> {
    let file = syn::parse_file(source).map_err(|error| ImplementError::Parse(error.to_string()))?;
    for item in &file.items {
        let syn::Item::Impl(block) = item else { continue };
        let names_the_trait = block
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .is_some_and(|segment| segment.ident == trait_name);
        if !names_the_trait {
            continue;
        }
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

/// Insert `method` just inside the `impl <trait_name> for …` block.
fn splice_after_header(
    source: &str,
    trait_name: &str,
    method: &str,
) -> Result<String, ImplementError> {
    let header = format!("impl {trait_name} for");
    let header_at = source
        .find(&header)
        .ok_or_else(|| ImplementError::NoImplBlock(trait_name.to_string()))?;
    let brace_at = source[header_at..]
        .find('{')
        .map(|offset| header_at + offset + 1)
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

    const IMPL: &str =
        "impl SampleService for Ctx<'_> {\n    fn greet(&self) -> anyhow::Result<()> {\n        Ok(())\n    }\n}\n";

    /// The rendered source must remain valid Rust.
    fn parses(source: &str) -> bool {
        syn::parse_file(source).is_ok()
    }

    #[test]
    fn inserts_a_handler_for_an_unimplemented_operation() {
        let out = implement(&sample_model(), IMPL, "status", "Ok(())", "crate::generated::").unwrap();
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

    #[test]
    fn a_missing_impl_block_is_reported() {
        let error = implement(&sample_model(), "// no impl here\n", "status", "Ok(())", "").unwrap_err();
        assert!(matches!(error, ImplementError::NoImplBlock(_)));
    }
}
