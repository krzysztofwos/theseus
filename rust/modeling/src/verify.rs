//! Self-verification: does the workspace on disk conform to its self-model?
//!
//! Nine checks, all derived from the same [`Model`]:
//!
//!   1. **Required dependencies** — every dependency the model declares is
//!      actually present in the crates' `Cargo.toml`. Realized as a functor
//!      from the spec crate-graph into the extracted one. A missing edge fails
//!      the functor.
//!   2. **Dependency direction** — every real dependency points from a higher
//!      architectural layer to a strictly lower one. Realized as a layering
//!      functor into a layer preorder. A violation has no monotone image.
//!   3. **Type references** — every type a service or port names resolves to a
//!      builtin or a defined type, so no operation points at a phantom type.
//!   4. **Port targets** — every service-targeting port names a service the
//!      model defines, so no port is bound to a phantom service.
//!   5. **Inbound services** — every inbound adapter drives a service the model
//!      defines, so no adapter is bound to a phantom service.
//!   6. **Client services** — every client adapter reaches a service the model
//!      defines, the mirror of the inbound check.
//!   7. **Generated drift** — model-derived files on disk match a fresh render.
//!   8. **Implementation coverage** — every operation has an authored handler.
//!      The service trait defaults each method to `unimplemented`, so this check
//!      holds the gate the compiler once did.
//!   9. **Flow conformance** — every authored handler reaches exactly the ports
//!      its operation's `uses` edges declare. Realized as a functor from the
//!      declared flow graph into the one extracted from the handlers.
//!
//! Checks 1 and 2 read the real manifests, so they degrade gracefully under
//! refactoring: rename a crate in the model and its manifest together and the
//! functor still verifies.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use theseus_kernel::{Category, CategoryBuilder, FunctorBuilder};

use crate::{
    codegen::GeneratedFile,
    label::container_inner,
    model::{Model, TypeShape},
};

/// The outcome of one verification check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// The full self-verification report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyReport {
    pub checks: Vec<Check>,
    pub conformant: bool,
}

impl VerifyReport {
    fn new() -> Self {
        Self {
            checks: Vec::new(),
            conformant: true,
        }
    }

    fn record(&mut self, name: &str, result: Result<String, String>) {
        let (ok, detail) = match result {
            Ok(detail) => (true, detail),
            Err(detail) => (false, detail),
        };
        if !ok {
            self.conformant = false;
        }
        self.checks.push(Check {
            name: name.to_string(),
            ok,
            detail,
        });
    }

    /// Render the report as human-readable lines, one per check plus a verdict.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for check in &self.checks {
            let mark = if check.ok { "✓" } else { "✗" };
            out.push_str(&format!(
                "  {mark} {}\n      {}\n",
                check.name, check.detail
            ));
        }
        out.push_str(if self.conformant {
            "conformant: workspace matches its self-model"
        } else {
            "NOT conformant: workspace diverges from its self-model"
        });
        out
    }
}

/// Run every self-verification check.
///
/// `workspace_root` is the repository root (the directory holding `rust/`).
/// `generated` is the set of files the adopter expects on disk, each compared
/// against a fresh render for the drift gate. `impls` pairs each service name with
/// the workspace-relative authored file implementing its trait, read for the
/// coverage check.
pub fn verify(
    model: &Model,
    workspace_root: &Path,
    generated: &[GeneratedFile],
    impls: &[(String, String)],
) -> VerifyReport {
    let mut report = VerifyReport::new();

    match extract_impl_category(model, workspace_root) {
        Ok(impl_cat) => {
            let spec = build_spec_category(model);
            report.record(
                "crate graph: required dependencies present",
                check_required_edges(&spec, &impl_cat),
            );
            report.record(
                "crate graph: dependency direction (layering functor)",
                check_layering(model, &impl_cat),
            );
        }
        Err(error) => report.record("crate graph: read manifests", Err(error)),
    }

    report.record(
        "types: every reference resolves to a definition",
        check_type_references(model),
    );

    report.record(
        "ports: every service-targeting port resolves to a service",
        check_port_targets(model),
    );

    report.record(
        "inbounds: every inbound adapter drives a defined service",
        check_inbound_services(model),
    );

    report.record(
        "clients: every client adapter reaches a defined service",
        check_client_services(model),
    );

    report.record(
        "generated code: in sync with model (drift gate)",
        check_generated_in_sync(generated, workspace_root),
    );

    report.record(
        "operations: every operation has an authored handler",
        check_implementation_coverage(model, workspace_root, impls),
    );

    report.record(
        "flow: every handler reaches exactly its declared ports",
        check_flow_conformance(model, workspace_root, impls),
    );

    report
}

