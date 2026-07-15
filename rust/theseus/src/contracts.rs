//! Contract tests: the tool catalog and the serialized outcome envelopes the
//! wire surfaces and the agent depend on. A silent change to either — a
//! tool-tagged operation that falls out of the catalog, an entry that loses its
//! description or schema, or an outcome whose JSON shape drifts — fails here
//! rather than surfacing as a broken agent surface after release.

use std::collections::BTreeSet;

use theseus_model::theseus_model;
use theseus_modeling::{Edit, apply_edit};

use crate::{CheckReport, RustItemResult, tool_catalog};

/// The catalog is exactly the operations the model marks with a `tool`, in both
/// directions — so exposing an operation to the agent is a model edit, and an
/// operation that loses its `tool` attribute leaves the catalog with it.
#[test]
fn the_catalog_is_exactly_the_modeled_tool_operations() {
    let model = theseus_model();
    let modeled: BTreeSet<String> = model
        .services
        .iter()
        .flat_map(|service| &service.operations)
        .filter(|op| op.tool.is_some())
        .map(|op| op.name.clone())
        .collect();
    let catalog: BTreeSet<String> = tool_catalog()
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .expect("every catalog entry has a name")
                .to_string()
        })
        .collect();
    assert!(!catalog.is_empty(), "the catalog is not vacuously empty");
    assert_eq!(
        catalog, modeled,
        "the tool catalog must be exactly the operations the model marks with a `tool`"
    );
}

/// Every catalog entry is well-formed for a tool-using host: a non-empty
/// description and an object input schema.
#[test]
fn every_catalog_entry_carries_a_description_and_an_object_schema() {
    for tool in tool_catalog() {
        let name = tool["name"].as_str().expect("a catalog name");
        assert!(
            tool["description"].as_str().is_some_and(|d| !d.is_empty()),
            "catalog entry `{name}` needs a non-empty description"
        );
        assert_eq!(
            tool["input_schema"]["type"], "object",
            "catalog entry `{name}` needs an object input schema"
        );
    }
}

/// A refused model edit serializes as `ok: false` with a coded-diagnostic
/// envelope — each diagnostic a `{code, message, repair}` an agent can read and
/// act on. The envelope shape is the contract; the specific code is free to
/// change.
#[test]
fn a_patch_refusal_serializes_as_a_coded_diagnostic_envelope() {
    let (outcome, proposed) = apply_edit(
        &theseus_model(),
        &Edit::Remove {
            target: "op:theseus:does_not_exist".to_string(),
        },
    );
    assert!(proposed.is_none(), "a refused edit yields no new model");
    let json = serde_json::to_value(&outcome).expect("the outcome serializes");
    assert_eq!(json["ok"], false);
    let diagnostics = json["diagnostics"]
        .as_array()
        .expect("a refusal carries a diagnostics array");
    assert!(
        !diagnostics.is_empty(),
        "a refusal carries at least one diagnostic"
    );
    for diagnostic in diagnostics {
        for key in ["code", "message", "repair"] {
            assert!(
                diagnostic[key]
                    .as_str()
                    .is_some_and(|value| !value.is_empty()),
                "each diagnostic carries a non-empty `{key}`"
            );
        }
    }
}

/// A compile check serializes as `{ok, detail}`, the shape every gated outcome
/// carries inside it.
#[test]
fn a_check_report_serializes_with_ok_and_detail() {
    let json = serde_json::to_value(CheckReport::failure("the gate rolled it back"))
        .expect("the report serializes");
    assert_eq!(json["ok"], false);
    assert_eq!(json["detail"], "the gate rolled it back");
}

/// A compile-gated edit that rolls back carries its diagnostic code beside
/// `applied: false`, and a committed one omits the field — so a caller reads a
/// stable code exactly when the write did not land.
#[test]
fn a_rolled_back_edit_carries_its_code_and_a_commit_omits_it() {
    let rolled_back = RustItemResult {
        applied: false,
        path: "rust/theseus/src/service.rs".to_string(),
        item: "fn:probe".to_string(),
        revision: "0".repeat(16),
        detail: "the compile gate rolled it back".to_string(),
        code: Some("GATE002".to_string()),
        check: CheckReport::failure("errors"),
    };
    let json = serde_json::to_value(&rolled_back).expect("the result serializes");
    assert_eq!(json["code"], "GATE002");

    let committed = RustItemResult {
        applied: true,
        code: None,
        ..rolled_back
    };
    let json = serde_json::to_value(&committed).expect("the result serializes");
    assert!(
        json.get("code").is_none(),
        "a committed edit omits the diagnostic code"
    );
}
