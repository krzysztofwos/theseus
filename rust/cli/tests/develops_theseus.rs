//! The `theseus` binary develops Theseus's own model.
//!
//! These drive the compiled CLI in read and preview mode, so they are
//! non-destructive: `query` reads the model, and `patch` without `--write`
//! previews an edit and reports the outcome without touching the workspace.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_theseus");

/// Run the binary and return its stdout and whether it exited zero.
fn theseus(args: &[&str]) -> (String, bool) {
    let output = Command::new(BIN).args(args).output().expect("run theseus");
    (
        String::from_utf8(output.stdout).unwrap(),
        output.status.success(),
    )
}

/// The current model hash, read from `query`'s output.
fn model_hash() -> String {
    let (out, ok) = theseus(&["query"]);
    assert!(ok, "query failed");
    let key = "\"model_hash\": \"";
    let start = out.find(key).expect("hash present") + key.len();
    let end = out[start..].find('"').unwrap() + start;
    out[start..end].to_string()
}

#[test]
fn query_lists_operations_types_and_ports() {
    let (out, ok) = theseus(&["query"]);
    assert!(ok);
    assert!(out.contains("op:theseus:verify"));
    assert!(out.contains("type:theseus:PatchRequest"));
    assert!(out.contains("port:theseus:workspace"));
}

#[test]
fn query_emits_nested_handles() {
    let (out, ok) = theseus(&["query"]);
    assert!(ok);
    // A field of a struct type and a method of a port are addressable too.
    assert!(out.contains("field:theseus:PatchRequest.verb"));
    assert!(out.contains("method:theseus:workspace.write_file"));
}

#[test]
fn query_node_narrows_to_one_handle() {
    let (out, ok) = theseus(&["query", "--node", "op:theseus:patch"]);
    assert!(ok);
    assert!(out.contains("op:theseus:patch"));
    assert!(!out.contains("op:theseus:verify"));
}

#[test]
fn query_find_searches_handles() {
    let (out, ok) = theseus(&["query", "--find", "workspace"]);
    assert!(ok);
    assert!(out.contains("port:theseus:workspace"));
    assert!(!out.contains("op:theseus:model"));
}

#[test]
fn query_kind_keeps_one_element_kind() {
    let (out, ok) = theseus(&["query", "--kind", "port"]);
    assert!(ok);
    assert!(out.contains("port:theseus:workspace"));
    assert!(!out.contains("op:theseus:verify"));
    assert!(!out.contains("type:theseus:PatchRequest"));
}

#[test]
fn patch_adds_an_operation() {
    let hash = model_hash();
    let (out, ok) = theseus(&[
        "patch",
        "--verb",
        "add",
        "--target",
        "model:theseus",
        "--kind",
        "operation",
        "--name",
        "echo",
        "--set",
        "summary=Echo the input back.",
        "--expect-model-hash",
        &hash,
    ]);
    assert!(ok);
    assert!(out.contains("\"ok\": true"));
    assert!(out.contains("+ operation echo"));
}

#[test]
fn patch_adds_a_field_to_a_struct() {
    let hash = model_hash();
    let (out, ok) = theseus(&[
        "patch",
        "--verb",
        "add",
        "--target",
        "type:theseus:QueryRequest",
        "--kind",
        "field",
        "--name",
        "limit",
        "--set",
        "ty=Option<String>",
        "--set",
        "doc=Cap the number of handles returned.",
        "--expect-model-hash",
        &hash,
    ]);
    assert!(ok);
    assert!(out.contains("+ field QueryRequest.limit: Option<String>"));
}

#[test]
fn patch_renames_an_operation() {
    let hash = model_hash();
    let (out, ok) = theseus(&[
        "patch",
        "--verb",
        "rename",
        "--target",
        "op:theseus:query",
        "--to",
        "lookup",
        "--expect-model-hash",
        &hash,
    ]);
    assert!(ok);
    assert!(out.contains("~ operation query -> lookup"));
}

#[test]
fn a_stale_patch_is_rejected() {
    let (out, ok) = theseus(&[
        "patch",
        "--verb",
        "add",
        "--target",
        "model:theseus",
        "--kind",
        "operation",
        "--name",
        "echo",
        "--expect-model-hash",
        "deadbeef",
    ]);
    assert!(!ok, "a stale patch must exit non-zero");
    assert!(out.contains("PATCH001"));
}

#[test]
fn removing_a_referenced_type_is_refused() {
    let hash = model_hash();
    let (out, ok) = theseus(&[
        "patch",
        "--verb",
        "remove",
        "--target",
        "type:theseus:VerifyReport",
        "--expect-model-hash",
        &hash,
    ]);
    assert!(!ok);
    assert!(out.contains("PATCH009"));
}
