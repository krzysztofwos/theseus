//! The journal's projection binary: write the crate skeletons that are missing
//! and every generated file, straight from the model. The adopter's analog of
//! `theseus scaffold` + `theseus generate`, standing only on the engine and
//! this model.

use journal_model::{generated_files, journal_model, workspace_root};

fn main() -> anyhow::Result<()> {
    let root = workspace_root();
    let model = journal_model();
    for file in theseus_modeling::scaffold_files(&model) {
        let path = root.join(&file.path);
        if path.exists() {
            continue;
        }
        write(&path, &file.contents)?;
        println!("scaffolded {}", file.path);
    }
    for file in generated_files(&model) {
        write(&root.join(&file.path), &file.contents)?;
        println!("wrote {}", file.path);
    }
    Ok(())
}

fn write(path: &std::path::Path, contents: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, contents)?;
    Ok(())
}
