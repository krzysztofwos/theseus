//! Crate scaffolding: the one-time skeleton of a service crate, rendered from
//! the model.
//!
//! Code generation keeps rewriting a crate's generated contract. Scaffolding
//! instead lays down the authored leaves a new crate starts from. A library
//! service crate (hosting services, no inbound) gets a manifest, module wiring,
//! and an empty adapter. A binary application crate (hosting an inbound adapter,
//! no service of its own) gets a manifest with a binary target and a composition
//! root. These are authored after they land, so an adopter writes only the files
//! that are absent and never clobbers work.

use crate::{
    codegen::{GeneratedFile, pascal_case},
    model::{CrateNode, Inbound, Model, Service, TypeShape},
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

    // A crate that hosts an inbound adapter but no service of its own is a binary
    // application: a command-line entry point that drives a service from another
    // crate. Its skeleton is a manifest with a binary target and a composition root.
    for inbound in &model.inbounds {
        if seen.contains(&inbound.crate_name.as_str()) {
            continue;
        }
        seen.push(&inbound.crate_name);
        if model
            .services
            .iter()
            .any(|s| s.crate_name == inbound.crate_name)
        {
            continue;
        }
        let (Some(node), Some(service)) = (
            model.crate_named(&inbound.crate_name),
            model.service_named(&inbound.service),
        ) else {
            continue;
        };
        let dir = &node.dir;
        files.push(GeneratedFile {
            path: format!("rust/{dir}/Cargo.toml"),
            contents: binary_cargo_toml(node, inbound, service, model),
        });
        files.push(GeneratedFile {
            path: format!("rust/{dir}/src/main.rs"),
            contents: binary_main(service),
        });
    }
    files
}

/// Render a binary application's manifest: a binary target named for the inbound,
/// `anyhow` and the command-surface dependency, and a path dependency on the
/// service's crate.
fn binary_cargo_toml(
    node: &CrateNode,
    inbound: &Inbound,
    service: &Service,
    model: &Model,
) -> String {
    // The manifest carries every dependency the model declares for the crate —
    // the service it drives, and whatever else the composition reaches — so the
    // dependency check verifies the scaffold it wrote.
    let mut deps: Vec<&str> = node.depends_on.iter().map(String::as_str).collect();
    if !deps.contains(&service.crate_name.as_str()) {
        deps.push(&service.crate_name);
    }
    let path_block: String = deps
        .iter()
        .map(|dep| {
            let dir = model
                .crate_named(dep)
                .map(|n| n.dir.as_str())
                .unwrap_or(dep);
            format!("{dep} = {{ path = \"../{dir}\" }}\n")
        })
        .collect();
    format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"A standalone command-line interface to the {} service\"\n\n\
         [[bin]]\nname = \"{}\"\npath = \"src/main.rs\"\n\n\
         [dependencies]\nanyhow = {{ workspace = true }}\nclap = {{ workspace = true }}\ntokio = {{ workspace = true }}\n\n{path_block}",
        node.name, service.name, inbound.name,
    )
}

/// Render the binary's composition root. A portless service backs the contract
/// with its adapter and drives the generated dispatch. A ported service leaves
/// the composition authored: the skeleton compiles, and the wiring — adapters
/// into a `Ctx` or a `Standalone` — is the leaf the architecture names.
fn binary_main(service: &Service) -> String {
    let module = service.crate_name.replace('-', "_");
    let header = format!(
        "//! A standalone command-line interface to the {} service.\n//!\n\
         //! The command surface, the parsed invocation, and the dispatch are generated\n\
         //! from the service's operations. This entry point is the authored composition\n\
         //! root: it backs the contract with adapters and drives it from the command line.\n\n\
         mod generated;\n\n\
         #[tokio::main(flavor = \"current_thread\")]\n\
         async fn main() -> anyhow::Result<()> {{\n",
        service.name,
    );
    let body = if service.outbound.is_empty() {
        let adapter = adapter_name(service);
        let var = service.name.to_lowercase();
        format!(
            "    let {var} = {module}::{adapter};\n    \
             let matches = generated::command().get_matches();\n    \
             generated::dispatch(&{var}, generated::Invocation::from_matches(&matches)?).await\n"
        )
    } else {
        format!(
            "    // The service carries ports: author its adapters in `{module}`, wire\n    \
             // them into a composition root, and drive the generated dispatch.\n    \
             todo!(\"compose the {} service's adapters\")\n",
            service.name,
        )
    };
    format!("{header}{body}}}\n")
}

