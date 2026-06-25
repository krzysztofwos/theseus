//! Theseus's model of itself — the adopter over [`theseus_modeling`].
//!
//! Supplies the concrete [`theseus_model`] plus the locations Theseus owns: the
//! self-model source and the generated scaffolding the engine renders into.

mod self_model;

pub use self_model::theseus_model;
use theseus_modeling::{GeneratedFile, Model, render_cli_module, render_model_source};

/// The self-model source file, relative to the workspace root. It is the model's
/// own projection — `generate` and `patch` reproject it.
pub const SELF_MODEL_PATH: &str = "rust/model/src/self_model.rs";

/// The generated scaffolding module, relative to the workspace root.
pub const GENERATED_CLI_PATH: &str = "rust/cli/src/generated.rs";

/// The authored service implementation, relative to the workspace root. `verify`
/// and the coverage report read it to find which operations have a handler.
pub const AUTHORED_IMPL_PATH: &str = "rust/cli/src/service.rs";

/// The leading comment block of the projected self-model source.
const SELF_MODEL_HEADER: &str = concat!(
    "// @generated projection of the model — the fixed point.\n",
    "// `theseus generate` and `patch` reproject this file. Edit its structure\n",
    "// freely (the compiler reads it), and it is kept in canonical form.\n",
    "//! Theseus's model of itself: the fixed point that describes the very tool\n",
    "//! that holds it, projected back to its builder form.\n",
    "\n",
);

/// The files Theseus projects from `model`: the CLI scaffolding and the self-model
/// source itself. `generate` and `patch` write them. `verify` drift-gates them, so
/// the self-model source is checked to be a fixed point of the renderer.
pub fn generated_files(model: &Model) -> Vec<GeneratedFile> {
    vec![
        GeneratedFile {
            path: GENERATED_CLI_PATH.to_string(),
            contents: render_cli_module(model),
        },
        GeneratedFile {
            path: SELF_MODEL_PATH.to_string(),
            contents: render_model_source(model, SELF_MODEL_HEADER, "theseus_model"),
        },
    ]
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use theseus_modeling::verify;

    use super::*;

    /// The repository root, derived from this crate's compile-time location.
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("crate lives at <root>/rust/model")
            .to_path_buf()
    }

    #[test]
    fn self_model_is_consistent() {
        let model = theseus_model();
        for node in &model.crates {
            for dep in &node.depends_on {
                assert!(
                    model.crate_named(dep).is_some(),
                    "dependency `{dep}` of `{}` is not a modeled crate",
                    node.name
                );
            }
        }
    }

    #[test]
    fn theseus_conforms_to_its_self_model() {
        let model = theseus_model();
        let report = verify(
            &model,
            &workspace_root(),
            &generated_files(&model),
            AUTHORED_IMPL_PATH,
        );
        assert!(
            report.conformant,
            "self-verification failed:\n{}",
            report.render()
        );
    }

    #[test]
    fn a_forbidden_dependency_is_detected() {
        // A model that (falsely) claims the kernel depends outward on the cli.
        let mut model = theseus_model();
        if let Some(kernel) = model.crates.iter_mut().find(|c| c.dir == "kernel") {
            kernel.depends_on.push("theseus-cli".to_string());
        }
        let report = verify(
            &model,
            &workspace_root(),
            &generated_files(&model),
            AUTHORED_IMPL_PATH,
        );
        assert!(!report.conformant);
    }

    #[test]
    fn rendered_surface_covers_every_operation() {
        let model = theseus_model();
        let rendered = render_cli_module(&model);
        for op in model.operations() {
            assert!(
                rendered.contains(&format!("Command::new({:?})", op.name)),
                "operation `{}` missing from generated surface",
                op.name
            );
        }
    }

    #[test]
    fn self_model_source_is_a_fixed_point() {
        // The on-disk self-model source must equal its own render: it projects to
        // the model that projects back to it.
        let model = theseus_model();
        let rendered = generated_files(&model)
            .into_iter()
            .find(|file| file.path == SELF_MODEL_PATH)
            .expect("self-model is a projected file")
            .contents;
        let on_disk = std::fs::read_to_string(workspace_root().join(SELF_MODEL_PATH)).unwrap();
        assert_eq!(
            on_disk, rendered,
            "self-model source is not its own projection"
        );
    }
}
