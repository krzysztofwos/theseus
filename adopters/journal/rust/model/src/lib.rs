//! The journal's model of record and project layout.
//!
//! The journal exists to prove the engine from the outside — a workspace the
//! Theseus self-model knows nothing about. Its canonical model is tracked as
//! JSON, and the shared versioned Rust workspace layout derives every generated,
//! authored, and checkpoint-owned path from that record.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use theseus_modeling::{
    GeneratedFile, Model, ModelRecord, ProjectId, ProjectLayoutError, RustWorkspaceLayout,
};

/// The canonical model record, relative to the journal workspace root.
pub const MODEL_RECORD_PATH: &str = "model.json";

const MODEL_RECORD: &str = include_str!("../../../model.json");

/// The journal's model: one service over a `store` port, driven by a CLI.
pub fn journal_model() -> Model {
    serde_json::from_str(MODEL_RECORD).expect("the embedded journal model record is valid JSON")
}

/// Load the current model record from a journal workspace root.
pub fn load_model(root: &Path) -> anyhow::Result<Model> {
    let path = root.join(MODEL_RECORD_PATH);
    let source = fs::read_to_string(&path)
        .with_context(|| format!("reading the journal model record at {}", path.display()))?;
    serde_json::from_str(&source)
        .with_context(|| format!("parsing the journal model record at {}", path.display()))
}

/// The journal's stable project identity and versioned Rust workspace policy.
pub fn project_layout() -> RustWorkspaceLayout {
    RustWorkspaceLayout::new(
        ProjectId::new("journal").expect("the static journal project id is valid"),
        ModelRecord::json(MODEL_RECORD_PATH).expect("the static journal model path is valid"),
    )
}

/// The journal workspace root: the directory holding this adopter's `rust/`.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives at <root>/rust/model")
        .to_path_buf()
}

/// The files the journal projects from its model, drift-gated by `verify`.
pub fn generated_files(model: &Model) -> Result<Vec<GeneratedFile>, ProjectLayoutError> {
    project_layout().generated_files(model)
}

/// The authored impl path of every service, paired with the service name.
pub fn authored_impls(model: &Model) -> Result<Vec<(String, String)>, ProjectLayoutError> {
    project_layout().authored_impls(model)
}

/// The authored adapter path of each inbound with an interior port.
pub fn interior_impls(model: &Model) -> Result<Vec<(String, String)>, ProjectLayoutError> {
    project_layout().interior_impls(model)
}

/// Every path whose lifecycle belongs to the journal's modeled workflow.
pub fn owned_paths(model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
    project_layout().owned_paths(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_conforms_to_its_model() {
        let model = journal_model();
        let generated = generated_files(&model).expect("the journal model renders");
        let authored = authored_impls(&model).expect("the authored paths resolve");
        let interior = interior_impls(&model).expect("the interior paths resolve");
        let report =
            theseus_modeling::verify(&model, &workspace_root(), &generated, &authored, &interior);
        assert!(
            report.conformant,
            "the journal diverges from its model:\n{}",
            report.render()
        );
    }

    #[test]
    fn the_tracked_record_is_the_canonical_layout_projection() {
        let model = journal_model();
        let projected = generated_files(&model)
            .expect("the journal model renders")
            .into_iter()
            .find(|file| file.path == MODEL_RECORD_PATH)
            .expect("the layout projects its model record");

        assert_eq!(projected.contents, MODEL_RECORD);
        assert!(MODEL_RECORD.ends_with('\n'));
        assert!(!MODEL_RECORD.ends_with("\n\n"));
        assert_eq!(load_model(&workspace_root()).unwrap(), model);
    }

    #[test]
    fn the_layout_owns_the_record_and_journal_projection() {
        let model = journal_model();
        let layout = project_layout();
        assert_eq!(layout.project_id().as_str(), "journal");
        assert_eq!(layout.model_record().path(), MODEL_RECORD_PATH);
        assert_eq!(
            authored_impls(&model).unwrap(),
            vec![(
                "Journal".to_string(),
                "rust/journal/src/service.rs".to_string()
            )]
        );
        assert!(interior_impls(&model).unwrap().is_empty());

        let owned = owned_paths(&model).expect("the ownership catalogue resolves");
        for path in [
            "Cargo.lock",
            MODEL_RECORD_PATH,
            "rust/cli/src/generated.rs",
            "rust/journal/src/generated.rs",
            "rust/journal/src/lib.rs",
            "rust/journal/src/service.rs",
        ] {
            assert!(owned.iter().any(|owned| owned == path), "missing {path}");
        }
    }
}
