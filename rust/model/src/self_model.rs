// @generated projection of the model — the fixed point.
// `theseus generate` and `patch` reproject this file. Edit its structure
// freely (the compiler reads it), and it is kept in canonical form.
//! Theseus's model of itself: the fixed point that describes the very tool
//! that holds it, projected back to its builder form.

use theseus_modeling::{Model, Port, Service, Transport};
pub fn theseus_model() -> Model {
    Model::new("Theseus")
        .crate_node("theseus-kernel", "kernel", 0, &[])
        .crate_node("theseus-modeling", "modeling", 1, &["theseus-kernel"])
        .crate_node("theseus-model", "model", 2, &["theseus-modeling"])
        .crate_node("theseus-calculator", "calculator", 1, &[])
        .crate_node(
            "theseus-cli",
            "cli",
            3,
            &["theseus-model", "theseus-modeling", "theseus-calculator"],
        )
        .struct_type(
            "GeneratedFile",
            &[
                ("path", "String", "Workspace-relative path of the file."),
                ("contents", "String", "The file's contents."),
            ],
        )
        .struct_type(
            "QueryRequest",
            &[
                (
                    "find",
                    "Option<String>",
                    "Filter handles by a substring of handle, name, kind, or summary.",
                ),
                ("node", "Option<String>", "Narrow to one element by its exact handle."),
                (
                    "kind",
                    "Option<String>",
                    "Keep only handles of this element kind (operation, type, or port).",
                ),
            ],
        )
        .struct_type(
            "PatchRequest",
            &[
                (
                    "verb",
                    "Option<String>",
                    "The verb for a single edit: add, remove, rename, or set. Omit when using `edit`.",
                ),
                (
                    "target",
                    "Option<String>",
                    "Handle of the parent or node for a single edit. Omit when using `edit`.",
                ),
                (
                    "kind",
                    "Option<String>",
                    "Node kind to add: operation, type, port, method, field, or variant.",
                ),
                ("name", "Option<String>", "Name of the node to add."),
                ("to", "Option<String>", "New name, for rename."),
                (
                    "set",
                    "Vec<String>",
                    "A `key=value` scalar assignment, repeatable, for add and set.",
                ),
                (
                    "expect_model_hash",
                    "String",
                    "The model hash the edit was computed against.",
                ),
                ("write", "bool", "Apply the edit by reprojecting the model."),
                (
                    "edit",
                    "Vec<String>",
                    "A pipe-separated edit `verb|target|key=value...`, repeatable, applied in order under one hash check.",
                ),
            ],
        )
        .foreign_type("ModelDocument", "String")
        .foreign_type("VerifyReport", "theseus_modeling::VerifyReport")
        .foreign_type("GeneratedFiles", "Vec<theseus_modeling::GeneratedFile>")
        .foreign_type("QueryResult", "theseus_modeling::QueryOutcome")
        .foreign_type("PatchResult", "theseus_modeling::PatchOutcome")
        .foreign_type("CoverageReport", "theseus_modeling::CoverageReport")
        .foreign_type("ImplementResult", "String")
        .struct_type(
            "ImplementRequest",
            &[
                ("method", "String", "Name of the operation to implement."),
                ("body", "Option<String>", "The handler body to splice into the impl."),
                (
                    "expect_model_hash",
                    "String",
                    "The model hash the edit was computed against.",
                ),
                (
                    "body_file",
                    "Option<String>",
                    "Read the body from this file, or from stdin when `-`. Overrides body.",
                ),
            ],
        )
        .foreign_type("HandlerSource", "String")
        .struct_type(
            "ShowRequest",
            &[("method", "String", "Name of the operation whose handler to show.")],
        )
        .struct_type(
            "Operands",
            &[("a", "f64", "Left operand."), ("b", "f64", "Right operand.")],
        )
        .foreign_type("CalcResult", "String")
        .struct_type(
            "CalcRequest",
            &[
                ("op", "String", "The operator: add, subtract, multiply, or divide."),
                ("a", "f64", "Left operand."),
                ("b", "f64", "Right operand."),
            ],
        )
        .service(
            Service::new("Theseus", Transport::Cli)
                .crate_name("theseus-cli")
                .operation(
                    "model",
                    "Print Theseus's model of itself as JSON.",
                    "Empty",
                    "ModelDocument",
                )
                .operation(
                    "verify",
                    "Check that the workspace conforms to its self-model.",
                    "Empty",
                    "VerifyReport",
                )
                .operation(
                    "generate",
                    "Regenerate model-derived code from the self-model.",
                    "Empty",
                    "GeneratedFiles",
                )
                .operation(
                    "query",
                    "Return a stable handle and model hash for a model element.",
                    "QueryRequest",
                    "QueryResult",
                )
                .operation(
                    "patch",
                    "Propose a hash-checked edit to the model.",
                    "PatchRequest",
                    "PatchResult",
                )
                .operation(
                    "coverage",
                    "Report which operations have an authored handler.",
                    "Empty",
                    "CoverageReport",
                )
                .operation(
                    "implement",
                    "Splice an authored handler for an unimplemented operation.",
                    "ImplementRequest",
                    "ImplementResult",
                )
                .operation(
                    "show",
                    "Show an operation's current handler source.",
                    "ShowRequest",
                    "HandlerSource",
                )
                .operation(
                    "calc",
                    "Evaluate an arithmetic expression through the calculator service.",
                    "CalcRequest",
                    "CalcResult",
                )
                .operation(
                    "scaffold",
                    "Write the skeleton of each library service crate that is missing it.",
                    "Empty",
                    "GeneratedFiles",
                )
                .port(
                    Port::new("workspace", "Writes generated files into the workspace.")
                        .method(
                            "write_file",
                            "Write one generated file to disk.",
                            "GeneratedFile",
                            "Empty",
                        ),
                )
                .port(
                    Port::new(
                            "calculator",
                            "Evaluates arithmetic through the calculator service.",
                        )
                        .targeting("Calculator"),
                ),
        )
        .service(
            Service::new("Calculator", Transport::InProcess)
                .crate_name("theseus-calculator")
                .operation("add", "Add the operands.", "Operands", "CalcResult")
                .operation(
                    "subtract",
                    "Subtract the operands.",
                    "Operands",
                    "CalcResult",
                )
                .operation(
                    "multiply",
                    "Multiply the operands.",
                    "Operands",
                    "CalcResult",
                )
                .operation("divide", "Divide the operands.", "Operands", "CalcResult"),
        )
}
