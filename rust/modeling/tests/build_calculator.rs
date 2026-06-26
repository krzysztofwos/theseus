//! An agent builds a Calculator service through the edit vocabulary.
//!
//! This drives the same [`apply_edit`] machinery the `theseus patch` command
//! delegates to: a sequence of hash-checked edits, each addressed by a handle and
//! accepted, ending in coherent rendered projections. It is the build-up an agent
//! performs to develop a service from a bare seed.

use theseus_modeling::{
    Edit, Model, Service, Transport, apply_edit, model_hash, render_cli_module, render_model_source,
};

/// A bare Calculator model: one crate, one service driven by a CLI inbound, no
/// operations yet.
fn seed() -> Model {
    Model::new("Calculator")
        .crate_node("calculator", "calculator", 0, &[])
        .service(Service::new("Calculator").crate_name("calculator"))
        .inbound("calculator", Transport::Cli, "Calculator", "calculator")
}

/// Apply an edit against the current hash, asserting acceptance, and return the
/// edited model — the way an agent threads each accepted edit into the next.
fn accept(model: Model, edit: Edit) -> Model {
    let hash = model_hash(&model);
    let (outcome, next) = apply_edit(&model, &edit, &hash);
    assert!(outcome.ok, "edit refused: {:?}", outcome.diagnostics);
    next.expect("an accepted edit yields a model")
}

/// Add a node under a parent handle, with optional `key=value` attributes.
fn add(parent: &str, kind: &str, name: &str, attrs: &[(&str, &str)]) -> Edit {
    Edit::Add {
        parent: parent.to_string(),
        kind: kind.to_string(),
        name: name.to_string(),
        attrs: attrs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

/// One binary arithmetic operation over the shared `Operands` request.
fn binary_op(name: &str) -> Edit {
    add(
        "model:calculator",
        "operation",
        name,
        &[("summary", "Compute."), ("request", "Operands")],
    )
}

#[test]
fn an_agent_builds_and_amends_a_calculator() {
    let mut model = seed();

    // Register the request type the operations share, then grow its second field.
    model = accept(
        model,
        add(
            "model:calculator",
            "type",
            "Operands",
            &[("shape", "struct:a=String")],
        ),
    );
    model = accept(
        model,
        add(
            "type:calculator:Operands",
            "field",
            "b",
            &[("ty", "String"), ("doc", "Right operand.")],
        ),
    );

    // Add four operations, each referencing the shared request type.
    for name in ["add", "subtract", "multiply", "divide"] {
        model = accept(model, binary_op(name));
    }
    assert_eq!(model.operations().len(), 4);

    // Amend: rename one operation and remove another.
    model = accept(
        model,
        Edit::Rename {
            target: "op:calculator:subtract".to_string(),
            to: "sub".to_string(),
        },
    );
    model = accept(
        model,
        Edit::Remove {
            target: "op:calculator:divide".to_string(),
        },
    );
    assert!(model.operation("subtract").is_none());
    assert!(model.operation("sub").is_some());
    assert!(model.operation("divide").is_none());
    assert_eq!(model.operations().len(), 3);

    // Both projections render. Each render parses its own tokens, so a successful
    // render guarantees valid Rust, and the output names the operations.
    let cli = render_cli_module(&model);
    assert!(cli.contains("Command::new(\"add\")"));
    assert!(cli.contains("Command::new(\"sub\")"));
    assert!(!cli.contains("Command::new(\"divide\")"));
    assert!(cli.contains("pub struct Operands"));

    let source = render_model_source(&model, "// projected\n", "calculator_model");
    assert!(source.contains("pub fn calculator_model() -> Model"));
    assert!(source.contains(".operation(\"add\""));
    assert!(source.contains(".struct_type("));
    assert!(source.contains("\"Operands\""));
}

#[test]
fn a_stale_hash_is_rejected_mid_build() {
    let model = seed();
    let (outcome, next) = apply_edit(&model, &binary_op("add"), "deadbeef");
    assert!(!outcome.ok);
    assert!(next.is_none());
    assert_eq!(outcome.diagnostics[0].code, "PATCH001");
}

#[test]
fn a_duplicate_operation_is_rejected() {
    let op = add("model:calculator", "operation", "noop", &[]);
    let model = accept(seed(), op.clone());
    let hash = model_hash(&model);
    let (outcome, _) = apply_edit(&model, &op, &hash);
    assert!(!outcome.ok);
    assert_eq!(outcome.diagnostics[0].code, "PATCH007");
}
