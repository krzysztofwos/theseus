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
        .crate_node("theseus-cli", "cli", 3, &["theseus-model", "theseus-modeling"])
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
                ("verb", "String", "The verb: add, remove, rename, or set."),
                (
                    "target",
                    "String",
                    "Handle of the parent, for add; of the node, for remove, rename, and set.",
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
            ],
        )
        .foreign_type("ModelDocument", "String")
        .foreign_type("VerifyReport", "theseus_modeling::VerifyReport")
        .foreign_type("GeneratedFiles", "Vec<theseus_modeling::GeneratedFile>")
        .foreign_type("QueryResult", "theseus_modeling::QueryOutcome")
        .foreign_type("PatchResult", "theseus_modeling::PatchOutcome")
        .foreign_type("CoverageReport", "theseus_modeling::CoverageReport")
        .service(
            Service::new("Theseus", Transport::Cli)
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
                .port(
                    Port::new("workspace", "Writes generated files into the workspace.")
                        .method(
                            "write_file",
                            "Write one generated file to disk.",
                            "GeneratedFile",
                            "Empty",
                        ),
                ),
        )
}
