//! Theseus's model of itself — the adopter over [`theseus_modeling`].
//!
//! Supplies the concrete [`theseus_model`] plus the locations Theseus owns: the
//! self-model source and the generated scaffolding the engine renders into.

mod self_model;

pub use self_model::theseus_model;
use theseus_modeling::{
    GeneratedFile, Inbound, Model, ModelRecord, ProjectId, ProjectLayoutError, RustWorkspaceLayout,
    Service,
};
use theseus_workspace::{ExpectedFile, ExpectedFileSet};

/// The self-model source file, relative to the workspace root. It is the model's
/// own projection — `generate` and `patch` reproject it.
pub const SELF_MODEL_PATH: &str = "rust/model/src/self_model.rs";

/// The leading comment block of the projected self-model source.
const SELF_MODEL_HEADER: &str = concat!(
    "// @generated projection of the model — the fixed point.\n",
    "// `theseus generate` and `patch` reproject this file. Edit its structure\n",
    "// freely (the compiler reads it), and it is kept in canonical form.\n",
    "//! Theseus's model of itself: the fixed point that describes the very tool\n",
    "//! that holds it, projected back to its builder form.\n",
    "\n",
);

/// Theseus's stable project identity and versioned Rust workspace policy.
pub fn project_layout() -> Result<RustWorkspaceLayout, ProjectLayoutError> {
    Ok(RustWorkspaceLayout::new(
        ProjectId::new("theseus")?,
        ModelRecord::rust_builder(SELF_MODEL_PATH, SELF_MODEL_HEADER, "theseus_model")?,
    ))
}

/// The authored service implementation for `service`, relative to the workspace
/// root: the `service.rs` of the crate the service lives in. `verify` and the
/// coverage report read it to find which operations have a handler.
pub fn authored_impl_path(model: &Model, service: &Service) -> Result<String, ProjectLayoutError> {
    project_layout()?.authored_impl_path(model, service)
}

/// The authored adapters file for `service`'s crate: the `lib.rs` beside the
/// generated contract, where the crate's shared port adapters live. The
/// `implement` and `show` operations reach a port's adapter methods here.
pub fn adapter_impl_path(model: &Model, service: &Service) -> Result<String, ProjectLayoutError> {
    project_layout()?.adapter_impl_path(model, service)
}

/// The authored adapters file of an inbound's interior ports: the `adapters.rs`
/// of the crate that hosts the inbound.
pub fn inbound_adapter_impl_path(
    model: &Model,
    inbound: &Inbound,
) -> Result<String, ProjectLayoutError> {
    project_layout()?.inbound_adapter_impl_path(model, inbound)
}

/// Each inbound carrying interior ports, paired with its authored adapters
/// file, for the interior-coverage check.
pub fn interior_impls(model: &Model) -> Result<Vec<(String, String)>, ProjectLayoutError> {
    project_layout()?.interior_impls(model)
}

/// The authored impl path of every service, paired with the service name.
pub fn authored_impls(model: &Model) -> Result<Vec<(String, String)>, ProjectLayoutError> {
    project_layout()?.authored_impls(model)
}

/// Every exact file path whose lifecycle is owned by Theseus's model-driven
/// workflow. Checkpoints use this complete, ambient-state-independent catalogue
/// to capture or tombstone only files the harness can create or edit.
pub fn checkpoint_paths(model: &Model) -> Result<Vec<String>, ProjectLayoutError> {
    project_layout()?.owned_paths(model)
}

/// The exact generated revision a checkpoint plan must observe after taking the
/// repository lease. Generated files for crates not yet scaffolded are expected
/// to remain absent.
pub fn checkpoint_expectations(
    root: &std::path::Path,
    model: &Model,
) -> Result<ExpectedFileSet, ProjectLayoutError> {
    Ok(generated_files(model)?
        .into_iter()
        .map(|file| ExpectedFile {
            contents: crate_is_scaffolded(root, &file).then_some(file.contents),
            path: file.path,
        })
        .collect())
}

/// Whether a generated file's crate is scaffolded — has a `Cargo.toml` on disk.
/// A crate added to the model is registered before its skeleton is written, so
/// its generated code waits for `scaffold` to lay the directory the workspace
/// can build.
pub fn crate_is_scaffolded(root: &std::path::Path, file: &GeneratedFile) -> bool {
    match file
        .path
        .strip_prefix("rust/")
        .and_then(|rest| rest.split_once('/'))
    {
        Some((dir, _)) => root.join("rust").join(dir).join("Cargo.toml").exists(),
        None => true,
    }
}

/// The files Theseus projects from `model`: the CLI scaffolding and the self-model
/// source itself. `generate` and `patch` write them. `verify` drift-gates them, so
/// the self-model source is checked to be a fixed point of the renderer.
pub fn generated_files(model: &Model) -> Result<Vec<GeneratedFile>, ProjectLayoutError> {
    project_layout()?.generated_files(model)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use theseus_modeling::{RenderError, render_cli_module, verify};

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
    fn checkpoint_paths_are_exact_sorted_and_independent_of_disk_state() {
        let model = Model::new("Probe")
            .crate_node("probe", "probe", 0, &[])
            .service(Service::new("Probe").crate_name("probe"));

        assert_eq!(
            checkpoint_paths(&model).expect("the ownership catalogue renders"),
            [
                "Cargo.lock",
                "Cargo.toml",
                "rust/model/src/self_model.rs",
                "rust/probe/Cargo.toml",
                "rust/probe/src/generated.rs",
                "rust/probe/src/lib.rs",
                "rust/probe/src/service.rs",
                "theseus.json",
            ]
        );
    }

    #[test]
    fn checkpoint_paths_reuse_projection_path_validation() {
        let model = Model::new("Probe")
            .crate_node("probe", "../outside", 0, &[])
            .service(Service::new("Probe").crate_name("probe"));

        assert!(matches!(
            checkpoint_paths(&model),
            Err(ProjectLayoutError::Render(
                RenderError::InvalidCrateDirectory { .. }
            ))
        ));
    }

    #[test]
    fn theseus_conforms_to_its_self_model() {
        let model = theseus_model();
        let generated = generated_files(&model).expect("self-model renders");
        let authored = authored_impls(&model).expect("authored paths resolve");
        let interiors = interior_impls(&model).expect("interior paths resolve");
        let report = verify(&model, &workspace_root(), &generated, &authored, &interiors);
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
        let generated = generated_files(&model).expect("self-model renders");
        let authored = authored_impls(&model).expect("authored paths resolve");
        let interiors = interior_impls(&model).expect("interior paths resolve");
        let report = verify(&model, &workspace_root(), &generated, &authored, &interiors);
        assert!(!report.conformant);
    }

    #[test]
    fn rendered_surface_covers_every_cli_operation() {
        use theseus_modeling::Transport;
        let model = theseus_model();
        let rendered = render_cli_module(&model).expect("CLI renders");
        let inbound = model
            .inbounds
            .iter()
            .find(|inbound| inbound.transport == Transport::Cli)
            .expect("the model has a CLI inbound adapter");
        let service = model
            .service_named(&inbound.service)
            .expect("the inbound drives a defined service");
        for op in &service.operations {
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
            .expect("self-model renders")
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
