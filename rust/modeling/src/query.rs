//! The read side of the agent protocol.
//!
//! An agent calls `query` to discover stable handles for the model's elements
//! and the current model hash. A handle names an element independently of its
//! position, and the hash anchors a subsequent [`patch`](crate::patch) so a
//! stale edit is rejected.

use serde::Serialize;

use crate::{
    hash::model_hash,
    model::{Field, Model, TypeDef, TypeShape},
    path::Target,
};

/// A stable reference to one model element.
#[derive(Debug, Clone, Serialize)]
pub struct Handle {
    /// Position-independent handle, e.g. `op:theseus:verify`.
    pub handle: String,
    /// The kind of element this handle names.
    pub kind: String,
    /// The element's name.
    pub name: String,
    /// The element's one-line summary.
    pub summary: String,
}

/// The result of a query: the model hash plus the matching handles.
#[derive(Debug, Clone, Serialize)]
pub struct QueryOutcome {
    pub model_hash: String,
    pub handles: Vec<Handle>,
}

/// Why a query failed.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The requested node handle is not in the model.
    #[error("no node with handle `{0}`")]
    UnknownNode(String),
}

/// One handle for an addressed node, its kind and summary read off the address.
fn handle(model: &Model, target: Target, name: String, summary: String) -> Handle {
    Handle {
        handle: target.render(model),
        kind: target.kind_word().to_string(),
        name,
        summary,
    }
}

/// A short description of a type's shape, e.g. `newtype String` or `struct`.
fn type_summary(def: &TypeDef) -> String {
    match &def.shape {
        TypeShape::Struct(_) => "struct".to_string(),
        TypeShape::Newtype(inner) => format!("newtype {inner}"),
        TypeShape::Enum(_) => "enum".to_string(),
        TypeShape::Foreign(path) => format!("foreign {path}"),
    }
}

/// A field's summary is its type label.
fn field_summary(field: &Field) -> String {
    field.ty.clone()
}

/// Every handle the model exposes: each operation, type, port, and the fields,
/// variants, and methods nested within them.
fn all_handles(model: &Model) -> Vec<Handle> {
    let mut handles = Vec::new();

    for node in &model.crates {
        let target = Target::Crate(node.name.clone());
        handles.push(handle(
            model,
            target,
            node.name.clone(),
            format!("crate in {} at layer {}", node.dir, node.layer),
        ));
        for dep in &node.depends_on {
            let target = Target::Dep {
                crate_name: node.name.clone(),
                dep: dep.clone(),
            };
            handles.push(handle(model, target, dep.clone(), String::new()));
        }
    }

    for service in &model.services {
        let target = Target::Service(service.name.clone());
        handles.push(handle(
            model,
            target,
            service.name.clone(),
            format!("service in {}", service.crate_name),
        ));
    }

    for inbound in &model.inbounds {
        let target = Target::Inbound(inbound.name.clone());
        handles.push(handle(
            model,
            target,
            inbound.name.clone(),
            format!(
                "{:?} inbound driving {}",
                inbound.transport, inbound.service
            ),
        ));
    }

    for op in model.operations() {
        let target = Target::Operation(op.name.clone());
        handles.push(handle(model, target, op.name.clone(), op.summary.clone()));
    }

    for def in &model.types {
        let target = Target::Type(def.name.clone());
        handles.push(handle(model, target, def.name.clone(), type_summary(def)));
        match &def.shape {
            TypeShape::Struct(fields) => {
                for field in fields {
                    let target = Target::Field {
                        ty: def.name.clone(),
                        name: field.name.clone(),
                    };
                    handles.push(handle(
                        model,
                        target,
                        field.name.clone(),
                        field_summary(field),
                    ));
                }
            }
            TypeShape::Enum(variants) => {
                for variant in variants {
                    let target = Target::Variant {
                        ty: def.name.clone(),
                        name: variant.clone(),
                    };
                    handles.push(handle(model, target, variant.clone(), String::new()));
                }
            }
            TypeShape::Newtype(_) | TypeShape::Foreign(_) => {}
        }
    }

    for port in model
        .services
        .iter()
        .flat_map(|service| service.outbound.iter())
    {
        let target = Target::Port(port.name.clone());
        handles.push(handle(
            model,
            target,
            port.name.clone(),
            port.summary.clone(),
        ));
        for method in &port.methods {
            let target = Target::Method {
                port: port.name.clone(),
                name: method.name.clone(),
            };
            handles.push(handle(
                model,
                target,
                method.name.clone(),
                method.summary.clone(),
            ));
        }
    }

    handles
}

