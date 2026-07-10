//! The bootstrap regenerator: render and write the generated files from a build
//! that stands only on the engine and the model.
//!
//! `theseus generate` runs inside the binary this workspace builds, so its build
//! reaches every crate the CLI composes — including the generated files on disk
//! and the authored code that consumes them. An edit that changes a renderer
//! together with authored code consuming the renderer's new output wedges that
//! path: the files that would fix the compile are files only the broken build
//! can produce. This binary's build reaches `theseus-modeling` and
//! `theseus-model` alone — the two crates that compile without any generated
//! file — so it compiles in every such state and writes the tree back to
//! buildable, where the modeled `generate` takes over again.

use std::path::{Path, PathBuf};

use theseus_model::{crate_is_scaffolded, generated_files, theseus_model};
use theseus_workspace::{FsMutation, MutationFile};

/// The repository root, derived from this crate's compile-time location.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crate lives at <root>/rust/model")
        .to_path_buf()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let root = workspace_root();
    let model = theseus_model();
    let mutation = FsMutation::begin(&root, &[])?;
    let files: Vec<_> = generated_files(&model)?
        .into_iter()
        .filter(|file| crate_is_scaffolded(&root, file))
        .collect();
    let changes: Vec<_> = files
        .iter()
        .map(|file| MutationFile::text(file.path.clone(), file.contents.clone()))
        .collect();
    let mut mutation = mutation;
    mutation.apply(&changes).await?;
    mutation.commit()?;
    for file in files {
        println!("wrote {}", file.path);
    }
    Ok(())
}