/// Render the crate manifest: `anyhow` and `async-trait` for the generated
/// contract, then a path dependency for each modeled crate dependency.
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
    // A ported service's composition roots carry the engine's `Model`, so its
    // crate depends on the engine through the workspace, and the workspace
    // points the name at wherever the engine lives.
    let engine = if services.iter().any(|s| !s.outbound.is_empty()) {
        "theseus-modeling = { workspace = true }\n"
    } else {
        ""
    };
    format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\ndescription = \"{}\"\n\n\
         [dependencies]\nanyhow = {{ workspace = true }}\nasync-trait = {{ workspace = true }}\n{engine}{path_block}",
        node.name,
        description(services),
    )
}

/// Render the library root: the module wiring and the public re-exports.
fn lib_rs(services: &[&Service], model: &Model) -> String {
    let mut contract: Vec<String> = services.iter().map(|s| trait_name(s)).collect();
    contract.extend(request_structs(services, model));
    // The boundary error types cross the crate line with the contract: a wire
    // adapter downcasts them to map an outcome, so the library exports them.
    contract.push("Refused".to_string());
    contract.push("Unimplemented".to_string());
    // A ported service's composition surface crosses the crate line too: the
    // borrowed and owned roots, and the port traits an adapter implements.
    for service in services.iter().filter(|s| !s.outbound.is_empty()) {
        contract.push("Ctx".to_string());
        contract.push("Standalone".to_string());
        for port in service.outbound.iter().filter(|p| p.target.is_none()) {
            contract.push(pascal_case(&port.name));
        }
    }
    // A portless service authors an adapter struct; a ported one authors its
    // handlers on `Ctx`, so only the portless adapters cross the crate line.
    let adapters: Vec<String> = services
        .iter()
        .filter(|s| s.outbound.is_empty())
        .map(|s| adapter_name(s))
        .collect();
    let adapter_uses = if adapters.is_empty() {
        String::new()
    } else {
        format!("{}\n", use_list("service", &adapters))
    };
    format!(
        "//! {}\n\nmod generated;\nmod service;\n\n{}\n{adapter_uses}",
        description(services),
        use_list("generated", &contract),
    )
}

/// Render the authored impl: an empty implementation of each service contract,
/// whose methods fall through to their `unimplemented` defaults until authored.
/// A portless service implements on its own adapter struct. A ported one
/// implements on the generated `Ctx`, the borrowed root its handlers reach
/// their ports through.
fn service_rs(services: &[&Service]) -> String {
    let mut traits: Vec<String> = services.iter().map(|s| trait_name(s)).collect();
    if services.iter().any(|s| !s.outbound.is_empty()) {
        traits.push("Ctx".to_string());
    }
    let blocks: Vec<String> = services
        .iter()
        .map(|s| {
            if s.outbound.is_empty() {
                let adapter = adapter_name(s);
                format!(
                    "/// The {} adapter.\npub struct {adapter};\n\n#[async_trait::async_trait]\nimpl {} for {adapter} {{}}\n",
                    s.name,
                    trait_name(s),
                )
            } else {
                format!(
                    "#[async_trait::async_trait]\nimpl {} for Ctx<'_> {{}}\n",
                    trait_name(s),
                )
            }
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
        assert!(lib.contains("pub use generated::{CalculatorService, Operands, Refused, Unimplemented};"));
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
    fn scaffolds_a_binary_adapter_crate() {
        let model = Model::new("App")
            .crate_node("calc", "calc", 0, &[])
            .crate_node("calc-cli", "calc-cli", 1, &["calc"])
            .service(Service::new("Calculator").crate_name("calc"))
            .inbound("calculator", Transport::Cli, "Calculator", "calc-cli");
        let files = scaffold_files(&model);

        let cargo = file(&files, "calc-cli/Cargo.toml");
        assert!(cargo.contains("[[bin]]"));
        assert!(cargo.contains("name = \"calculator\""));
        assert!(cargo.contains("clap = { workspace = true }"));
        assert!(cargo.contains("calc = { path = \"../calc\" }"));

        let main = file(&files, "calc-cli/src/main.rs");
        assert!(main.contains("let calculator = calc::Calculator;"));
        assert!(main.contains("generated::dispatch(&calculator,"));
        // A binary crate has no lib.rs or service.rs of its own.
        assert!(
            !files
                .iter()
                .any(|f| f.path.ends_with("calc-cli/src/lib.rs"))
        );
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