/// Whether a handle matches `text` in its handle, name, kind, or summary,
/// comparing without regard to case.
fn matches(handle: &Handle, text: &str) -> bool {
    let needle = text.to_lowercase();
    [&handle.handle, &handle.name, &handle.kind, &handle.summary]
        .iter()
        .any(|field| field.to_lowercase().contains(&needle))
}

/// Query the model for handles, optionally searched and narrowed.
///
/// With no filters this lists every operation, type, and port. `find` keeps the
/// handles whose handle, name, kind, or summary contains its text. `node` then
/// keeps the single handle whose `handle` equals its text exactly, applied after
/// `find` so an exact handle wins.
///
/// Returns an error only when `node` names a handle absent from the result, so
/// an agent can tell "no such node" from "here it is".
pub fn query(
    model: &Model,
    find: Option<&str>,
    node: Option<&str>,
) -> Result<QueryOutcome, QueryError> {
    let mut handles: Vec<Handle> = all_handles(model)
        .into_iter()
        .filter(|handle| find.is_none_or(|text| matches(handle, text)))
        .collect();

    if let Some(target) = node {
        handles.retain(|handle| handle.handle == target);
        if handles.is_empty() {
            return Err(QueryError::UnknownNode(target.to_string()));
        }
    }

    Ok(QueryOutcome {
        model_hash: model_hash(model),
        handles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{TypeDef, TypeShape},
        test_support::sample_model,
    };

    /// The sample model with a couple of types added so a query exercises all
    /// three handle kinds.
    fn model_with_types() -> Model {
        let mut model = sample_model();
        model.types = vec![
            TypeDef {
                name: "Empty".to_string(),
                shape: TypeShape::Struct(vec![]),
            },
            TypeDef {
                name: "Name".to_string(),
                shape: TypeShape::Newtype("String".to_string()),
            },
        ];
        model
    }

    #[test]
    fn query_all_lists_operations_types_and_ports() {
        let model = model_with_types();
        let outcome = query(&model, None, None).unwrap();

        let kinds: Vec<&str> = outcome.handles.iter().map(|h| h.kind.as_str()).collect();
        assert!(kinds.contains(&"operation"));
        assert!(kinds.contains(&"type"));
        assert!(kinds.contains(&"port"));

        let expected = model.crates.len()
            + model
                .crates
                .iter()
                .map(|c| c.depends_on.len())
                .sum::<usize>()
            + model.services.len()
            + model.inbounds.len()
            + model.operations().len()
            + model.types.len()
            + model
                .services
                .iter()
                .map(|s| s.outbound.len())
                .sum::<usize>();
        assert_eq!(outcome.handles.len(), expected);
        assert_eq!(outcome.model_hash, model_hash(&model));
    }

    #[test]
    fn query_emits_type_and_port_handles() {
        let model = model_with_types();
        let outcome = query(&model, None, None).unwrap();
        let handles: Vec<&str> = outcome.handles.iter().map(|h| h.handle.as_str()).collect();
        assert!(handles.contains(&"op:sample:greet"));
        assert!(handles.contains(&"type:sample:Name"));
        assert!(handles.contains(&"port:sample:store"));

        let name_type = outcome
            .handles
            .iter()
            .find(|h| h.handle == "type:sample:Name")
            .unwrap();
        assert_eq!(name_type.summary, "newtype String");
    }

    #[test]
    fn find_narrows_to_matching_handles() {
        let model = model_with_types();
        let outcome = query(&model, Some("greet"), None).unwrap();
        assert_eq!(outcome.handles.len(), 1);
        assert_eq!(outcome.handles[0].handle, "op:sample:greet");
    }

    #[test]
    fn find_matches_case_insensitively_across_fields() {
        let model = model_with_types();
        let outcome = query(&model, Some("NEWTYPE"), None).unwrap();
        assert_eq!(outcome.handles.len(), 1);
        assert_eq!(outcome.handles[0].handle, "type:sample:Name");
    }

    #[test]
    fn node_returns_the_single_exact_handle() {
        let model = model_with_types();
        let outcome = query(&model, None, Some("port:sample:store")).unwrap();
        assert_eq!(outcome.handles.len(), 1);
        assert_eq!(outcome.handles[0].kind, "port");
    }

    #[test]
    fn node_errors_on_an_unknown_handle() {
        let model = model_with_types();
        assert!(query(&model, None, Some("op:sample:nope")).is_err());
    }
}
