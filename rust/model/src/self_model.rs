// @generated projection of the model — the fixed point.
// `theseus generate` and `patch` reproject this file. Edit its structure
// freely (the compiler reads it), and it is kept in canonical form.
//! Theseus's model of itself: the fixed point that describes the very tool
//! that holds it, projected back to its builder form.

use theseus_modeling::{Model, Port, Service, Transport, Variant};

pub fn theseus_model() -> Model {
    Model::new("Theseus")
        .crate_node("theseus-kernel", "kernel", 0, &[])
        .crate_node("theseus-modeling", "modeling", 1, &["theseus-kernel"])
        .crate_node("theseus-model", "model", 2, &["theseus-modeling"])
        .crate_node("theseus-calculator", "calculator", 1, &[])
        .crate_node(
            "theseus-calculator-cli",
            "calculator-cli",
            2,
            &["theseus-calculator"],
        )
        .crate_node(
            "theseus-calculator-grpc",
            "calculator-grpc",
            2,
            &["theseus-calculator"],
        )
        .crate_node(
            "theseus-calculator-grpc-client",
            "calculator-grpc-client",
            2,
            &["theseus-calculator"],
        )
        .crate_node(
            "theseus",
            "theseus",
            3,
            &["theseus-model", "theseus-modeling", "theseus-calculator"],
        )
        .crate_node(
            "theseus-cli",
            "cli",
            5,
            &[
                "theseus",
                "theseus-model",
                "theseus-modeling",
                "theseus-calculator",
                "theseus-http-client",
                "theseus-calculator-grpc-client",
            ],
        )
        .crate_node(
            "theseus-agent",
            "agent",
            4,
            &["theseus", "theseus-model", "theseus-modeling", "theseus-calculator"],
        )
        .crate_node(
            "theseus-mcp",
            "mcp",
            4,
            &["theseus", "theseus-model", "theseus-modeling", "theseus-calculator"],
        )
        .crate_node("theseus-http", "http", 4, &["theseus"])
        .crate_node("theseus-grpc", "grpc", 4, &["theseus", "theseus-modeling"])
        .crate_node(
            "theseus-http-client",
            "http-client",
            4,
            &["theseus", "theseus-modeling"],
        )
        .crate_node(
            "theseus-grpc-client",
            "grpc-client",
            4,
            &["theseus", "theseus-modeling"],
        )
        .foreign_type("GeneratedFile", "theseus_modeling::GeneratedFile")
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
        .foreign_enum(
            "Edit",
            "theseus_modeling::Edit",
            &[
                Variant::data(
                    "add",
                    &[
                        (
                            "parent",
                            "String",
                            "Handle the new node attaches to; the model root for a top-level node.",
                        ),
                        (
                            "kind",
                            "String",
                            "Node kind: operation, type, port, method, field, or variant.",
                        ),
                        ("name", "String", "Name of the new node."),
                        (
                            "attrs",
                            "Option<BTreeMap<String, String>>",
                            "Scalar attributes, e.g. `shape`, `ty`, or `summary`.",
                        ),
                    ],
                ),
                Variant::data(
                    "remove",
                    &[("target", "String", "Handle of the node to remove.")],
                ),
                Variant::data(
                    "rename",
                    &[
                        ("target", "String", "Handle of the node to rename."),
                        ("to", "String", "The new name."),
                    ],
                ),
                Variant::data(
                    "set",
                    &[
                        ("target", "String", "Handle of the node to edit."),
                        (
                            "attrs",
                            "BTreeMap<String, String>",
                            "Scalar attributes to set.",
                        ),
                    ],
                ),
            ],
        )
        .struct_type(
            "PatchRequest",
            &[
                (
                    "edit",
                    "Vec<Edit>",
                    "The edits to apply in order, each a verb over a handle from `query`.",
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
        .foreign_type("ImplementResult", "String")
        .struct_type(
            "ImplementRequest",
            &[
                (
                    "method",
                    "String",
                    "Name of the operation — or, with `port`, the port method — to implement.",
                ),
                ("body", "String", "The handler body to splice into the impl."),
                (
                    "port",
                    "Option<String>",
                    "Name of the port whose adapter method to implement.",
                ),
                (
                    "adapter",
                    "Option<String>",
                    "The adapter type to target when the file holds more than one.",
                ),
            ],
        )
        .foreign_type("HandlerSource", "String")
        .struct_type(
            "ShowRequest",
            &[
                (
                    "method",
                    "String",
                    "Name of the operation — or, with `port`, the port method — to show.",
                ),
                (
                    "port",
                    "Option<String>",
                    "Name of the port whose adapter method to show.",
                ),
                (
                    "adapter",
                    "Option<String>",
                    "The adapter type to target when the file holds more than one.",
                ),
            ],
        )
        .foreign_type("CheckReport", "String")
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
            Service::new("Theseus")
                .crate_name("theseus")
                .operation(
                    "model",
                    "Print Theseus's model of itself as JSON.",
                    "Empty",
                    "ModelDocument",
                )
                .tool("Return Theseus's model of itself as JSON.")
                .operation(
                    "verify",
                    "Check that the workspace conforms to its self-model.",
                    "Empty",
                    "VerifyReport",
                )
                .tool("Check that the workspace conforms to the model.")
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
                .tool(
                    "List model element handles, optionally filtered by `find` (a substring), `node` (an exact handle), or `kind`.",
                )
                .operation(
                    "patch",
                    "Propose an edit to the model.",
                    "PatchRequest",
                    "PatchResult",
                )
                .tool(
                    "Edit the model. Each edit names a handle from `query`; a top-level node attaches to the model root, `model:<model>`. An operation's `tool` attribute is its agent tool description — an operation carrying one joins this tool catalog at the next rebuild. `write` true reprojects to disk.",
                )
                .operation(
                    "coverage",
                    "Report which operations have an authored handler.",
                    "Empty",
                    "CoverageReport",
                )
                .tool("Report which operations have no authored handler.")
                .operation(
                    "implement",
                    "Splice an authored handler or adapter method and compile-check it.",
                    "ImplementRequest",
                    "ImplementResult",
                )
                .tool(
                    "Write a handler for an operation into the service impl, so a newly-added operation stops being unimplemented. `method` is the operation name. `body` is the Rust handler body — the statements inside the generated `fn <method>(&self, request: <Request>) -> anyhow::Result<<Response>>`, which the splice wraps for you. With `port`, `method` names one of that port's methods instead, and the body lands in the port's adapter impl in the crate's authored adapters file — `adapter` picks the implementing type when the file holds more than one. The write is followed by a compile check, and the result carries its outcome — on a failure, fix the body and implement again, which replaces the method in place. Author it after `patch` adds the operation or method (use `show` to read the signature), then `verify`. Example: `{ \"method\": \"greet\", \"body\": \"Ok(\\\"hello\\\".to_string())\" }`.",
                )
                .operation(
                    "show",
                    "Show an operation's current handler source.",
                    "ShowRequest",
                    "HandlerSource",
                )
                .tool(
                    "Show the current authored handler source for an operation. `method` is an operation name from `query` (kind `operation`). With `port`, `method` names one of that port's methods and the adapter method shows instead — `adapter` picks the implementing type when the file holds more than one. For a method with no authored source yet, it returns the generated signature, so you can read the request and response types before authoring. Example: `{ \"method\": \"verify\" }`.",
                )
                .operation(
                    "check",
                    "Compile-check the workspace and report the outcome.",
                    "Empty",
                    "CheckReport",
                )
                .tool(
                    "Compile-check the workspace and report the outcome. `implement` runs it after each write on its own. Call it directly after a `patch` that writes, or to prove the tree compiles before a rebuild.",
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
                )
                .port(
                    Port::new(
                            "toolchain",
                            "Compile-checks the workspace and reports the outcome.",
                        )
                        .method(
                            "check",
                            "Compile-check the workspace and report the outcome.",
                            "Empty",
                            "CheckReport",
                        ),
                ),
        )
        .service(
            Service::new("Calculator")
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
        .inbound("theseus", Transport::Cli, "Theseus", "theseus-cli")
        .inbound("agent", Transport::Agent, "Theseus", "theseus-agent")
        .inbound("mcp", Transport::Mcp, "Theseus", "theseus-mcp")
        .inbound("http", Transport::Http, "Theseus", "theseus-http")
        .inbound("grpc", Transport::Grpc, "Theseus", "theseus-grpc")
        .inbound("calculator", Transport::Cli, "Calculator", "theseus-calculator-cli")
        .inbound(
            "calculator-grpc",
            Transport::Grpc,
            "Calculator",
            "theseus-calculator-grpc",
        )
        .client("http-client", Transport::Http, "Theseus", "theseus-http-client")
        .client("grpc-client", Transport::Grpc, "Theseus", "theseus-grpc-client")
        .client(
            "calculator-grpc-client",
            Transport::Grpc,
            "Calculator",
            "theseus-calculator-grpc-client",
        )
}