/// Every authored handler must reach exactly the ports its operation declares
/// through `uses`. The check reads each service's authored impl, extracts the
/// ports each handler touches, and holds the two edge sets against each other —
/// a declared port the handler never reaches and a reached port the operation
/// never declares both fail. An operation still on its `unimplemented` default
/// is coverage's finding, so its declared edges wait for the handler.
fn check_flow_conformance(
    model: &Model,
    root: &Path,
    impls: &[(String, String)],
) -> Result<String, String> {
    flow_conformance(model, |service| {
        let path = impls
            .iter()
            .find(|(name, _)| name == &service.name)
            .map(|(_, path)| path.as_str())
            .ok_or_else(|| format!("no authored impl path for service `{}`", service.name))?;
        fs::read_to_string(root.join(path)).map_err(|e| format!("reading {path}: {e}"))
    })
}

/// The flow check over supplied sources: `source_of` hands each service its
/// authored impl, so the conformance law is testable without a filesystem.
fn flow_conformance<E: std::fmt::Display>(
    model: &Model,
    mut source_of: impl FnMut(&crate::model::Service) -> Result<String, E>,
) -> Result<String, String> {
    let mut objects: Vec<String> = Vec::new();
    let mut declared_edges: Vec<(String, String, String)> = Vec::new();
    let mut extracted_edges: Vec<(String, String, String)> = Vec::new();
    let mut violations: Vec<String> = Vec::new();

    for service in &model.services {
        let ports: BTreeSet<String> = service
            .outbound
            .iter()
            .map(|port| port.name.clone())
            .collect();
        for port in &ports {
            objects.push(port_object(service, port));
        }
        let source = source_of(service).map_err(|e| e.to_string())?;
        let flows = crate::flow::handler_flows(
            &source,
            &crate::coverage::service_trait_name(service),
            &ports,
        )
        .map_err(|e| e.to_string())?;
        for op in &service.operations {
            objects.push(operation_object(service, op));
            let declared: BTreeSet<String> = op.uses.iter().cloned().collect();
            for port in &declared {
                if !ports.contains(port) {
                    violations.push(format!(
                        "operation `{}` uses port `{port}`, which service `{}` does not declare",
                        op.name, service.name
                    ));
                }
            }
            let Some(reached) = flows.get(&op.name) else {
                continue;
            };
            for port in declared.difference(reached) {
                violations.push(format!(
                    "operation `{}` declares port `{port}` but its handler does not reach it",
                    op.name
                ));
            }
            for port in reached.difference(&declared) {
                violations.push(format!(
                    "the handler for `{}` reaches port `{port}`, which the operation does not declare",
                    op.name
                ));
            }
            for port in &declared {
                declared_edges.push((
                    flow_label(&op.name, port),
                    operation_object(service, op),
                    port_object(service, port),
                ));
            }
            for port in reached {
                extracted_edges.push((
                    flow_label(&op.name, port),
                    operation_object(service, op),
                    port_object(service, port),
                ));
            }
        }
    }

    if !violations.is_empty() {
        return Err(violations.join("; "));
    }

    let functor = build_flow_functor(&objects, &declared_edges, &extracted_edges)?;
    Ok(format!(
        "{functor} declared flow edge(s) all realized by their handlers"
    ))
}

