//! Implementation coverage: which modeled operations have authored handlers.
//!
//! An adopter implements the generated service trait. An operation it has not
//! overridden falls through to the trait's `unimplemented` default. This reads
//! the authored impl source and reports which operations still lack a handler, so
//! [`verify`](crate::verify) can gate on it and an agent can work the list.

use std::collections::BTreeSet;

use serde::Serialize;
use syn::{ImplItem, Item};

use crate::{codegen::pascal_case, model::Model};

/// The implementation status of a model's operations.
#[derive(Debug, Clone, Serialize)]
pub struct CoverageReport {
    /// Total operations in the model.
    pub total: usize,
    /// Operations with an authored handler.
    pub implemented: usize,
    /// Operations still on the `unimplemented` default.
    pub unimplemented: Vec<OperationGap>,
}

/// One operation lacking an authored handler, with the signature to fill.
#[derive(Debug, Clone, Serialize)]
pub struct OperationGap {
    pub name: String,
    pub summary: String,
    pub request: String,
    pub response: String,
}

/// Why coverage could not be computed.
#[derive(Debug, thiserror::Error)]
pub enum CoverageError {
    /// The authored impl source did not parse as Rust.
    #[error("parsing the authored impl: {0}")]
    Parse(String),
}

/// Report which of the model's operations have an authored handler in
/// `impl_source` — the source of the file implementing the service trait.
pub fn coverage(model: &Model, impl_source: &str) -> Result<CoverageReport, CoverageError> {
    let trait_name = service_trait_name(model);
    let implemented = implemented_methods(impl_source, &trait_name)?;

    let operations = model.operations();
    let unimplemented: Vec<OperationGap> = operations
        .iter()
        .filter(|op| !implemented.contains(&op.name))
        .map(|op| OperationGap {
            name: op.name.clone(),
            summary: op.summary.clone(),
            request: op.request.clone(),
            response: op.response.clone(),
        })
        .collect();

    Ok(CoverageReport {
        total: operations.len(),
        implemented: operations.len() - unimplemented.len(),
        unimplemented,
    })
}

/// The trait name codegen emits for the model's first service.
fn service_trait_name(model: &Model) -> String {
    let service = model.services.first().map_or("", |s| s.name.as_str());
    format!("{}Service", pascal_case(service))
}

/// The method names of the `impl <trait_name> for …` block in the source.
fn implemented_methods(source: &str, trait_name: &str) -> Result<BTreeSet<String>, CoverageError> {
    let file = syn::parse_file(source).map_err(|error| CoverageError::Parse(error.to_string()))?;
    let mut names = BTreeSet::new();
    for item in &file.items {
        let Item::Impl(block) = item else { continue };
        let names_the_trait = block
            .trait_
            .as_ref()
            .and_then(|(_, path, _)| path.segments.last())
            .is_some_and(|segment| segment.ident == trait_name);
        if !names_the_trait {
            continue;
        }
        for impl_item in &block.items {
            if let ImplItem::Fn(method) = impl_item {
                names.insert(method.sig.ident.to_string());
            }
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    #[test]
    fn reports_an_unimplemented_operation() {
        // The sample model has operations `greet` and `status`.
        let source = "impl SampleService for Ctx { fn greet(&self) {} }";
        let report = coverage(&sample_model(), source).unwrap();
        assert_eq!(report.total, 2);
        assert_eq!(report.implemented, 1);
        assert_eq!(report.unimplemented.len(), 1);
        assert_eq!(report.unimplemented[0].name, "status");
    }

    #[test]
    fn a_fully_implemented_impl_leaves_no_gaps() {
        let source = "impl SampleService for Ctx { fn greet(&self) {} fn status(&self) {} }";
        let report = coverage(&sample_model(), source).unwrap();
        assert!(report.unimplemented.is_empty());
        assert_eq!(report.implemented, 2);
    }

    #[test]
    fn methods_outside_the_service_trait_are_ignored() {
        let source = "impl Other for Ctx { fn greet(&self) {} }";
        let report = coverage(&sample_model(), source).unwrap();
        assert_eq!(report.implemented, 0);
    }

    #[test]
    fn unparseable_source_is_an_error() {
        assert!(matches!(
            coverage(&sample_model(), "fn (").unwrap_err(),
            CoverageError::Parse(_)
        ));
    }
}
