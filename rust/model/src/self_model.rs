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
        .crate_node("theseus-text-utils", "text-utils", 1, &[])
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
                (
                    "write",
                    "bool",
                    "When true, apply the edit and reproject the model to disk; when false, validate and preview only.",
                ),
            ],
        )
        .foreign_type("Transcript", "Vec<crate::agent::Message>")
        .foreign_type("ToolCatalog", "Vec<serde_json::Value>")
        .foreign_type("Reply", "crate::agent::Reply")
        .struct_type(
            "Turn",
            &[
                ("system", "String", "The framing handed to the model."),
                ("messages", "Transcript", "The conversation so far."),
                ("tools", "ToolCatalog", "The tool catalog the model may call."),
            ],
        )
        .foreign_type("ModelDocument", "String")
        .foreign_type("VerifyReport", "theseus_modeling::VerifyReport")
        .foreign_type("GeneratedFiles", "Vec<theseus_modeling::GeneratedFile>")
        .foreign_type("ExpectedProjection", "theseus::ExpectedFileSet")
        .foreign_type("WorkspaceMutation", "theseus::PendingMutation")
        .foreign_type("CheckpointSnapshotRequest", "theseus::CheckpointSnapshotRequest")
        .foreign_type("CheckpointStateRequest", "theseus::CheckpointStateRequest")
        .foreign_type("CheckpointSnapshot", "theseus::CheckpointSnapshot")
        .foreign_type("CheckpointRestore", "theseus::CheckpointRestore")
        .foreign_type("QueryResult", "theseus_modeling::QueryOutcome")
        .foreign_type("PatchResult", "theseus_modeling::PatchOutcome")
        .foreign_type("CoverageReport", "theseus_modeling::CoverageReport")
        .foreign_type("ImplementResult", "theseus::ImplementResult")
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
        .foreign_type("CheckReport", "theseus::CheckReport")
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
        .struct_type(
            "SnapshotRequest",
            &[("label", "String", "A short label naming the snapshot.")],
        )
        .struct_type(
            "SnapshotRef",
            &[("reference", "String", "The snapshot id, as returned by `snapshot`.")],
        )
        .struct_type(
            "SnapshotRetention",
            &[("keep", "u32", "Number of newest snapshots to retain.")],
        )
        .struct_type(
            "ReadRequest",
            &[("path", "String", "The workspace-relative file to read.")],
        )
        .struct_type(
            "SearchRequest",
            &[
                ("pattern", "String", "The text to find."),
                (
                    "path",
                    "Option<String>",
                    "A workspace-relative subtree to search. The whole workspace when omitted.",
                ),
            ],
        )
        .struct_type(
            "ListRequest",
            &[
                (
                    "path",
                    "Option<String>",
                    "The workspace-relative directory to list. The root when omitted.",
                ),
            ],
        )
        .struct_type(
            "SlugifyRequest",
            &[("input", "String", "The string to convert to a slug.")],
        )
        .struct_type(
            "WordCountRequest",
            &[("input", "String", "The string whose words to count.")],
        )
        .struct_type(
            "TruncateRequest",
            &[
                ("input", "String", "The string to truncate."),
                (
                    "max_chars",
                    "u32",
                    "Maximum number of characters to keep before appending ellipsis.",
                ),
            ],
        )
        .struct_type(
            "CapitalizeRequest",
            &[("input", "String", "The string to title-case.")],
        )
        .foreign_type("SlugifyResponse", "String")
        .foreign_type("WordCountResponse", "String")
        .foreign_type("TruncateResponse", "String")
        .foreign_type("CapitalizeResponse", "String")
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
                .uses(&["workspace", "toolchain"])
                .tool(
                    "Regenerate model-derived code (generated.rs files) from the self-model. Call this after scaffolding a new service crate so generated.rs exists before authoring handlers.",
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
                .uses(&["workspace", "toolchain"])
                .tool(
                    "Edit the model. Each edit names a handle from `query`; a top-level node attaches to the model root, `model:<model>`. An operation's `tool` attribute is its agent tool description — an operation carrying one joins this tool catalog at the next rebuild. An operation's `uses` attribute declares the ports its handler reaches, comma-separated — `verify` holds the authored handler to exactly these. `write` true reprojects under a repository transaction and compile gate; a failed check restores the prior files.",
                )
                .operation(
                    "coverage",
                    "Report which operations have no authored handler.",
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
                .uses(&["workspace", "toolchain"])
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
                .uses(&["toolchain"])
                .tool(
                    "Compile-check the workspace and report the outcome. `implement` runs it after each write on its own. Call it directly after a `patch` that writes, or to prove the tree compiles before a rebuild.",
                )
                .operation(
                    "calc",
                    "Evaluate an arithmetic expression through the calculator service.",
                    "CalcRequest",
                    "CalcResult",
                )
                .uses(&["calculator"])
                .operation(
                    "scaffold",
                    "Write the skeleton of each library service crate that is missing it.",
                    "Empty",
                    "GeneratedFiles",
                )
                .uses(&["workspace", "toolchain"])
                .tool(
                    "Scaffold missing library service crates under a repository transaction and compile gate. A failed check restores the prior files and removes only paths the transaction created.",
                )
                .operation(
                    "test",
                    "Run the workspace tests and report the outcome.",
                    "Empty",
                    "CheckReport",
                )
                .uses(&["toolchain"])
                .tool(
                    "Run the workspace tests and report the outcome. Slower than check; use it when behavior matters.",
                )
                .operation(
                    "snapshot",
                    "Checkpoint tracked files and exact model-owned working-tree state, and return the snapshot id.",
                    "SnapshotRequest",
                    "String",
                )
                .uses(&["checkpoint"])
                .tool(
                    "Checkpoint tracked files and the exact present or absent state of paths owned by the current persisted model before risky edits. Returns a snapshot id for rollback. Requires write permission.",
                )
                .operation(
                    "rollback",
                    "Restore tracked files and exact model-owned working-tree state from a snapshot.",
                    "SnapshotRef",
                    "String",
                )
                .uses(&["checkpoint"])
                .tool(
                    "Restore tracked files and the exact present or absent state of model-owned paths from a snapshot, leaving unrelated untracked files untouched. Requires write permission.",
                )
                .operation(
                    "release",
                    "Release a snapshot that is no longer needed.",
                    "SnapshotRef",
                    "String",
                )
                .uses(&["checkpoint"])
                .tool(
                    "Release a snapshot by atomically deleting its validated pair of private Git refs. Requires write permission.",
                )
                .operation(
                    "prune",
                    "Release older snapshots beyond a retention limit.",
                    "SnapshotRetention",
                    "String",
                )
                .uses(&["checkpoint"])
                .tool(
                    "Release older snapshot refs, retaining only the requested number of newest snapshots. Requires write permission.",
                )
                .operation(
                    "diff",
                    "Show what changed in the working tree since a snapshot.",
                    "SnapshotRef",
                    "String",
                )
                .uses(&["checkpoint"])
                .tool(
                    "Show what changed in the working tree since a snapshot. `reference` is the snapshot id returned by `snapshot`. Returns a bounded, escaped Git-style diff with exact mode records, or an empty string when nothing has changed. Requires write permission.",
                )
                .operation(
                    "restart",
                    "Compile-check readiness for an inbound-managed process restart.",
                    "Empty",
                    "Empty",
                )
                .uses(&["toolchain"])
                .tool(
                    "Compile-check readiness for process replacement. The agent inbound uses success to rebuild and resume this session in the new binary; other inbounds must arrange their own rebuild and replacement. Apply the edits first — `patch` with write true, `implement` each handler, `check` — then call this alone, with no other tool in the turn.",
                )
                .operation(
                    "read",
                    "Read a workspace file, capped for a tool result.",
                    "ReadRequest",
                    "String",
                )
                .tool(
                    "Read a file from the workspace. `path` is workspace-relative, e.g. `rust/theseus/src/lib.rs`. The result is capped, so `search` first to find the right spot. Prefer `show` for an operation's handler or an adapter method — `read` reaches everything else: authored composition roots, generated files, manifests, docs. Example: { \"path\": \"README.md\" }.",
                )
                .operation(
                    "search",
                    "Find a pattern's occurrences across the workspace.",
                    "SearchRequest",
                    "String",
                )
                .tool(
                    "Search the workspace for lines containing `pattern`, reported as path:line: text, capped. `path` narrows the search to a subtree, e.g. `rust/agent`. Use it to find house patterns and neighbors before authoring, then `read` the file. Example: { \"pattern\": \"impl Toolchain\", \"path\": \"rust/theseus\" }.",
                )
                .operation(
                    "list",
                    "List a workspace directory.",
                    "ListRequest",
                    "String",
                )
                .tool(
                    "List a workspace directory's entries, directories marked with a trailing `/`. `path` is workspace-relative; omit it for the workspace root. Example: { \"path\": \"rust\" }.",
                )
                .operation(
                    "lint",
                    "Run clippy across the workspace with warnings denied.",
                    "Empty",
                    "CheckReport",
                )
                .uses(&["toolchain"])
                .tool(
                    "Run clippy across the workspace with warnings denied and report the outcome.",
                )
                .port(
                    Port::new("workspace", "Writes generated files into the workspace.")
                        .method(
                            "begin_mutation",
                            "Acquire the repository write lease and open a recoverable mutation after checking the expected generated revision.",
                            "ExpectedProjection",
                            "WorkspaceMutation",
                        )
                        .gated(),
                )
                .port(
                    Port::new("checkpoint", "Checkpoints and restores the working tree.")
                        .method(
                            "snapshot",
                            "Checkpoint tracked files and exact model-owned working-tree state.",
                            "CheckpointSnapshotRequest",
                            "CheckpointSnapshot",
                        )
                        .gated()
                        .method(
                            "restore",
                            "Restore tracked files and exact model-owned working-tree state from a snapshot.",
                            "CheckpointStateRequest",
                            "CheckpointRestore",
                        )
                        .gated()
                        .method(
                            "diff",
                            "Return a bounded, escaped Git-style diff with exact mode records against the given snapshot.",
                            "CheckpointStateRequest",
                            "String",
                        )
                        .gated()
                        .method(
                            "release",
                            "Release a snapshot that is no longer needed.",
                            "String",
                            "String",
                        )
                        .gated()
                        .method(
                            "prune",
                            "Release older snapshots beyond a retention limit.",
                            "SnapshotRetention",
                            "String",
                        )
                        .gated(),
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
                        )
                        .method(
                            "check_mutation",
                            "Compile-check an already leased workspace mutation, allowing its journal to cover lockfile updates.",
                            "Empty",
                            "CheckReport",
                        )
                        .method(
                            "test",
                            "Run the workspace tests and report the outcome.",
                            "Empty",
                            "CheckReport",
                        )
                        .method(
                            "lint",
                            "Run clippy across the workspace with warnings denied and report the outcome.",
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
        .service(
            Service::new("TextUtils")
                .crate_name("theseus-text-utils")
                .operation(
                    "slugify",
                    "Convert a string to a URL-safe slug.",
                    "SlugifyRequest",
                    "SlugifyResponse",
                )
                .operation(
                    "word_count",
                    "Count the words in a string.",
                    "WordCountRequest",
                    "WordCountResponse",
                )
                .operation(
                    "truncate",
                    "Truncate a string to at most N characters, appending an ellipsis when cut.",
                    "TruncateRequest",
                    "TruncateResponse",
                )
                .operation(
                    "capitalize",
                    "Capitalize the first letter of every word (title case).",
                    "CapitalizeRequest",
                    "CapitalizeResponse",
                ),
        )
        .inbound("theseus", Transport::Cli, "Theseus", "theseus-cli")
        .inbound("agent", Transport::Agent, "Theseus", "theseus-agent")
        .turns(32)
        .inbound_port(
            Port::new("llm", "Completes one turn of the conversation.")
                .method(
                    "complete",
                    "Complete one turn from the transcript and the tool catalog.",
                    "Turn",
                    "Reply",
                ),
        )
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