/// Verify the flow functor: the declared graph maps into the extracted one,
/// every `uses` edge landing on the reach with the same endpoints. Returns the
/// number of verified edges.
fn build_flow_functor(
    objects: &[String],
    declared: &[(String, String, String)],
    extracted: &[(String, String, String)],
) -> Result<usize, String> {
    let object_refs: Vec<&str> = objects.iter().map(String::as_str).collect();
    let build = |name: &str, doc: &str, edges: &[(String, String, String)]| {
        let edge_refs: Vec<(&str, &str, &str)> = edges
            .iter()
            .map(|(l, s, t)| (l.as_str(), s.as_str(), t.as_str()))
            .collect();
        CategoryBuilder::new(name, doc)
            .objects(&object_refs)
            .morphisms(&edge_refs)
            .build()
            .map_err(|e| e.to_string())
    };
    let declared_cat = build("DeclaredFlow", "modeled uses edges", declared)?;
    let extracted_cat = build("ExtractedFlow", "handler port reaches", extracted)?;

    let object_pairs: Vec<(&str, &str)> = object_refs.iter().map(|o| (*o, *o)).collect();
    let morphism_pairs: Vec<(&str, &str)> = declared
        .iter()
        .map(|(label, _, _)| (label.as_str(), label.as_str()))
        .collect();
    let functor = FunctorBuilder::new("Flow", "declared flow into extracted flow")
        .map_objects(&object_pairs)
        .map_morphisms(&morphism_pairs)
        .build();
    functor
        .verify(&declared_cat, &extracted_cat)
        .map_err(|e| format!("flow functor failed: {e}"))?;
    Ok(morphism_pairs.len())
}

/// Label for an operation object in the flow graph.
fn operation_object(service: &crate::model::Service, op: &crate::model::Operation) -> String {
    format!("operation:{}:{}", service.name, op.name)
}

/// Label for a port object in the flow graph.
fn port_object(service: &crate::model::Service, port: &str) -> String {
    format!("port:{}:{port}", service.name)
}

/// Label for a flow edge between an operation and a port it uses.
fn flow_label(op: &str, port: &str) -> String {
    format!("{op}__uses__{port}")
}

/// Every modeled operation must have an authored handler. An operation left on
/// the service trait's `unimplemented` default is reported here, moving the gate
/// on missing behavior from the compiler to verification. Each service's handlers
/// are read from the file `impls` pairs with its name.
fn check_implementation_coverage(
    model: &Model,
    root: &Path,
    impls: &[(String, String)],
) -> Result<String, String> {
    let report = crate::coverage::coverage(model, |service| {
        let path = impls
            .iter()
            .find(|(name, _)| name == &service.name)
            .map(|(_, path)| path.as_str())
            .ok_or_else(|| format!("no authored impl path for service `{}`", service.name))?;
        fs::read_to_string(root.join(path)).map_err(|e| format!("reading {path}: {e}"))
    })
    .map_err(|e| e.to_string())?;
    if report.unimplemented.is_empty() {
        Ok(format!("{} operation(s) all implemented", report.total))
    } else {
        let names: Vec<&str> = report
            .unimplemented
            .iter()
            .map(|gap| gap.name.as_str())
            .collect();
        Err(format!(
            "{} of {} operation(s) unimplemented: {}",
            report.unimplemented.len(),
            report.total,
            names.join(", ")
        ))
    }
}

/// Every service-targeting port must name a service the model defines, so a port
/// bound to a phantom service is caught before code generation reaches for its
/// trait.
fn check_port_targets(model: &Model) -> Result<String, String> {
    let services: BTreeSet<&str> = model.services.iter().map(|s| s.name.as_str()).collect();
    let mut bound = 0;
    for port in model
        .services
        .iter()
        .flat_map(|service| service.outbound.iter())
        .chain(model.inbounds.iter().flat_map(|i| i.outbound.iter()))
    {
        if let Some(target) = &port.target {
            bound += 1;
            if !services.contains(target.as_str()) {
                return Err(format!(
                    "port `{}` targets service `{target}`, which the model does not define",
                    port.name
                ));
            }
        }
    }
    Ok(format!("{bound} service-targeting port(s) all resolve"))
}

/// Every client adapter must reach a service the model defines — the mirror of
/// the inbound check.
fn check_client_services(model: &Model) -> Result<String, String> {
    let services: Vec<&str> = model.services.iter().map(|s| s.name.as_str()).collect();
    for client in &model.clients {
        if !services.contains(&client.service.as_str()) {
            return Err(format!(
                "client `{}` reaches service `{}`, which the model does not define",
                client.name, client.service
            ));
        }
    }
    Ok(format!(
        "{} client adapter(s) all reach a defined service",
        model.clients.len()
    ))
}

