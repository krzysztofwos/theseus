//! Self-verification: does the workspace on disk conform to its self-model?
//!
//! Five checks, all derived from the same [`Model`]:
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
//!   4. **Generated drift** — model-derived files on disk match a fresh render.
//!   5. **Implementation coverage** — every operation has an authored handler.
//!      The service trait defaults each method to `unimplemented`, so this check
//!      holds the gate the compiler once did.
//!
//! Checks 1 and 2 read the real manifests, so they degrade gracefully under
//! refactoring: rename a crate in the model and its manifest together and the
//! functor still verifies.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use serde::Serialize;
use theseus_kernel::{Category, CategoryBuilder, FunctorBuilder};

use crate::{
    codegen::GeneratedFile,
    model::{Model, TypeShape},
};

/// The outcome of one verification check.
#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

/// The full self-verification report.
#[derive(Debug, Clone, Serialize)]
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
        "generated code: in sync with model (drift gate)",
        check_generated_in_sync(generated, workspace_root),
    );

    report.record(
        "operations: every operation has an authored handler",
        check_implementation_coverage(model, workspace_root, impls),
    );

    report
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
    for service in &model.services {
        for port in &service.outbound {
            for method in &port.methods {
                referenced.push(&method.request);
                referenced.push(&method.response);
            }
        }
    }
    for type_def in &model.types {
        match &type_def.shape {
            TypeShape::Struct(fields) => {
                referenced.extend(fields.iter().map(|field| field.ty.as_str()))
            }
            TypeShape::Newtype(inner) => referenced.push(inner),
            TypeShape::Enum(_) | TypeShape::Foreign(_) => {}
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

/// The element type of an `Option<…>` or `Vec<…>` label, when the label is one.
fn container_inner(label: &str) -> Option<&str> {
    label
        .strip_prefix("Option<")
        .or_else(|| label.strip_prefix("Vec<"))
        .and_then(|rest| rest.strip_suffix('>'))
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
fn manifest_references_dir(manifest: &str, dir: &str) -> bool {
    manifest.contains(&format!("\"../{dir}\""))
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
    use crate::model::{CrateNode, Service, Transport};

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
            .service(Service::new("Sample", Transport::Cli).operation("op", "", "Empty", "Ghost"));
        assert!(check_type_references(&model).is_err());
    }

    #[test]
    fn a_registered_type_resolves() {
        // Registering `Ghost` as a foreign type makes the reference resolve.
        let model = Model::new("Sample")
            .foreign_type("Ghost", "String")
            .service(Service::new("Sample", Transport::Cli).operation("op", "", "Empty", "Ghost"));
        assert!(check_type_references(&model).is_ok());
    }

    #[test]
    fn a_scalar_primitive_field_resolves() {
        // A struct field typed as a scalar primitive needs no definition.
        let model = Model::new("Sample")
            .struct_type("Operands", &[("a", "f64", "")])
            .service(
                Service::new("Sample", Transport::Cli).operation("op", "", "Operands", "Empty"),
            );
        assert!(check_type_references(&model).is_ok());
    }
}
