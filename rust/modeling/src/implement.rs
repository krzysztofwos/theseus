//! Filling a handler: splice an authored body for an unimplemented operation.
//!
//! [`implement`] renders an operation's handler signature from the model, wraps
//! the caller's body, and splices the method into the source implementing the
//! service trait. The signature carries absolute paths, so the inserted code
//! needs no imports. The operation must exist in the model and still be on its
//! `unimplemented` default — this fills a hole, it does not rewrite a handler.

use crate::{
    codegen::handler_signature,
    coverage::{implemented_methods, service_trait_name},
    model::Model,
};

/// Why a handler could not be spliced.
#[derive(Debug, thiserror::Error)]
pub enum ImplementError {
    /// The model has no operation by that name.
    #[error("no operation named `{0}`")]
    UnknownOperation(String),
    /// The operation already has an authored handler.
    #[error("operation `{0}` already has a handler")]
    AlreadyImplemented(String),
    /// The source has no block implementing the service trait.
    #[error("no `impl {0} for …` block found in the authored impl")]
    NoImplBlock(String),
    /// The authored impl did not parse as Rust.
    #[error("parsing the authored impl: {0}")]
    Parse(String),
}

/// Splice a handler for `op_name` into `impl_source`, returning the new source.
///
/// `request_path` prefixes the request type so the inserted method compiles
/// without imports, e.g. `crate::generated::`.
pub fn implement(
    model: &Model,
    impl_source: &str,
    op_name: &str,
    body: &str,
    request_path: &str,
) -> Result<String, ImplementError> {
    let operation = model
        .operation(op_name)
        .ok_or_else(|| ImplementError::UnknownOperation(op_name.to_string()))?;

    let trait_name = service_trait_name(model);
    let implemented = implemented_methods(impl_source, &trait_name)
        .map_err(|error| ImplementError::Parse(error.to_string()))?;
    if implemented.contains(op_name) {
        return Err(ImplementError::AlreadyImplemented(op_name.to_string()));
    }

    let signature = handler_signature(operation, model, request_path);
    let method = format!("    {signature} {{\n{body}\n    }}\n");
    splice_after_header(impl_source, &trait_name, &method)
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

    #[test]
    fn splices_a_handler_for_an_unimplemented_operation() {
        let out = implement(&sample_model(), IMPL, "status", "Ok(())", "crate::generated::").unwrap();
        assert!(out.contains("fn status(&self) -> anyhow::Result<()>"));
        // The existing handler is preserved, and the new one lands inside the block.
        assert!(out.contains("fn greet"));
        assert!(out.find("fn status").unwrap() < out.find("fn greet").unwrap());
    }

    #[test]
    fn refuses_an_already_implemented_operation() {
        let error = implement(&sample_model(), IMPL, "greet", "Ok(())", "").unwrap_err();
        assert!(matches!(error, ImplementError::AlreadyImplemented(_)));
    }

    #[test]
    fn refuses_an_unknown_operation() {
        let error = implement(&sample_model(), IMPL, "nope", "Ok(())", "").unwrap_err();
        assert!(matches!(error, ImplementError::UnknownOperation(_)));
    }

    #[test]
    fn reports_a_missing_impl_block() {
        let error = implement(&sample_model(), "// no impl here\n", "status", "Ok(())", "").unwrap_err();
        assert!(matches!(error, ImplementError::NoImplBlock(_)));
    }
}
