//! The journal's model of record: a second adopter of the Theseus engine.
//!
//! The journal exists to prove the engine from the outside — a workspace the
//! Theseus self-model knows nothing about, holding its own model, its own path
//! conventions, and its own projection binary. Everything the engine renders
//! here (contracts, port traits, the command surface, the conformance checks)
//! it renders from this model alone.

use std::path::{Path, PathBuf};

use theseus_modeling::{GeneratedFile, Model, Port, Service, Transport};

/// The journal's model: one service over a `store` port, driven by a CLI.
pub fn journal_model() -> Model {
    Model::new("Journal")
        .crate_node("journal-model", "model", 0, &[])
        .crate_node("journal", "journal", 1, &[])
        .crate_node("journal-cli", "cli", 2, &["journal", "journal-model"])
        .struct_type("AddRequest", &[("text", "String", "The entry to record.")])
        .struct_type("SearchRequest", &[("term", "String", "The text to look for.")])
        .service(
            Service::new("Journal")
                .crate_name("journal")
                .operation("add", "Record one journal entry.", "AddRequest", "String")
                .uses(&["store"])
                .operation("list", "Print every journal entry.", "Empty", "String")
                .uses(&["store"])
                .operation(
                    "search",
                    "Print the entries containing a term.",
                    "SearchRequest",
                    "String",
                )
                .uses(&["store"])
                .port(
                    Port::new("store", "Persists the journal's entries.")
                        .method("append", "Append one entry.", "String", "Empty")
                        .method("read_all", "Read every entry.", "Empty", "String"),
                ),
        )
        .inbound("journal", Transport::Cli, "Journal", "journal-cli")
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
pub fn generated_files(model: &Model) -> Vec<GeneratedFile> {
    let mut files = Vec::new();
    for crate_name in ["journal", "journal-cli"] {
        let dir = model
            .crate_named(crate_name)
            .map(|node| node.dir.as_str())
            .expect("the crate is modeled");
        files.push(GeneratedFile {
            path: format!("rust/{dir}/src/generated.rs"),
            contents: theseus_modeling::render_module_for_crate(model, crate_name),
        });
    }
    files
}

/// The authored impl path of every service, paired with the service name.
pub fn authored_impls(model: &Model) -> Vec<(String, String)> {
    model
        .services
        .iter()
        .map(|service| {
            let dir = model
                .crate_named(&service.crate_name)
                .map(|node| node.dir.as_str())
                .expect("the service's crate is modeled");
            (service.name.clone(), format!("rust/{dir}/src/service.rs"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_journal_conforms_to_its_model() {
        let model = journal_model();
        let report = theseus_modeling::verify(
            &model,
            &workspace_root(),
            &generated_files(&model),
            &authored_impls(&model),
            &[],
        );
        assert!(
            report.conformant,
            "the journal diverges from its model:\n{}",
            report.render()
        );
    }
}
