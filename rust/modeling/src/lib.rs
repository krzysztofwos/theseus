//! General-purpose architecture modeling.
//!
//! Holds the [`Model`] vocabulary and the engine that operates on any model:
//! stable [`model_hash`]ing, [`verify`]ication, code generation, and the agent
//! query/patch surface. An adopter crate supplies a concrete model.

mod codegen;
mod dsl;
mod hash;
mod model;
mod verify;

#[cfg(test)]
mod test_support;

pub use codegen::{GeneratedFile, render_cli_module};
pub use hash::model_hash;
pub use model::{
    CrateNode, Field, Method, Model, Operation, Port, Service, Transport, TypeDef, TypeShape,
};
pub use verify::{Check, VerifyReport, verify};

/// Render a model as a self-describing JSON document: the model plus its hash.
///
/// This is the read side of self-reference: an adopter prints it to describe
/// itself.
pub fn describe(model: &Model) -> String {
    let document = serde_json::json!({
        "model": model,
        "model_hash": model_hash(model),
    });
    serde_json::to_string_pretty(&document).expect("model document always serializes")
}
