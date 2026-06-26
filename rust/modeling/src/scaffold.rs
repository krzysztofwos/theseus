//! Crate scaffolding: the one-time skeleton of a service crate, rendered from
//! the model.
//!
//! Code generation keeps rewriting a crate's generated contract. Scaffolding
//! instead lays down the authored leaves a new service crate starts from — its
//! manifest, its module wiring, and an empty adapter. These are authored after
//! they land, so an adopter writes only the files that are absent and never
//! clobbers work. The skeleton is produced for a library service crate: one that
//! hosts services and no inbound adapter of its own.

use crate::{
    codegen::{GeneratedFile, pascal_case},
    model::{CrateNode, Model, Service, TypeShape},
};

/// The skeleton files for every library service crate the model describes.
pub fn scaffold_files(model: &Model) -> Vec<GeneratedFile> {
    let mut files = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for service in &model.services {
        if seen.contains(&service.crate_name.as_str()) {
            continue;
        }
        seen.push(&service.crate_name);

        let services: Vec<&Service> = model
            .services
            .iter()
            .filter(|s| s.crate_name == service.crate_name)
            .collect();
        // A crate that also hosts an inbound adapter is a binary with a
        // hand-written composition root, not the library adapter skeleton.
        if model
            .inbounds
            .iter()
            .any(|inbound| inbound.crate_name == service.crate_name)
        {
            continue;
        }
        let Some(node) = model.crate_named(&service.crate_name) else {
            continue;
        };

        let dir = &node.dir;
        files.push(GeneratedFile {
            path: format!("rust/{dir}/Cargo.toml"),
            contents: cargo_toml(node, &services, model),
        });
        files.push(GeneratedFile {
            path: format!("rust/{dir}/src/lib.rs"),
            contents: lib_rs(&services, model),
        });
        files.push(GeneratedFile {
            path: format!("rust/{dir}/src/service.rs"),
            contents: service_rs(&services),
        });
    }
    files
}

/// Render the crate manifest: `anyhow` for the generated contract, then a path
/// dependency for each modeled crate dependency.
fn cargo_toml(node: &CrateNode, services: &[&Service], model: &Model) -> String {
    let mut paths: Vec<String> = node
        .depends_on
        .iter()
        .filter_map(|dep| {
            model
                .crate_named(dep)
                .map(|d| format!("{dep} = {{ path = \"../{}\" }}\n", d.dir))
        })
        .collect();
    paths.sort();
    let path_block = if paths.is_empty() {
        String::new()
    } else {
        format!("\n{}", paths.concat())
    };
    format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"{}\"\n\n\
         [dependencies]\nanyhow = {{ workspace = true }}\n{path_block}",
        node.name,
        description(services),
    )
}

/// Render the library root: the module wiring and the public re-exports.
fn lib_rs(services: &[&Service], model: &Model) -> String {
    let mut contract: Vec<String> = services.iter().map(|s| trait_name(s)).collect();
    contract.extend(request_structs(services, model));
    let adapters: Vec<String> = services.iter().map(|s| adapter_name(s)).collect();
    format!(
        "//! {}\n\nmod generated;\nmod service;\n\n{}\n{}\n",
        description(services),
        use_list("generated", &contract),
        use_list("service", &adapters),
    )
}

/// Render the authored adapter: an empty implementation of each service contract,
/// whose methods fall through to their `unimplemented` defaults until authored.
fn service_rs(services: &[&Service]) -> String {
    let traits: Vec<String> = services.iter().map(|s| trait_name(s)).collect();
    let blocks: Vec<String> = services
        .iter()
        .map(|s| {
            let adapter = adapter_name(s);
            format!(
                "/// The {} adapter.\npub struct {adapter};\n\nimpl {} for {adapter} {{}}\n",
                s.name,
                trait_name(s),
            )
        })
        .collect();
    format!(
        "//! The authored adapter implementing the generated contract.\n//!\n\
         //! A method without a handler here falls through to the trait's `unimplemented`\n\
         //! default, and the coverage check reports it. The structured-edit tooling writes\n\
         //! the handlers into this file.\n\n{}\n\n{}",
        use_list("crate::generated", &traits),
        blocks.join("\n"),
    )
}

/// A `use <path>::{...};` line, unbraced for a single item.
fn use_list(path: &str, items: &[String]) -> String {
    match items {
        [one] => format!("pub use {path}::{one};"),
        many => format!("pub use {path}::{{{}}};", many.join(", ")),
    }
}

fn trait_name(service: &Service) -> String {
    format!("{}Service", pascal_case(&service.name))
}

fn adapter_name(service: &Service) -> String {
    pascal_case(&service.name)
}

/// A one-line crate description naming the services it hosts.
fn description(services: &[&Service]) -> String {
    let names: Vec<&str> = services.iter().map(|s| s.name.as_str()).collect();
    format!("The {} service", names.join(" and "))
}

/// The distinct struct request types the services' operations take. These are the
/// types the crate's generated module emits and the library re-exports.
fn request_structs(services: &[&Service], model: &Model) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for service in services {
        for op in &service.operations {
            if op.request != "Empty"
                && let Some(def) = model.type_def(&op.request)
                && matches!(def.shape, TypeShape::Struct(_))
                && !seen.contains(&def.name)
            {
                seen.push(def.name.clone());
            }
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Model, Transport};

    fn calculator_model() -> Model {
        Model::new("App")
            .crate_node("app", "app", 1, &[])
            .struct_type("Operands", &[("a", "f64", "Left.")])
            .foreign_type("Sum", "String")
            .service(
                Service::new("Calculator")
                    .crate_name("app")
                    .operation("add", "Add.", "Operands", "Sum"),
            )
    }

    fn file<'a>(files: &'a [GeneratedFile], suffix: &str) -> &'a str {
        files
            .iter()
            .find(|f| f.path.ends_with(suffix))
            .map(|f| f.contents.as_str())
            .unwrap_or_else(|| panic!("no scaffolded file ending in `{suffix}`"))
    }

    #[test]
    fn scaffolds_a_library_service_crate() {
        let files = scaffold_files(&calculator_model());
        let cargo = file(&files, "app/Cargo.toml");
        assert!(cargo.contains("anyhow = { workspace = true }"));

        let lib = file(&files, "app/src/lib.rs");
        assert!(lib.contains("mod generated;"));
        assert!(lib.contains("pub use generated::{CalculatorService, Operands};"));
        assert!(lib.contains("pub use service::Calculator;"));

        let service = file(&files, "app/src/service.rs");
        assert!(service.contains("pub struct Calculator;"));
        assert!(service.contains("impl CalculatorService for Calculator {}"));
    }

    #[test]
    fn a_path_dependency_is_rendered_from_the_crate_graph() {
        let model = Model::new("App")
            .crate_node("kit", "kit", 0, &[])
            .crate_node("app", "app", 1, &["kit"])
            .service(Service::new("Worker").crate_name("app"));
        let files = scaffold_files(&model);
        let cargo = file(&files, "app/Cargo.toml");
        assert!(cargo.contains("kit = { path = \"../kit\" }"));
    }

    #[test]
    fn a_crate_hosting_an_inbound_and_a_service_is_not_scaffolded() {
        // A crate with an inbound adapter is a binary with an authored root.
        let model = Model::new("App")
            .crate_node("cli", "cli", 0, &[])
            .service(Service::new("App").crate_name("cli"))
            .inbound("app", Transport::Cli, "App", "cli");
        assert!(scaffold_files(&model).is_empty());
    }
}
