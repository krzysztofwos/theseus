//! Theseus's model of itself — the adopter over [`theseus_modeling`].
//!
//! Supplies the concrete [`theseus_model`] plus the locations Theseus owns: the
//! self-model source and the generated scaffolding the engine renders into.

mod self_model;

pub use self_model::theseus_model;
use theseus_modeling::{
    GeneratedFile, Model, RenderError, Service, Transport, render_model_source,
    render_module_for_crate, render_proto,
};

/// The self-model source file, relative to the workspace root. It is the model's
/// own projection — `generate` and `patch` reproject it.
pub const SELF_MODEL_PATH: &str = "rust/model/src/self_model.rs";

/// The authored service implementation for `service`, relative to the workspace
/// root: the `service.rs` of the crate the service lives in. `verify` and the
/// coverage report read it to find which operations have a handler.
pub fn authored_impl_path(model: &Model, service: &Service) -> String {
    let dir = crate_dir(model, service);
    format!("rust/{dir}/src/service.rs")
}

/// The authored adapters file for `service`'s crate: the `lib.rs` beside the
/// generated contract, where the crate's shared port adapters live. The
/// `implement` and `show` operations reach a port's adapter methods here.
pub fn adapter_impl_path(model: &Model, service: &Service) -> String {
    format!("rust/{}/src/lib.rs", crate_dir(model, service))
}

/// The authored adapters file of an inbound's interior ports: the `adapters.rs`
/// of the crate that hosts the inbound.
pub fn inbound_adapter_impl_path(model: &Model, inbound: &theseus_modeling::Inbound) -> String {
    format!(
        "rust/{}/src/adapters.rs",
        dir_of(model, &inbound.crate_name)
    )
}

/// Each inbound carrying interior ports, paired with its authored adapters
/// file, for the interior-coverage check.
pub fn interior_impls(model: &Model) -> Vec<(String, String)> {
    model
        .inbounds
        .iter()
        .filter(|inbound| !inbound.outbound.is_empty())
        .map(|inbound| {
            (
                inbound.name.clone(),
                inbound_adapter_impl_path(model, inbound),
            )
        })
        .collect()
}

/// The authored impl path of every service, paired with the service name.
pub fn authored_impls(model: &Model) -> Vec<(String, String)> {
    model
        .services
        .iter()
        .map(|service| (service.name.clone(), authored_impl_path(model, service)))
        .collect()
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

/// The directory under `rust/` of the crate a service lives in.
fn crate_dir<'a>(model: &'a Model, service: &Service) -> &'a str {
    dir_of(model, &service.crate_name)
}

/// The directory under `rust/` of a named crate. Patch refuses an edit naming
/// an unmodeled crate (PATCH017), so a model that reaches here resolves.
fn dir_of<'a>(model: &'a Model, crate_name: &str) -> &'a str {
    model
        .crate_named(crate_name)
        .map(|node| node.dir.as_str())
        .unwrap_or_else(|| panic!("crate `{crate_name}` is not modeled"))
}

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
pub fn generated_files(model: &Model) -> Result<Vec<GeneratedFile>, RenderError> {
    let mut files = Vec::new();
    let mut rendered: Vec<&str> = Vec::new();
    // Every crate that hosts a service, a CLI, HTTP, or gRPC inbound, an inbound
    // with an interior of its own — a loop's ports or turn budget — or a client
    // adapter gets a generated file. An agent or MCP inbound with no interior
    // drives the tool surface rendered with its service's crate, so its own
    // crate gets none.
    let hosting = model
        .services
        .iter()
        .map(|service| service.crate_name.as_str())
        .chain(
            model
                .inbounds
                .iter()
                .filter(|inbound| {
                    matches!(
                        inbound.transport,
                        Transport::Cli | Transport::Http | Transport::Grpc
                    ) || !inbound.outbound.is_empty()
                        || inbound.turns.is_some()
                })
                .map(|inbound| inbound.crate_name.as_str()),
        )
        .chain(
            model
                .clients
                .iter()
                .map(|client| client.crate_name.as_str()),
        );
    for crate_name in hosting {
        if rendered.contains(&crate_name) {
            continue;
        }
        rendered.push(crate_name);
        let dir = model
            .crate_named(crate_name)
            .map(|node| node.dir.as_str())
            .ok_or_else(|| RenderError::CrateNotModeled {
                crate_name: crate_name.to_string(),
            })?;
        files.push(GeneratedFile {
            path: format!("rust/{dir}/src/generated.rs"),
            contents: render_module_for_crate(model, crate_name)?,
        });
    }
    // A gRPC inbound or client crate also carries its proto contract — the wire
    // schema its build compiles — drift-gated like every generated file.
    let grpc_hosts = model
        .inbounds
        .iter()
        .filter(|inbound| inbound.transport == Transport::Grpc)
        .map(|inbound| (inbound.crate_name.as_str(), inbound.service.as_str()))
        .chain(
            model
                .clients
                .iter()
                .filter(|client| client.transport == Transport::Grpc)
                .map(|client| (client.crate_name.as_str(), client.service.as_str())),
        );
    for (crate_name, service_name) in grpc_hosts {
        if let Some(service) = model.service_named(service_name) {
            let dir = model
                .crate_named(crate_name)
                .map(|node| node.dir.as_str())
                .ok_or_else(|| RenderError::CrateNotModeled {
                    crate_name: crate_name.to_string(),
                })?;
            files.push(GeneratedFile {
                path: format!("rust/{dir}/proto/{}.proto", service.name.to_lowercase()),
                contents: render_proto(model, service)?,
            });
        }
    }
    files.push(GeneratedFile {
        path: SELF_MODEL_PATH.to_string(),
        contents: render_model_source(model, SELF_MODEL_HEADER, "theseus_model")?,
    });
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use theseus_modeling::{render_cli_module, verify};

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
        let generated = generated_files(&model).expect("self-model renders");
        let report = verify(
            &model,
            &workspace_root(),
            &generated,
            &authored_impls(&model),
            &interior_impls(&model),
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
        let generated = generated_files(&model).expect("self-model renders");
        let report = verify(
            &model,
            &workspace_root(),
            &generated,
            &authored_impls(&model),
            &interior_impls(&model),
        );
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
