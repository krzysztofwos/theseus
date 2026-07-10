//! The journal's projection binary: write the crate skeletons that are missing
//! and every generated file, straight from the model. The adopter's analog of
//! `theseus scaffold` + `theseus generate`, standing only on the engine and
//! this model.

use journal_model::{generated_files, journal_model, workspace_root};
use theseus_workspace::{FsMutation, MutationFile};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let root = workspace_root();
    let model = journal_model();
    let mut mutation = FsMutation::begin(&root, &[])?;
    let mut files = Vec::new();
    for file in theseus_modeling::scaffold_files(&model) {
        let path = root.join(&file.path);
        if path.exists() {
            continue;
        }
        files.push(("scaffolded", file));
    }
    for file in generated_files(&model)? {
        files.push(("wrote", file));
    }
    let changes: Vec<_> = files
        .iter()
        .map(|(_, file)| MutationFile::text(file.path.clone(), file.contents.clone()))
        .collect();
    mutation.apply(&changes).await?;
    mutation.commit()?;
    for (action, file) in files {
        println!("{action} {}", file.path);
    }
    Ok(())
}