/// Every inbound adapter must drive a service the model defines, so an adapter
/// bound to a phantom service is caught before code generation reaches for its
/// trait.
fn check_inbound_services(model: &Model) -> Result<String, String> {
    let services: BTreeSet<&str> = model.services.iter().map(|s| s.name.as_str()).collect();
    for inbound in &model.inbounds {
        if !services.contains(inbound.service.as_str()) {
            return Err(format!(
                "inbound `{}` drives service `{}`, which the model does not define",
                inbound.name, inbound.service
            ));
        }
    }
    Ok(format!(
        "{} inbound adapter(s) all drive a defined service",
        model.inbounds.len()
    ))
}

/// Every type a service or port names must resolve: a builtin, an `Option` of a
/// resolvable type, or a defined [`TypeDef`]. Catches a phantom reference — a
/// request or response label with no type behind it.
fn check_type_references(model: &Model) -> Result<String, String> {
    let defined: BTreeSet<&str> = model.types.iter().map(|t| t.name.as_str()).collect();

    let mut referenced: Vec<&str> = Vec::new();
    for op in model.operations() {
        referenced.push(&op.request);
        referenced.push(&op.response);
    }
    for port in model
        .services
        .iter()
        .flat_map(|service| service.outbound.iter())
        .chain(model.inbounds.iter().flat_map(|i| i.outbound.iter()))
    {
        for method in &port.methods {
            referenced.push(&method.request);
            referenced.push(&method.response);
        }
    }
    for type_def in &model.types {
        match &type_def.shape {
            TypeShape::Struct(fields) => {
                referenced.extend(fields.iter().map(|field| field.ty.as_str()))
            }
            TypeShape::Newtype(inner) => referenced.push(inner),
            TypeShape::Enum { variants, .. } => {
                for variant in variants {
                    referenced.extend(variant.fields.iter().map(|field| field.ty.as_str()));
                }
            }
            TypeShape::Foreign(_) => {}
        }
    }

    let dangling: BTreeSet<&str> = referenced
        .into_iter()
        .filter(|label| !type_label_resolves(label, &defined))
        .collect();

    if dangling.is_empty() {
        Ok(format!(
            "{} defined type(s); every reference resolves",
            defined.len()
        ))
    } else {
        Err(format!(
            "type reference(s) with no definition: {}",
            dangling.into_iter().collect::<Vec<_>>().join(", ")
        ))
    }
}

/// Does a type label resolve? A builtin, an `Option<T>` or `Vec<T>` of a
/// resolvable `T`, or a defined type name.
fn type_label_resolves(label: &str, defined: &BTreeSet<&str>) -> bool {
    if is_builtin_type(label) {
        return true;
    }
    if let Some(inner) = container_inner(label) {
        return type_label_resolves(inner, defined);
    }
    defined.contains(label)
}

/// The type labels a model may use without defining them: the unit marker, and
/// the Rust scalar primitives a command-line argument can parse into.
fn is_builtin_type(label: &str) -> bool {
    matches!(
        label,
        "Empty"
            | "String"
            | "bool"
            | "char"
            | "f32"
            | "f64"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    )
}

/// Every expected generated file on disk must match the rendered contents.
fn check_generated_in_sync(generated: &[GeneratedFile], root: &Path) -> Result<String, String> {
    for file in generated {
        let path = root.join(&file.path);
        let on_disk = fs::read_to_string(&path)
            .map_err(|e| format!("reading {}: {e}; run `theseus generate`", file.path))?;
        if on_disk != file.contents {
            return Err(format!("`{}` is stale; run `theseus generate`", file.path));
        }
    }
    Ok(format!(
        "{} generated file(s) match the model",
        generated.len()
    ))
}

/// Label for a dependency edge between two crates.
fn dep_label(source: &str, target: &str) -> String {
    format!("{source}__depends_on__{target}")
}

fn manifest_path(root: &Path, dir: &str) -> PathBuf {
    root.join("rust").join(dir).join("Cargo.toml")
}

/// Does `manifest` declare a path dependency on the crate living in `../<dir>`?
/// Only the `[dependencies]` table counts: the layering law governs the
/// delivered coupling, and a test-only dependency crosses layers freely — a
/// client's round trip drives the server it mirrors.
fn manifest_references_dir(manifest: &str, dir: &str) -> bool {
    delivered_dependencies(manifest).contains(&format!("\"../{dir}\""))
}

/// The `[dependencies]` table of a manifest, cut at the section boundaries.
fn delivered_dependencies(manifest: &str) -> String {
    let mut inside = false;
    let mut section = String::new();
    for line in manifest.lines() {
        let header = line.trim();
        if header.starts_with('[') {
            inside = header == "[dependencies]";
            continue;
        }
        if inside {
            section.push_str(line);
            section.push('\n');
        }
    }
    section
}

/// The intended crate graph, straight from the model.
fn build_spec_category(model: &Model) -> Category {
    let names: Vec<&str> = model.crates.iter().map(|c| c.name.as_str()).collect();
    let edges: Vec<(String, String, String)> = model
        .crates
        .iter()
        .flat_map(|node| {
            node.depends_on
                .iter()
                .map(move |dep| (dep_label(&node.name, dep), node.name.clone(), dep.clone()))
        })
        .collect();
    let edge_refs: Vec<(&str, &str, &str)> = edges
        .iter()
        .map(|(l, s, t)| (l.as_str(), s.as_str(), t.as_str()))
        .collect();
    CategoryBuilder::new("SelfModelCrates", "modeled crate graph")
        .objects(&names)
        .morphisms(&edge_refs)
        .build()
        .expect("spec crate graph is well-formed")
}

/// The real crate graph, extracted from the manifests on disk. An edge is
/// present iff the source crate's `Cargo.toml` declares a path dependency on the
/// target crate's directory — independent of what the model claims.
fn extract_impl_category(model: &Model, root: &Path) -> Result<Category, String> {
    let names: Vec<&str> = model.crates.iter().map(|c| c.name.as_str()).collect();

    let mut edges: Vec<(String, String, String)> = Vec::new();
    for node in &model.crates {
        let path = manifest_path(root, &node.dir);
        let manifest =
            fs::read_to_string(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        for other in &model.crates {
            if other.name == node.name {
                continue;
            }
            if manifest_references_dir(&manifest, &other.dir) {
                edges.push((
                    dep_label(&node.name, &other.name),
                    node.name.clone(),
                    other.name.clone(),
                ));
            }
        }
    }

    let edge_refs: Vec<(&str, &str, &str)> = edges
        .iter()
        .map(|(l, s, t)| (l.as_str(), s.as_str(), t.as_str()))
        .collect();
    CategoryBuilder::new("WorkspaceCrates", "extracted crate graph")
        .objects(&names)
        .morphisms(&edge_refs)
        .build()
        .map_err(|e| e.to_string())
}

/// Verify a functor from the spec graph into the extracted graph: every modeled
/// dependency edge must exist on disk with the same endpoints.
fn check_required_edges(spec: &Category, impl_cat: &Category) -> Result<String, String> {
    let object_pairs: Vec<(&str, &str)> = spec
        .objects()
        .iter()
        .map(|o| (o.as_str(), o.as_str()))
        .collect();
    let morphism_pairs: Vec<(&str, &str)> = spec
        .morphisms()
        .values()
        .filter(|m| !m.is_identity())
        .map(|m| (m.label().as_str(), m.label().as_str()))
        .collect();

    let functor = FunctorBuilder::new("RequiredDeps", "required dependency edges")
        .map_objects(&object_pairs)
        .map_morphisms(&morphism_pairs)
        .build();
    functor
        .verify(spec, impl_cat)
        .map_err(|e| format!("modeled dependency not found on disk: {e}"))?;
    Ok(format!(
        "{} modeled dependency edge(s) all present",
        morphism_pairs.len()
    ))
}

/// Build the layer preorder `L0 <- L1 <- ... <- Lmax`, with a step morphism for
/// every strictly-descending pair.
fn build_levels(max_layer: u32) -> Category {
    let objects: Vec<String> = (0..=max_layer).map(|i| format!("L{i}")).collect();
    let object_refs: Vec<&str> = objects.iter().map(String::as_str).collect();

    let mut steps: Vec<(String, String, String)> = Vec::new();
    for high in 0..=max_layer {
        for low in 0..high {
            steps.push((
                format!("step_{high}_to_{low}"),
                format!("L{high}"),
                format!("L{low}"),
            ));
        }
    }
    let step_refs: Vec<(&str, &str, &str)> = steps
        .iter()
        .map(|(l, s, t)| (l.as_str(), s.as_str(), t.as_str()))
        .collect();

    CategoryBuilder::new("Levels", "layer preorder")
        .objects(&object_refs)
        .morphisms(&step_refs)
        .build()
        .expect("levels category is well-formed")
}

/// Verify a layering functor from the extracted graph into the layer preorder.
/// Each crate maps to its layer. Each dependency maps to the descending step
/// between layers. A dependency that does not strictly descend has no image and
/// fails the check.
fn check_layering(model: &Model, impl_cat: &Category) -> Result<String, String> {
    let layer_of = |name: &str| model.crate_named(name).map(|c| c.layer);
    let max_layer = model.crates.iter().map(|c| c.layer).max().unwrap_or(0);
    let levels = build_levels(max_layer);

    let mut object_pairs: Vec<(String, String)> = Vec::new();
    for object in impl_cat.objects() {
        let layer = layer_of(object.as_str())
            .ok_or_else(|| format!("crate `{object}` has no modeled layer"))?;
        object_pairs.push((object.as_str().to_string(), format!("L{layer}")));
    }

    let mut morphism_pairs: Vec<(String, String)> = Vec::new();
    for morphism in impl_cat.morphisms().values() {
        if morphism.is_identity() {
            continue;
        }
        let source = morphism.source().as_str();
        let target = morphism.target().as_str();
        let source_layer = layer_of(source).expect("source layer known");
        let target_layer = layer_of(target).expect("target layer known");
        if source_layer <= target_layer {
            return Err(format!(
                "layering violation: `{source}` (L{source_layer}) depends on `{target}` (L{target_layer}); \
                 dependencies must point to a strictly lower layer"
            ));
        }
        morphism_pairs.push((
            morphism.label().as_str().to_string(),
            format!("step_{source_layer}_to_{target_layer}"),
        ));
    }

    let object_refs: Vec<(&str, &str)> = object_pairs
        .iter()
        .map(|(s, t)| (s.as_str(), t.as_str()))
        .collect();
    let morphism_refs: Vec<(&str, &str)> = morphism_pairs
        .iter()
        .map(|(s, t)| (s.as_str(), t.as_str()))
        .collect();

    let functor = FunctorBuilder::new("Layering", "crate layering")
        .map_objects(&object_refs)
        .map_morphisms(&morphism_refs)
        .build();
    functor
        .verify(impl_cat, &levels)
        .map_err(|e| format!("layering functor failed: {e}"))?;
    Ok(format!(
        "{} dependency edge(s) all descend through the layer preorder",
        morphism_pairs.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{CrateNode, Service};

    fn layered_model() -> Model {
        Model {
            name: "Sample".to_string(),
            crates: vec![
                CrateNode {
                    name: "core".to_string(),
                    dir: "core".to_string(),
                    layer: 0,
                    depends_on: vec![],
                },
                CrateNode {
                    name: "app".to_string(),
                    dir: "app".to_string(),
                    layer: 1,
                    depends_on: vec!["core".to_string()],
                },
            ],
            types: vec![],
            services: vec![],
            inbounds: vec![],
            clients: vec![],
        }
    }

    #[test]
    fn required_edge_absence_is_detected() {
        // The spec demands `app -> core`. The extracted graph lacks it.
        let spec = CategoryBuilder::new("Spec", "spec")
            .objects(&["app", "core"])
            .morphism(&dep_label("app", "core"), "app", "core")
            .build()
            .unwrap();
        let impl_cat = CategoryBuilder::new("Impl", "impl")
            .objects(&["app", "core"])
            .build()
            .unwrap();
        assert!(check_required_edges(&spec, &impl_cat).is_err());
    }

    #[test]
    fn layering_rejects_an_upward_edge() {
        // An impl graph with an illegal L0 -> L1 dependency.
        let bad = CategoryBuilder::new("Bad", "bad")
            .objects(&["core", "app"])
            .morphism(&dep_label("core", "app"), "core", "app")
            .build()
            .unwrap();
        assert!(check_layering(&layered_model(), &bad).is_err());
    }

    #[test]
    fn dangling_type_reference_is_detected() {
        // The operation responds with `Ghost`, which no type defines.
        let model = Model::new("Sample")
            .service(Service::new("Sample").operation("op", "", "Empty", "Ghost"));
        assert!(check_type_references(&model).is_err());
    }

    #[test]
    fn a_test_only_dependency_stays_outside_the_layering_law() {
        let manifest = concat!(
            "[package]\nname = \"x\"\n\n",
            "[dependencies]\na = { path = \"../a\" }\n\n",
            "[dev-dependencies]\nb = { path = \"../b\" }\n",
        );
        assert!(manifest_references_dir(manifest, "a"));
        assert!(!manifest_references_dir(manifest, "b"));
    }

    fn flow_model() -> Model {
        Model::new("Sample").service(
            Service::new("Sample")
                .crate_name("sample")
                .operation("run", "Run.", "Empty", "String")
                .uses(&["toolchain"])
                .operation("describe", "Describe.", "Empty", "String")
                .port(
                    crate::model::Port::new("toolchain", "Checks the build.")
                        .method("check", "Check.", "Empty", "String"),
                ),
        )
    }

    fn from(source: &str) -> impl FnMut(&Service) -> Result<String, String> + '_ {
        move |_| Ok(source.to_string())
    }

    #[test]
    fn a_handler_matching_its_declared_flow_conforms() {
        let source = r#"
            impl SampleService for Ctx {
                async fn run(&self) -> anyhow::Result<String> { self.toolchain.check().await }
                async fn describe(&self) -> anyhow::Result<String> { Ok("sample".to_string()) }
            }
        "#;
        let detail = flow_conformance(&flow_model(), from(source)).unwrap();
        assert!(detail.contains("1 declared flow edge(s)"), "{detail}");
    }

    #[test]
    fn a_declared_port_the_handler_never_reaches_fails() {
        let source = r#"
            impl SampleService for Ctx {
                async fn run(&self) -> anyhow::Result<String> { Ok("skipped".to_string()) }
            }
        "#;
        let error = flow_conformance(&flow_model(), from(source)).unwrap_err();
        assert!(
            error.contains("declares port `toolchain` but its handler does not reach it"),
            "{error}"
        );
    }

    #[test]
    fn a_reached_port_the_operation_never_declares_fails() {
        let source = r#"
            impl SampleService for Ctx {
                async fn describe(&self) -> anyhow::Result<String> { self.toolchain.check().await }
            }
        "#;
        let error = flow_conformance(&flow_model(), from(source)).unwrap_err();
        assert!(
            error.contains("reaches port `toolchain`, which the operation does not declare"),
            "{error}"
        );
    }

    #[test]
    fn a_uses_edge_naming_an_unknown_port_fails() {
        let model = Model::new("Sample").service(
            Service::new("Sample")
                .operation("run", "Run.", "Empty", "String")
                .uses(&["ghost"]),
        );
        let error = flow_conformance(&model, from("")).unwrap_err();
        assert!(
            error.contains("uses port `ghost`, which service `Sample` does not declare"),
            "{error}"
        );
    }

    #[test]
    fn an_unimplemented_handler_leaves_flow_to_the_coverage_check() {
        // No impl block at all: coverage reports the gap, flow stays quiet.
        let detail = flow_conformance(&flow_model(), from("")).unwrap();
        assert!(detail.contains("0 declared flow edge(s)"), "{detail}");
    }

    #[test]
    fn an_inbound_port_with_a_dangling_type_is_detected() {
        let model = Model::new("Sample")
            .service(Service::new("Sample"))
            .inbound("agent", crate::model::Transport::Agent, "Sample", "agent")
            .inbound_port(
                crate::model::Port::new("llm", "Completes one turn.").method(
                    "complete",
                    "Complete one turn.",
                    "Ghost",
                    "String",
                ),
            );
        assert!(check_type_references(&model).is_err());
    }

    #[test]
    fn a_registered_type_resolves() {
        // Registering `Ghost` as a foreign type makes the reference resolve.
        let model = Model::new("Sample")
            .foreign_type("Ghost", "String")
            .service(Service::new("Sample").operation("op", "", "Empty", "Ghost"));
        assert!(check_type_references(&model).is_ok());
    }

    #[test]
    fn a_scalar_primitive_field_resolves() {
        // A struct field typed as a scalar primitive needs no definition.
        let model = Model::new("Sample")
            .struct_type("Operands", &[("a", "f64", "")])
            .service(Service::new("Sample").operation("op", "", "Operands", "Empty"));
        assert!(check_type_references(&model).is_ok());
    }
}
