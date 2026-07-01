//! The write side of the agent protocol.
//!
//! A patch resolves and checks each edit against the current model, refusing an
//! edit the model cannot accept. Every refusal carries a coded [`Diagnostic`]
//! with a repair shape, so an agent knows what went wrong and what to do next. An
//! accepted patch returns the edited model. The adapter reprojects it (the
//! self-model source and the generated scaffolding) from there.
//!
//! [`apply_edit`] is the one entry point and [`Edit`] is the edit vocabulary: four
//! verbs — add, remove, rename, set — over the handles
//! [`query`](crate::query) mints. A handle resolves to a typed
//! [`Target`](crate::path::Target), and the edit acts on the node it names.

use serde::Serialize;

use crate::{
    hash::model_hash,
    model::{
        CrateNode, Field, Inbound, Method, Model, Operation, Port, Service, Transport, TypeDef,
        TypeShape,
    },
    path::{NodeKind, Target},
};

/// One structured edit to a model: a verb over a handle.
#[derive(Debug, Clone)]
pub enum Edit {
    /// Add a node of `kind`, named `name`, under the node `parent` addresses.
    Add {
        parent: String,
        kind: String,
        name: String,
        attrs: Vec<(String, String)>,
    },
    /// Remove the node `target` addresses.
    Remove { target: String },
    /// Rename the node `target` addresses to `to`.
    Rename { target: String, to: String },
    /// Set scalar attributes on the node `target` addresses.
    Set {
        target: String,
        attrs: Vec<(String, String)>,
    },
}

/// A coded reason a patch was refused, paired with a repair shape.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    pub repair: String,
}

/// The result of attempting a patch.
#[derive(Debug, Clone, Serialize)]
pub struct PatchOutcome {
    /// Whether the patch was accepted.
    pub ok: bool,
    /// The model hash the patch was computed against.
    pub base_model_hash: String,
    /// The hash of the edited model. Absent when the patch was refused.
    pub new_model_hash: Option<String>,
    /// A human-readable summary of the change.
    pub diff: Vec<String>,
    /// Reasons for refusal. Empty on success.
    pub diagnostics: Vec<Diagnostic>,
}

impl PatchOutcome {
    fn refused(base: String, diagnostics: Vec<Diagnostic>) -> Self {
        Self {
            ok: false,
            base_model_hash: base,
            new_model_hash: None,
            diff: Vec::new(),
            diagnostics,
        }
    }
}

/// Attempt one edit against the current model.
///
/// On success, returns the accepted outcome and the edited model. On refusal,
/// returns the outcome with diagnostics and no model.
pub fn apply_edit(current: &Model, edit: &Edit) -> (PatchOutcome, Option<Model>) {
    apply_edits(current, std::slice::from_ref(edit))
}

/// Attempt a sequence of edits, applying each to the running result. The first
/// refusal stops the sequence and writes nothing. On success the accepted outcome
/// carries every edit's diff.
pub fn apply_edits(current: &Model, edits: &[Edit]) -> (PatchOutcome, Option<Model>) {
    let base = model_hash(current);
    let mut model = current.clone();
    let mut diff = Vec::new();
    for edit in edits {
        match plan(&model, edit) {
            Err(diagnostics) => return (PatchOutcome::refused(base, diagnostics), None),
            Ok(plan) => {
                diff.extend(describe(&plan));
                model = apply(&model, &plan);
            }
        }
    }

    let outcome = PatchOutcome {
        ok: true,
        base_model_hash: base,
        new_model_hash: Some(model_hash(&model)),
        diff,
        diagnostics: Vec::new(),
    };
    (outcome, Some(model))
}

/// A validated, resolved edit, ready to apply.
enum Plan {
    Add {
        parent: Target,
        kind: NodeKind,
        name: String,
        attrs: Vec<(String, String)>,
    },
    Remove {
        target: Target,
    },
    Rename {
        target: Target,
        to: String,
    },
    Set {
        target: Target,
        attrs: Vec<(String, String)>,
    },
}

/// Resolve and check an edit, yielding a [`Plan`] or the reasons it was refused.
fn plan(model: &Model, edit: &Edit) -> Result<Plan, Vec<Diagnostic>> {
    // Crate, dependency, and service additions operate on the crate graph or
    // create a service. Every other edit needs a service to act on or within.
    let crate_graph_edit = matches!(edit, Edit::Add { kind, .. } if matches!(kind.as_str(), "crate" | "dep" | "service"));
    if model.services.is_empty() && !crate_graph_edit {
        return Err(vec![diagnostic(
            "PATCH004",
            "model has no service to edit",
            "add a service before editing within it",
        )]);
    }
    match edit {
        Edit::Add {
            parent,
            kind,
            name,
            attrs,
        } => plan_add(model, parent, kind, name, attrs),
        Edit::Remove { target } => plan_remove(model, target),
        Edit::Rename { target, to } => plan_rename(model, target, to),
        Edit::Set { target, attrs } => plan_set(model, target, attrs),
    }
}

fn plan_add(
    model: &Model,
    parent: &str,
    kind: &str,
    name: &str,
    attrs: &[(String, String)],
) -> Result<Plan, Vec<Diagnostic>> {
    let parent = resolve(model, parent)?;
    let kind = NodeKind::parse(kind).ok_or_else(|| {
        vec![diagnostic(
            "PATCH002",
            format!("unknown node kind `{kind}`"),
            "kind is one of: service, operation, type, port, method, field, variant",
        )]
    })?;
    if name.trim().is_empty() {
        return Err(vec![diagnostic(
            "PATCH003",
            "node name must be non-empty",
            "pass a name via --name <name>",
        )]);
    }

    match kind {
        NodeKind::Crate => {
            under_root(&parent, kind)?;
            free(model.crate_named(name).is_some(), "crate", name)?;
            allow_keys(attrs, &["dir", "layer"])?;
            required(attrs, "dir")?;
            parse_layer(required(attrs, "layer")?).map_err(layer_refused)?;
        }
        NodeKind::Dep => {
            let crate_name = parent_crate(model, &parent)?;
            free(
                dep_exists(model, crate_name, name),
                "dependency",
                &format!("{crate_name}.{name}"),
            )?;
            allow_keys(attrs, &[])?;
        }
        NodeKind::Service => {
            under_root(&parent, kind)?;
            free(service_exists(model, name), "service", name)?;
            allow_keys(attrs, &["crate"])?;
            required(attrs, "crate")?;
        }
        NodeKind::Inbound => {
            under_root(&parent, kind)?;
            free(inbound_exists(model, name), "inbound", name)?;
            allow_keys(attrs, &["transport", "service", "crate"])?;
            parse_transport(required(attrs, "transport")?).map_err(transport_refused)?;
            required(attrs, "service")?;
            required(attrs, "crate")?;
        }
        NodeKind::Operation => {
            attaches_to_service(model, &parent)?;
            free(model.operation(name).is_some(), "operation", name)?;
            allow_keys(attrs, &["summary", "request", "response"])?;
        }
        NodeKind::Type => {
            under_root(&parent, kind)?;
            free(model.type_def(name).is_some(), "type", name)?;
            allow_keys(attrs, &["shape"])?;
            let shape = required(attrs, "shape")?;
            parse_shape(shape).map_err(shape_refused)?;
        }
        NodeKind::Port => {
            attaches_to_service(model, &parent)?;
            free(port_exists(model, name), "port", name)?;
            allow_keys(attrs, &["summary", "target"])?;
        }
        NodeKind::Method => {
            let port = parent_port(model, &parent)?;
            free(
                method_of(model, port, name).is_some(),
                "method",
                &format!("{port}.{name}"),
            )?;
            allow_keys(attrs, &["summary", "request", "response"])?;
        }
        NodeKind::Field => {
            let ty = parent_struct(model, &parent)?;
            free(
                field_of(model, ty, name).is_some(),
                "field",
                &format!("{ty}.{name}"),
            )?;
            allow_keys(attrs, &["ty", "doc"])?;
            required(attrs, "ty")?;
        }
        NodeKind::Variant => {
            let ty = parent_enum(model, &parent)?;
            free(
                variant_exists(model, ty, name),
                "variant",
                &format!("{ty}.{name}"),
            )?;
            allow_keys(attrs, &[])?;
        }
    }

    Ok(Plan::Add {
        parent,
        kind,
        name: name.to_string(),
        attrs: attrs.to_vec(),
    })
}

fn plan_remove(model: &Model, target: &str) -> Result<Plan, Vec<Diagnostic>> {
    let target = resolve(model, target)?;
    reject_root(&target)?;
    present(model, &target)?;
    if let Target::Type(name) = &target
        && type_referenced(model, name)
    {
        return Err(vec![diagnostic(
            "PATCH009",
            format!("type `{name}` is still referenced"),
            "remove or rename the references first",
        )]);
    }
    Ok(Plan::Remove { target })
}

fn plan_rename(model: &Model, target: &str, to: &str) -> Result<Plan, Vec<Diagnostic>> {
    let target = resolve(model, target)?;
    reject_root(&target)?;
    present(model, &target)?;
    if to.trim().is_empty() || sibling_taken(model, &target, to) {
        return Err(vec![diagnostic(
            "PATCH008",
            format!("cannot rename to `{to}`: name is empty or already taken"),
            "choose an unused name for --to",
        )]);
    }
    Ok(Plan::Rename {
        target,
        to: to.to_string(),
    })
}

fn plan_set(
    model: &Model,
    target: &str,
    attrs: &[(String, String)],
) -> Result<Plan, Vec<Diagnostic>> {
    let target = resolve(model, target)?;
    reject_root(&target)?;
    present(model, &target)?;
    let settable = settable_keys(&target);
    if settable.is_empty() {
        return Err(vec![diagnostic(
            "PATCH010",
            format!("a {} has no settable attributes", target.kind_word()),
            "rename it, or remove and re-add it with the new shape",
        )]);
    }
    allow_keys(attrs, settable)?;
    // Validate the values, not just the keys, so an unparseable layer or
    // transport is refused here instead of silently dropped by apply_set.
    match &target {
        Target::Crate(_) => {
            if let Some(layer) = attr(attrs, "layer") {
                parse_layer(layer).map_err(layer_refused)?;
            }
        }
        Target::Inbound(_) => {
            if let Some(transport) = attr(attrs, "transport") {
                parse_transport(transport).map_err(transport_refused)?;
            }
        }
        _ => {}
    }
    Ok(Plan::Set {
        target,
        attrs: attrs.to_vec(),
    })
}

/// Apply a planned edit to a clone of the model.
fn apply(current: &Model, plan: &Plan) -> Model {
    let mut next = current.clone();
    match plan {
        Plan::Add {
            parent,
            kind,
            name,
            attrs,
        } => apply_add(&mut next, parent, *kind, name, attrs),
        Plan::Remove { target } => apply_remove(&mut next, target),
        Plan::Rename { target, to } => apply_rename(&mut next, target, to),
        Plan::Set { target, attrs } => apply_set(&mut next, target, attrs),
    }
    next
}

fn apply_add(
    model: &mut Model,
    parent: &Target,
    kind: NodeKind,
    name: &str,
    attrs: &[(String, String)],
) {
    match kind {
        NodeKind::Crate => model.crates.push(CrateNode {
            name: name.to_string(),
            dir: attr(attrs, "dir").unwrap_or_default().to_string(),
            layer: parse_layer(attr(attrs, "layer").unwrap_or("0"))
                .expect("layer validated during planning"),
            depends_on: Vec::new(),
        }),
        NodeKind::Dep => {
            if let Target::Crate(crate_name) = parent
                && let Some(node) = crate_node_mut(model, crate_name)
            {
                node.depends_on.push(name.to_string());
            }
        }
        NodeKind::Service => model.services.push(Service {
            name: name.to_string(),
            crate_name: attr(attrs, "crate").unwrap_or_default().to_string(),
            operations: Vec::new(),
            outbound: Vec::new(),
        }),
        NodeKind::Inbound => model.inbounds.push(Inbound {
            name: name.to_string(),
            transport: parse_transport(attr(attrs, "transport").unwrap_or("Cli"))
                .expect("transport validated during planning"),
            service: attr(attrs, "service").unwrap_or_default().to_string(),
            crate_name: attr(attrs, "crate").unwrap_or_default().to_string(),
        }),
        NodeKind::Operation => {
            let service =
                target_service_index(model, parent).expect("service resolved in planning");
            model.services[service].operations.push(Operation {
                name: name.to_string(),
                summary: attr(attrs, "summary").unwrap_or_default().to_string(),
                request: attr(attrs, "request").unwrap_or("Empty").to_string(),
                response: attr(attrs, "response").unwrap_or("Empty").to_string(),
            });
        }
        NodeKind::Type => model.types.push(TypeDef {
            name: name.to_string(),
            shape: parse_shape(attr(attrs, "shape").unwrap_or_default())
                .expect("shape validated during planning"),
        }),
        NodeKind::Port => {
            let service =
                target_service_index(model, parent).expect("service resolved in planning");
            model.services[service].outbound.push(Port {
                name: name.to_string(),
                summary: attr(attrs, "summary").unwrap_or_default().to_string(),
                target: attr(attrs, "target").map(str::to_string),
                methods: Vec::new(),
            });
        }
        NodeKind::Method => {
            if let Target::Port(port) = parent
                && let Some(port) = port_mut(model, port)
            {
                port.methods.push(Method {
                    name: name.to_string(),
                    summary: attr(attrs, "summary").unwrap_or_default().to_string(),
                    request: attr(attrs, "request").unwrap_or("Empty").to_string(),
                    response: attr(attrs, "response").unwrap_or("Empty").to_string(),
                });
            }
        }
        NodeKind::Field => {
            if let Target::Type(ty) = parent
                && let Some(fields) = struct_fields_mut(model, ty)
            {
                fields.push(Field {
                    name: name.to_string(),
                    ty: attr(attrs, "ty").unwrap_or_default().to_string(),
                    doc: attr(attrs, "doc").unwrap_or_default().to_string(),
                });
            }
        }
        NodeKind::Variant => {
            if let Target::Type(ty) = parent
                && let Some(variants) = enum_variants_mut(model, ty)
            {
                variants.push(name.to_string());
            }
        }
    }
}

fn apply_remove(model: &mut Model, target: &Target) {
    match target {
        Target::Crate(name) => model.crates.retain(|node| &node.name != name),
        Target::Dep { crate_name, dep } => {
            if let Some(node) = crate_node_mut(model, crate_name) {
                node.depends_on.retain(|d| d != dep);
            }
        }
        Target::Service(name) => model.services.retain(|service| &service.name != name),
        Target::Inbound(name) => model.inbounds.retain(|inbound| &inbound.name != name),
        Target::Operation(name) => {
            for service in &mut model.services {
                service.operations.retain(|op| &op.name != name);
            }
        }
        Target::Type(name) => model.types.retain(|t| &t.name != name),
        Target::Port(name) => {
            for service in &mut model.services {
                service.outbound.retain(|port| &port.name != name);
            }
        }
        Target::Method { port, name } => {
            if let Some(port) = port_mut(model, port) {
                port.methods.retain(|method| &method.name != name);
            }
        }
        Target::Field { ty, name } => {
            if let Some(fields) = struct_fields_mut(model, ty) {
                fields.retain(|field| &field.name != name);
            }
        }
        Target::Variant { ty, name } => {
            if let Some(variants) = enum_variants_mut(model, ty) {
                variants.retain(|variant| variant != name);
            }
        }
        Target::Model => {}
    }
}

fn apply_rename(model: &mut Model, target: &Target, to: &str) {
    match target {
        Target::Crate(name) => {
            for node in &mut model.crates {
                if &node.name == name {
                    node.name = to.to_string();
                }
                // A dependency on the renamed crate follows it.
                for dep in &mut node.depends_on {
                    if dep == name {
                        *dep = to.to_string();
                    }
                }
            }
            // A service placed in the renamed crate follows it.
            for service in &mut model.services {
                if service.crate_name == *name {
                    service.crate_name = to.to_string();
                }
            }
        }
        Target::Inbound(name) => {
            for inbound in &mut model.inbounds {
                if &inbound.name == name {
                    inbound.name = to.to_string();
                }
            }
        }
        Target::Dep { crate_name, dep } => {
            if let Some(node) = crate_node_mut(model, crate_name) {
                for d in &mut node.depends_on {
                    if d == dep {
                        *d = to.to_string();
                    }
                }
            }
        }
        Target::Service(name) => {
            for service in &mut model.services {
                if &service.name == name {
                    service.name = to.to_string();
                }
            }
            // A port bound to the renamed service follows it.
            for port in ports_mut(model) {
                if port.target.as_deref() == Some(name) {
                    port.target = Some(to.to_string());
                }
            }
        }
        Target::Operation(name) => {
            for op in operations_mut(model) {
                if &op.name == name {
                    op.name = to.to_string();
                }
            }
        }
        Target::Type(name) => {
            for t in &mut model.types {
                if &t.name == name {
                    t.name = to.to_string();
                }
            }
            rewrite_type_references(model, name, to);
        }
        Target::Port(name) => {
            for port in ports_mut(model) {
                if &port.name == name {
                    port.name = to.to_string();
                }
            }
        }
        Target::Method { port, name } => {
            if let Some(port) = port_mut(model, port) {
                for method in &mut port.methods {
                    if &method.name == name {
                        method.name = to.to_string();
                    }
                }
            }
        }
        Target::Field { ty, name } => {
            if let Some(fields) = struct_fields_mut(model, ty) {
                for field in fields {
                    if &field.name == name {
                        field.name = to.to_string();
                    }
                }
            }
        }
        Target::Variant { ty, name } => {
            if let Some(variants) = enum_variants_mut(model, ty) {
                for variant in variants {
                    if variant == name {
                        *variant = to.to_string();
                    }
                }
            }
        }
        Target::Model => {}
    }
}

fn apply_set(model: &mut Model, target: &Target, attrs: &[(String, String)]) {
    match target {
        Target::Crate(name) => {
            if let Some(node) = crate_node_mut(model, name) {
                set_if(attrs, "dir", &mut node.dir);
                if let Some(layer) = attr(attrs, "layer").and_then(|v| parse_layer(v).ok()) {
                    node.layer = layer;
                }
            }
        }
        Target::Operation(name) => {
            if let Some(op) = operations_mut(model)
                .into_iter()
                .find(|op| &op.name == name)
            {
                set_if(attrs, "summary", &mut op.summary);
                set_if(attrs, "request", &mut op.request);
                set_if(attrs, "response", &mut op.response);
            }
        }
        Target::Port(name) => {
            if let Some(port) = port_mut(model, name) {
                set_if(attrs, "summary", &mut port.summary);
            }
        }
        Target::Method { port, name } => {
            if let Some(port) = port_mut(model, port)
                && let Some(method) = port.methods.iter_mut().find(|m| &m.name == name)
            {
                set_if(attrs, "summary", &mut method.summary);
                set_if(attrs, "request", &mut method.request);
                set_if(attrs, "response", &mut method.response);
            }
        }
        Target::Field { ty, name } => {
            if let Some(fields) = struct_fields_mut(model, ty)
                && let Some(field) = fields.iter_mut().find(|f| &f.name == name)
            {
                set_if(attrs, "ty", &mut field.ty);
                set_if(attrs, "doc", &mut field.doc);
            }
        }
        Target::Inbound(name) => {
            if let Some(inbound) = model.inbounds.iter_mut().find(|i| &i.name == name) {
                set_if(attrs, "service", &mut inbound.service);
                set_if(attrs, "crate", &mut inbound.crate_name);
                if let Some(transport) =
                    attr(attrs, "transport").and_then(|v| parse_transport(v).ok())
                {
                    inbound.transport = transport;
                }
            }
        }
        Target::Dep { .. }
        | Target::Service(_)
        | Target::Type(_)
        | Target::Variant { .. }
        | Target::Model => {}
    }
}

/// A one-line summary of a planned edit, for the outcome's `diff`.
fn describe(plan: &Plan) -> Vec<String> {
    let line = match plan {
        Plan::Add {
            parent,
            kind,
            name,
            attrs,
        } => {
            let at = add_address(parent, *kind, name);
            match kind {
                NodeKind::Operation | NodeKind::Method => format!(
                    "+ {} {at} ({} => {})",
                    kind.word(),
                    attr(attrs, "request").unwrap_or("Empty"),
                    attr(attrs, "response").unwrap_or("Empty"),
                ),
                NodeKind::Service => {
                    format!("+ service {at} (in {})", attr(attrs, "crate").unwrap_or(""))
                }
                NodeKind::Inbound => format!(
                    "+ inbound {at} ({} driving {} in {})",
                    attr(attrs, "transport").unwrap_or(""),
                    attr(attrs, "service").unwrap_or(""),
                    attr(attrs, "crate").unwrap_or(""),
                ),
                NodeKind::Crate => format!(
                    "+ crate {at} ({} at layer {})",
                    attr(attrs, "dir").unwrap_or(""),
                    attr(attrs, "layer").unwrap_or(""),
                ),
                NodeKind::Type => format!("+ type {at} ({})", attr(attrs, "shape").unwrap_or("")),
                NodeKind::Field => format!("+ field {at}: {}", attr(attrs, "ty").unwrap_or("")),
                NodeKind::Dep | NodeKind::Port | NodeKind::Variant => {
                    format!("+ {} {at}", kind.word())
                }
            }
        }
        Plan::Remove { target } => format!("- {} {}", target.kind_word(), address(target)),
        Plan::Rename { target, to } => {
            format!("~ {} {} -> {to}", target.kind_word(), address(target))
        }
        Plan::Set { target, attrs } => format!(
            "~ {} {} {}",
            target.kind_word(),
            address(target),
            join_attrs(attrs)
        ),
    };
    vec![line]
}

/// A node's short human address, e.g. `verify` or `workspace.write_file`.
fn address(target: &Target) -> String {
    match target {
        Target::Model => "model".to_string(),
        Target::Crate(name)
        | Target::Service(name)
        | Target::Inbound(name)
        | Target::Operation(name)
        | Target::Type(name)
        | Target::Port(name) => name.clone(),
        Target::Dep { crate_name, dep } => format!("{crate_name}.{dep}"),
        Target::Method { port, name } => format!("{port}.{name}"),
        Target::Field { ty, name } | Target::Variant { ty, name } => format!("{ty}.{name}"),
    }
}

/// The address an addition reads as, from its parent and the new name.
fn add_address(parent: &Target, kind: NodeKind, name: &str) -> String {
    match (parent, kind) {
        (Target::Crate(crate_name), NodeKind::Dep) => format!("{crate_name}.{name}"),
        (Target::Port(port), NodeKind::Method) => format!("{port}.{name}"),
        (Target::Type(ty), NodeKind::Field | NodeKind::Variant) => format!("{ty}.{name}"),
        _ => name.to_string(),
    }
}

fn join_attrs(attrs: &[(String, String)]) -> String {
    attrs
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ============================================================================
// Resolution and checks.
// ============================================================================

/// Parse a handle into a [`Target`], turning a parse failure into a diagnostic.
fn resolve(model: &Model, handle: &str) -> Result<Target, Vec<Diagnostic>> {
    Target::parse(model, handle).map_err(|error| {
        vec![diagnostic(
            "PATCH002",
            error.to_string(),
            "address a node with a handle from `theseus query`",
        )]
    })
}

/// The node a non-add edit targets must exist.
fn present(model: &Model, target: &Target) -> Result<(), Vec<Diagnostic>> {
    if node_exists(model, target) {
        Ok(())
    } else {
        Err(vec![diagnostic(
            "PATCH005",
            format!("no node with handle `{}`", target.render(model)),
            "run `theseus query` to list the addressable nodes",
        )])
    }
}

/// The model root is not a removable, renamable, or settable node.
fn reject_root(target: &Target) -> Result<(), Vec<Diagnostic>> {
    if matches!(target, Target::Model) {
        Err(vec![diagnostic(
            "PATCH002",
            "the model root is not an editable node",
            "address an operation, type, port, method, field, or variant",
        )])
    } else {
        Ok(())
    }
}

/// A top-level addition attaches to the model root.
fn under_root(parent: &Target, kind: NodeKind) -> Result<(), Vec<Diagnostic>> {
    if matches!(parent, Target::Model) {
        Ok(())
    } else {
        Err(vec![diagnostic(
            "PATCH006",
            format!("a {} attaches to the model root", kind.word()),
            "pass --parent model:<model>",
        )])
    }
}

/// The parent of a method, resolved to the name of an existing port.
fn parent_port<'a>(model: &Model, parent: &'a Target) -> Result<&'a str, Vec<Diagnostic>> {
    match parent {
        Target::Port(port) if port_exists(model, port) => Ok(port),
        _ => Err(vec![diagnostic(
            "PATCH006",
            "a method attaches to an existing port",
            "pass --parent port:<model>:<name>",
        )]),
    }
}

/// The parent of a field, resolved to the name of an existing struct type.
fn parent_struct<'a>(model: &Model, parent: &'a Target) -> Result<&'a str, Vec<Diagnostic>> {
    match parent {
        Target::Type(ty) if matches!(shape_of(model, ty), Some(TypeShape::Struct(_))) => Ok(ty),
        _ => Err(vec![diagnostic(
            "PATCH006",
            "a field attaches to an existing struct type",
            "pass --parent type:<model>:<name> naming a struct",
        )]),
    }
}

/// The parent of a variant, resolved to the name of an existing enum type.
fn parent_enum<'a>(model: &Model, parent: &'a Target) -> Result<&'a str, Vec<Diagnostic>> {
    match parent {
        Target::Type(ty) if matches!(shape_of(model, ty), Some(TypeShape::Enum(_))) => Ok(ty),
        _ => Err(vec![diagnostic(
            "PATCH006",
            "a variant attaches to an existing enum type",
            "pass --parent type:<model>:<name> naming an enum",
        )]),
    }
}

/// A name being added must not already be taken among its siblings.
fn free(taken: bool, kind: &str, name: &str) -> Result<(), Vec<Diagnostic>> {
    if taken {
        Err(vec![diagnostic(
            "PATCH007",
            format!("{kind} `{name}` already exists"),
            "choose an unused name",
        )])
    } else {
        Ok(())
    }
}

/// The keys an edit carries must all be settable on the node kind.
fn allow_keys(attrs: &[(String, String)], allowed: &[&str]) -> Result<(), Vec<Diagnostic>> {
    for (key, _) in attrs {
        if !allowed.contains(&key.as_str()) {
            let list = if allowed.is_empty() {
                "none".to_string()
            } else {
                allowed.join(", ")
            };
            return Err(vec![diagnostic(
                "PATCH010",
                format!("unknown attribute `{key}`"),
                format!("settable attributes here: {list}"),
            )]);
        }
    }
    Ok(())
}

/// An attribute the edit requires must be present.
fn required<'a>(attrs: &'a [(String, String)], key: &str) -> Result<&'a str, Vec<Diagnostic>> {
    attr(attrs, key).ok_or_else(|| {
        vec![diagnostic(
            "PATCH011",
            format!("attribute `{key}` is required here"),
            format!("pass --set {key}=<value>"),
        )]
    })
}

/// The settable scalar attributes of a node kind.
fn settable_keys(target: &Target) -> &'static [&'static str] {
    match target {
        Target::Crate(_) => &["dir", "layer"],
        Target::Inbound(_) => &["transport", "service", "crate"],
        Target::Operation(_) | Target::Method { .. } => &["summary", "request", "response"],
        Target::Port(_) => &["summary"],
        Target::Field { .. } => &["ty", "doc"],
        Target::Dep { .. }
        | Target::Service(_)
        | Target::Type(_)
        | Target::Variant { .. }
        | Target::Model => &[],
    }
}

/// Whether the name `to` is already taken among a node's siblings.
fn sibling_taken(model: &Model, target: &Target, to: &str) -> bool {
    match target {
        Target::Crate(_) => model.crate_named(to).is_some(),
        Target::Dep { crate_name, .. } => dep_exists(model, crate_name, to),
        Target::Service(_) => service_exists(model, to),
        Target::Inbound(_) => inbound_exists(model, to),
        Target::Operation(_) => model.operation(to).is_some(),
        Target::Type(_) => model.type_def(to).is_some(),
        Target::Port(_) => port_exists(model, to),
        Target::Method { port, .. } => method_of(model, port, to).is_some(),
        Target::Field { ty, .. } => field_of(model, ty, to).is_some(),
        Target::Variant { ty, .. } => variant_exists(model, ty, to),
        Target::Model => true,
    }
}

fn node_exists(model: &Model, target: &Target) -> bool {
    match target {
        Target::Model => true,
        Target::Crate(name) => model.crate_named(name).is_some(),
        Target::Dep { crate_name, dep } => dep_exists(model, crate_name, dep),
        Target::Service(name) => service_exists(model, name),
        Target::Inbound(name) => inbound_exists(model, name),
        Target::Operation(name) => model.operation(name).is_some(),
        Target::Type(name) => model.type_def(name).is_some(),
        Target::Port(name) => port_exists(model, name),
        Target::Method { port, name } => method_of(model, port, name).is_some(),
        Target::Field { ty, name } => field_of(model, ty, name).is_some(),
        Target::Variant { ty, name } => variant_exists(model, ty, name),
    }
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, value)| value.as_str())
}

fn set_if(attrs: &[(String, String)], key: &str, slot: &mut String) {
    if let Some(value) = attr(attrs, key) {
        *slot = value.to_string();
    }
}

// ============================================================================
// Node lookups.
// ============================================================================

fn port_exists(model: &Model, name: &str) -> bool {
    model
        .services
        .iter()
        .any(|service| service.outbound.iter().any(|port| port.name == name))
}

fn service_exists(model: &Model, name: &str) -> bool {
    model.services.iter().any(|service| service.name == name)
}

fn inbound_exists(model: &Model, name: &str) -> bool {
    model.inbounds.iter().any(|inbound| inbound.name == name)
}

/// Whether `crate_name` already declares a dependency on `dep`.
fn dep_exists(model: &Model, crate_name: &str, dep: &str) -> bool {
    model
        .crate_named(crate_name)
        .is_some_and(|node| node.depends_on.iter().any(|d| d == dep))
}

fn crate_node_mut<'a>(model: &'a mut Model, name: &str) -> Option<&'a mut CrateNode> {
    model.crates.iter_mut().find(|node| node.name == name)
}

/// The parent of a dependency, resolved to the name of an existing crate.
fn parent_crate<'a>(model: &Model, parent: &'a Target) -> Result<&'a str, Vec<Diagnostic>> {
    match parent {
        Target::Crate(name) if model.crate_named(name).is_some() => Ok(name),
        _ => Err(vec![diagnostic(
            "PATCH006",
            "a dependency attaches to an existing crate",
            "pass --parent crate:<model>:<name>",
        )]),
    }
}

/// An operation or port attaches to the model root or to an existing service.
fn attaches_to_service(model: &Model, parent: &Target) -> Result<(), Vec<Diagnostic>> {
    match parent {
        Target::Model => Ok(()),
        Target::Service(name) if service_exists(model, name) => Ok(()),
        _ => Err(vec![diagnostic(
            "PATCH006",
            "an operation or port attaches to the model root or a service",
            "pass --parent model:<model> or service:<model>:<name>",
        )]),
    }
}

/// The index of the service an operation or port attaches to: the one a service
/// handle names, or the first service under the model root.
fn target_service_index(model: &Model, parent: &Target) -> Option<usize> {
    match parent {
        Target::Service(name) => model.services.iter().position(|s| &s.name == name),
        _ if model.services.is_empty() => None,
        _ => Some(0),
    }
}

fn shape_of<'a>(model: &'a Model, ty: &str) -> Option<&'a TypeShape> {
    model.type_def(ty).map(|def| &def.shape)
}

fn method_of<'a>(model: &'a Model, port: &str, name: &str) -> Option<&'a Method> {
    model
        .services
        .iter()
        .flat_map(|service| service.outbound.iter())
        .find(|p| p.name == port)
        .and_then(|p| p.methods.iter().find(|m| m.name == name))
}

fn field_of<'a>(model: &'a Model, ty: &str, name: &str) -> Option<&'a Field> {
    match shape_of(model, ty) {
        Some(TypeShape::Struct(fields)) => fields.iter().find(|f| f.name == name),
        _ => None,
    }
}

fn variant_exists(model: &Model, ty: &str, name: &str) -> bool {
    matches!(shape_of(model, ty), Some(TypeShape::Enum(variants)) if variants.iter().any(|v| v == name))
}

fn operations_mut(model: &mut Model) -> impl Iterator<Item = &mut Operation> {
    model
        .services
        .iter_mut()
        .flat_map(|service| service.operations.iter_mut())
}

fn ports_mut(model: &mut Model) -> impl Iterator<Item = &mut Port> {
    model
        .services
        .iter_mut()
        .flat_map(|service| service.outbound.iter_mut())
}

fn port_mut<'a>(model: &'a mut Model, name: &str) -> Option<&'a mut Port> {
    ports_mut(model).find(|port| port.name == name)
}

fn struct_fields_mut<'a>(model: &'a mut Model, ty: &str) -> Option<&'a mut Vec<Field>> {
    match model.types.iter_mut().find(|t| t.name == ty) {
        Some(TypeDef {
            shape: TypeShape::Struct(fields),
            ..
        }) => Some(fields),
        _ => None,
    }
}

fn enum_variants_mut<'a>(model: &'a mut Model, ty: &str) -> Option<&'a mut Vec<String>> {
    match model.types.iter_mut().find(|t| t.name == ty) {
        Some(TypeDef {
            shape: TypeShape::Enum(variants),
            ..
        }) => Some(variants),
        _ => None,
    }
}

// ============================================================================
// Type references.
// ============================================================================

/// Whether any operation, struct field, or port method names this type.
///
/// A struct field type may wrap the name in `Option<…>`, so a field counts as a
/// reference when its inner type matches.
fn type_referenced(model: &Model, name: &str) -> bool {
    let in_operations = model
        .operations()
        .into_iter()
        .any(|op| op.request == name || op.response == name);
    let in_fields = model.types.iter().any(|t| match &t.shape {
        TypeShape::Struct(fields) => fields.iter().any(|f| field_names_type(&f.ty, name)),
        _ => false,
    });
    let in_methods = model.services.iter().any(|service| {
        service.outbound.iter().any(|port| {
            port.methods
                .iter()
                .any(|m| m.request == name || m.response == name)
        })
    });
    in_operations || in_fields || in_methods
}

/// Whether a field type label names this type, bare or wrapped in `Option<…>`.
fn field_names_type(ty: &str, name: &str) -> bool {
    ty == name || option_inner(ty) == Some(name)
}

/// The inner type of an `Option<…>` label, when the label is one.
fn option_inner(ty: &str) -> Option<&str> {
    ty.strip_prefix("Option<")?.strip_suffix('>')
}

/// Rewrite every reference to `name` so it points at `to`.
///
/// Operations' request and response labels, struct field types (bare or wrapped
/// in `Option<…>`), and port method request and response labels are updated.
fn rewrite_type_references(model: &mut Model, name: &str, to: &str) {
    for t in &mut model.types {
        if let TypeShape::Struct(fields) = &mut t.shape {
            for field in fields {
                if field.ty == name {
                    field.ty = to.to_string();
                } else if option_inner(&field.ty) == Some(name) {
                    field.ty = format!("Option<{to}>");
                }
            }
        }
    }
    for service in &mut model.services {
        for op in &mut service.operations {
            if op.request == name {
                op.request = to.to_string();
            }
            if op.response == name {
                op.response = to.to_string();
            }
        }
        for port in &mut service.outbound {
            for method in &mut port.methods {
                if method.request == name {
                    method.request = to.to_string();
                }
                if method.response == name {
                    method.response = to.to_string();
                }
            }
        }
    }
}

// ============================================================================
// Shape parsing.
// ============================================================================

/// Why a `--set shape=…` value could not be parsed.
#[derive(Debug, thiserror::Error)]
enum ShapeError {
    #[error("shape must be `kind:value`")]
    Format,
    #[error("unknown shape `{0}`; expected newtype, foreign, enum, or struct")]
    UnknownKind(String),
    #[error("struct field must be `name=Type`")]
    Field,
}

/// Why an inbound transport name could not be parsed.
#[derive(Debug, thiserror::Error)]
#[error("unknown transport `{0}`")]
struct TransportError(String);

/// Parse an inbound transport name into a [`Transport`].
fn parse_transport(text: &str) -> Result<Transport, TransportError> {
    match text {
        "Cli" => Ok(Transport::Cli),
        "Http" => Ok(Transport::Http),
        "Grpc" => Ok(Transport::Grpc),
        "Agent" => Ok(Transport::Agent),
        "Mcp" => Ok(Transport::Mcp),
        other => Err(TransportError(other.to_string())),
    }
}

fn transport_refused(error: TransportError) -> Vec<Diagnostic> {
    vec![diagnostic(
        "PATCH013",
        error.to_string(),
        "transport is one of: Cli, Http, Grpc, Agent, Mcp",
    )]
}

/// Parse a shape spec into a [`TypeShape`]: `newtype:Inner`, `foreign:Path`,
/// `enum:A,B,C`, or `struct:field=Type,field=Type` (a field may carry an inline
/// doc as `field=Type:doc`).
fn parse_shape(spec: &str) -> Result<TypeShape, ShapeError> {
    let (kind, value) = spec.split_once(':').ok_or(ShapeError::Format)?;
    match kind {
        "newtype" => Ok(TypeShape::Newtype(value.to_string())),
        "foreign" => Ok(TypeShape::Foreign(value.to_string())),
        "enum" => Ok(TypeShape::Enum(
            value.split(',').map(|v| v.trim().to_string()).collect(),
        )),
        "struct" => {
            let fields = value
                .split(',')
                .map(parse_field)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(TypeShape::Struct(fields))
        }
        other => Err(ShapeError::UnknownKind(other.to_string())),
    }
}

/// Parse one struct field: `name=Type`, or `name=Type:doc` to carry a doc inline.
/// The type holds no `:`, so the first one after the type begins the doc.
fn parse_field(spec: &str) -> Result<Field, ShapeError> {
    let (name, rest) = spec.split_once('=').ok_or(ShapeError::Field)?;
    let (ty, doc) = rest.split_once(':').unwrap_or((rest, ""));
    Ok(Field {
        name: name.trim().to_string(),
        ty: ty.trim().to_string(),
        doc: doc.trim().to_string(),
    })
}

fn shape_refused(error: ShapeError) -> Vec<Diagnostic> {
    vec![diagnostic(
        "PATCH012",
        error.to_string(),
        "pass --set shape=newtype:Inner, foreign:Path, enum:A,B, or struct:f=Type",
    )]
}

/// Why a crate's architectural layer could not be parsed.
#[derive(Debug, thiserror::Error)]
#[error("layer must be a non-negative integer, not `{0}`")]
struct LayerError(String);

/// Parse a crate's architectural layer, a non-negative integer.
fn parse_layer(text: &str) -> Result<u32, LayerError> {
    text.parse().map_err(|_| LayerError(text.to_string()))
}

fn layer_refused(error: LayerError) -> Vec<Diagnostic> {
    vec![diagnostic(
        "PATCH014",
        error.to_string(),
        "pass --set layer=<integer>",
    )]
}

fn diagnostic(code: &str, message: impl Into<String>, repair: impl Into<String>) -> Diagnostic {
    Diagnostic {
        code: code.to_string(),
        message: message.into(),
        repair: repair.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    /// Apply an edit, returning the outcome and model.
    fn edit(model: &Model, edit: Edit) -> (PatchOutcome, Option<Model>) {
        apply_edit(model, &edit)
    }

    /// Apply an edit and unwrap the accepted model.
    fn accept(model: &Model, e: Edit) -> Model {
        let (outcome, next) = edit(model, e);
        assert!(outcome.ok, "edit refused: {:?}", outcome.diagnostics);
        next.expect("an accepted edit yields a model")
    }

    fn add(parent: &str, kind: &str, name: &str, attrs: &[(&str, &str)]) -> Edit {
        Edit::Add {
            parent: parent.to_string(),
            kind: kind.to_string(),
            name: name.to_string(),
            attrs: attrs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn code(outcome: &PatchOutcome) -> &str {
        &outcome.diagnostics[0].code
    }

    #[test]
    fn add_a_crate_and_a_dependency_on_it() {
        let model = sample_model();
        let model = accept(
            &model,
            add(
                "model:sample",
                "crate",
                "sample-extra",
                &[("dir", "extra"), ("layer", "1")],
            ),
        );
        let node = model
            .crate_named("sample-extra")
            .expect("the crate was added");
        assert_eq!(node.dir, "extra");
        assert_eq!(node.layer, 1);

        // A dependency addressed to an existing crate appends to its depends_on.
        let model = accept(
            &model,
            add("crate:sample:sample", "dep", "sample-extra", &[]),
        );
        let base = model.crate_named("sample").unwrap();
        assert!(base.depends_on.iter().any(|d| d == "sample-extra"));
    }

    #[test]
    fn add_crate_rejects_a_non_integer_layer() {
        let model = sample_model();
        let (outcome, _) = edit(
            &model,
            add(
                "model:sample",
                "crate",
                "sample-extra",
                &[("dir", "extra"), ("layer", "ground")],
            ),
        );
        assert_eq!(code(&outcome), "PATCH014");
    }

    #[test]
    fn set_crate_rejects_a_non_integer_layer() {
        let model = sample_model();
        let (outcome, next) = edit(
            &model,
            Edit::Set {
                target: "crate:sample:sample".to_string(),
                attrs: vec![("layer".to_string(), "ground".to_string())],
            },
        );
        assert_eq!(code(&outcome), "PATCH014");
        assert!(next.is_none(), "a refused set must not mutate the model");
    }

    #[test]
    fn a_dependency_needs_a_crate_parent() {
        let model = sample_model();
        let (outcome, _) = edit(&model, add("model:sample", "dep", "x", &[]));
        assert_eq!(code(&outcome), "PATCH006");
    }

    #[test]
    fn add_a_service_and_an_operation_addressed_to_it() {
        let model = sample_model();
        let model = accept(
            &model,
            add(
                "model:sample",
                "service",
                "Calculator",
                &[("crate", "calc")],
            ),
        );
        let calculator = model
            .services
            .iter()
            .find(|s| s.name == "Calculator")
            .expect("the service was added");
        assert_eq!(calculator.crate_name, "calc");

        // An operation addressed to the new service lands there, not in the first.
        let model = accept(
            &model,
            add(
                "service:sample:Calculator",
                "operation",
                "add",
                &[("request", "Operands")],
            ),
        );
        let calculator = model
            .services
            .iter()
            .find(|s| s.name == "Calculator")
            .unwrap();
        assert!(calculator.operations.iter().any(|op| op.name == "add"));
        assert!(
            !model.services[0]
                .operations
                .iter()
                .any(|op| op.name == "add")
        );
    }

    #[test]
    fn add_a_service_targeting_port_to_a_named_service() {
        let model = accept(
            &sample_model(),
            add(
                "model:sample",
                "service",
                "Calculator",
                &[("crate", "calc")],
            ),
        );
        let model = accept(
            &model,
            add(
                "service:sample:Sample",
                "port",
                "calculator",
                &[
                    ("summary", "Calls the calculator."),
                    ("target", "Calculator"),
                ],
            ),
        );
        let port = model.services[0]
            .outbound
            .iter()
            .find(|p| p.name == "calculator")
            .expect("the port was added to the named service");
        assert_eq!(port.target.as_deref(), Some("Calculator"));
    }

    #[test]
    fn add_an_inbound_adapter() {
        let model = sample_model();
        let model = accept(
            &model,
            add(
                "model:sample",
                "inbound",
                "tools",
                &[
                    ("transport", "Cli"),
                    ("service", "Sample"),
                    ("crate", "sample"),
                ],
            ),
        );
        let inbound = model
            .inbounds
            .iter()
            .find(|i| i.name == "tools")
            .expect("the inbound was added");
        assert_eq!(inbound.service, "Sample");
        assert_eq!(inbound.crate_name, "sample");
        assert_eq!(inbound.transport, Transport::Cli);
    }

    #[test]
    fn add_inbound_rejects_an_unknown_transport() {
        let model = sample_model();
        let (outcome, _) = edit(
            &model,
            add(
                "model:sample",
                "inbound",
                "tools",
                &[("transport", "Telepathy"), ("service", "Sample")],
            ),
        );
        assert_eq!(code(&outcome), "PATCH013");
    }

    #[test]
    fn add_operation_under_the_root() {
        let model = sample_model();
        let next = accept(
            &model,
            add(
                "model:sample",
                "operation",
                "ping",
                &[("summary", "Ping."), ("response", "Empty")],
            ),
        );
        assert!(next.operation("ping").is_some());
    }

    #[test]
    fn a_batch_applies_every_edit_in_order() {
        let model = sample_model();
        let edits = vec![
            add(
                "model:sample",
                "type",
                "Token",
                &[("shape", "newtype:String")],
            ),
            add("model:sample", "operation", "ping", &[]),
            Edit::Rename {
                target: "op:sample:ping".to_string(),
                to: "pong".to_string(),
            },
        ];
        let (outcome, next) = apply_edits(&model, &edits);
        assert!(outcome.ok, "batch refused: {:?}", outcome.diagnostics);
        assert_eq!(outcome.diff.len(), 3);
        // The rename sees the operation the earlier edit added.
        let next = next.unwrap();
        assert!(next.type_def("Token").is_some());
        assert!(next.operation("pong").is_some());
        assert!(next.operation("ping").is_none());
    }

    #[test]
    fn a_batch_is_atomic_on_failure() {
        let model = sample_model();
        let edits = vec![
            add(
                "model:sample",
                "type",
                "Token",
                &[("shape", "newtype:String")],
            ),
            Edit::Remove {
                target: "op:sample:nope".to_string(),
            },
        ];
        let (outcome, next) = apply_edits(&model, &edits);
        assert!(!outcome.ok);
        // The first edit's effect is discarded — nothing is written.
        assert!(next.is_none());
        assert_eq!(code(&outcome), "PATCH005");
    }

    #[test]
    fn duplicate_name_is_refused() {
        let model = sample_model();
        let (outcome, _) = edit(&model, add("model:sample", "operation", "greet", &[]));
        assert_eq!(code(&outcome), "PATCH007");
    }

    #[test]
    fn a_malformed_handle_is_refused() {
        let model = sample_model();
        let (outcome, _) = edit(
            &model,
            Edit::Remove {
                target: "nonsense".to_string(),
            },
        );
        assert_eq!(code(&outcome), "PATCH002");
    }

    #[test]
    fn add_type_field_and_variant() {
        let model = sample_model();
        let model = accept(
            &model,
            add(
                "model:sample",
                "type",
                "Operands",
                &[("shape", "struct:a=String")],
            ),
        );
        let model = accept(
            &model,
            add(
                "type:sample:Operands",
                "field",
                "b",
                &[("ty", "String"), ("doc", "Right operand.")],
            ),
        );
        let fields = match &model.type_def("Operands").unwrap().shape {
            TypeShape::Struct(fields) => fields,
            other => panic!("expected a struct, found {other:?}"),
        };
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[1].doc, "Right operand.");

        let model = accept(
            &model,
            add("model:sample", "type", "Status", &[("shape", "enum:Ready")]),
        );
        let model = accept(&model, add("type:sample:Status", "variant", "Busy", &[]));
        match &model.type_def("Status").unwrap().shape {
            TypeShape::Enum(variants) => assert_eq!(variants, &["Ready", "Busy"]),
            other => panic!("expected an enum, found {other:?}"),
        }
    }

    #[test]
    fn add_method_to_a_port() {
        let model = sample_model();
        let model = accept(
            &model,
            add(
                "port:sample:store",
                "method",
                "read",
                &[("request", "Empty"), ("response", "String")],
            ),
        );
        let port = model.services[0]
            .outbound
            .iter()
            .find(|p| p.name == "store")
            .unwrap();
        assert_eq!(port.methods[0].name, "read");
        assert_eq!(port.methods[0].response, "String");
    }

    #[test]
    fn a_field_needs_a_struct_parent() {
        let model = accept(
            &sample_model(),
            add(
                "model:sample",
                "type",
                "Token",
                &[("shape", "newtype:String")],
            ),
        );
        let (outcome, _) = edit(
            &model,
            add("type:sample:Token", "field", "x", &[("ty", "String")]),
        );
        assert_eq!(code(&outcome), "PATCH006");
    }

    #[test]
    fn a_type_without_a_shape_is_refused() {
        let model = sample_model();
        let (outcome, _) = edit(&model, add("model:sample", "type", "Bare", &[]));
        assert_eq!(code(&outcome), "PATCH011");
    }

    #[test]
    fn a_struct_shape_carries_inline_field_docs() {
        let next = accept(
            &sample_model(),
            add(
                "model:sample",
                "type",
                "Operands",
                &[(
                    "shape",
                    "struct:a=String:Left operand.,b=String:Right operand.",
                )],
            ),
        );
        let fields = match &next.type_def("Operands").unwrap().shape {
            TypeShape::Struct(fields) => fields,
            other => panic!("expected a struct, found {other:?}"),
        };
        assert_eq!(fields[0].doc, "Left operand.");
        assert_eq!(fields[1].doc, "Right operand.");
    }

    #[test]
    fn an_unknown_attribute_is_refused() {
        let model = sample_model();
        let (outcome, _) = edit(
            &model,
            Edit::Set {
                target: "op:sample:greet".to_string(),
                attrs: vec![("color".to_string(), "blue".to_string())],
            },
        );
        assert_eq!(code(&outcome), "PATCH010");
    }

    #[test]
    fn set_an_operations_request_and_response() {
        let model = accept(
            &sample_model(),
            add(
                "model:sample",
                "type",
                "Reply",
                &[("shape", "newtype:String")],
            ),
        );
        let model = accept(
            &model,
            Edit::Set {
                target: "op:sample:greet".to_string(),
                attrs: vec![("response".to_string(), "Reply".to_string())],
            },
        );
        assert_eq!(model.operation("greet").unwrap().response, "Reply");
    }

    #[test]
    fn remove_and_rename_a_field() {
        let model = accept(
            &sample_model(),
            add(
                "model:sample",
                "type",
                "Operands",
                &[("shape", "struct:a=String,b=String")],
            ),
        );
        let renamed = accept(
            &model,
            Edit::Rename {
                target: "field:sample:Operands.a".to_string(),
                to: "left".to_string(),
            },
        );
        let removed = accept(
            &renamed,
            Edit::Remove {
                target: "field:sample:Operands.b".to_string(),
            },
        );
        let fields = match &removed.type_def("Operands").unwrap().shape {
            TypeShape::Struct(fields) => fields,
            other => panic!("expected a struct, found {other:?}"),
        };
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "left");
    }

    #[test]
    fn remove_a_referenced_type_is_refused() {
        let mut model = sample_model();
        model.types.push(TypeDef {
            name: "Greeting".to_string(),
            shape: TypeShape::Newtype("String".to_string()),
        });
        model.services[0].operations[0].response = "Greeting".to_string();
        let (outcome, next) = edit(
            &model,
            Edit::Remove {
                target: "type:sample:Greeting".to_string(),
            },
        );
        assert!(!outcome.ok);
        assert!(next.is_none());
        assert_eq!(code(&outcome), "PATCH009");
    }

    #[test]
    fn rename_a_type_updates_references() {
        let mut model = sample_model();
        model.types.push(TypeDef {
            name: "Greeting".to_string(),
            shape: TypeShape::Newtype("String".to_string()),
        });
        model.services[0].operations[0].response = "Greeting".to_string();
        let renamed = accept(
            &model,
            Edit::Rename {
                target: "type:sample:Greeting".to_string(),
                to: "Reply".to_string(),
            },
        );
        assert!(renamed.type_def("Greeting").is_none());
        assert!(renamed.type_def("Reply").is_some());
        assert_eq!(renamed.services[0].operations[0].response, "Reply");
    }

    #[test]
    fn removing_a_missing_node_is_refused() {
        let model = sample_model();
        let (outcome, _) = edit(
            &model,
            Edit::Remove {
                target: "op:sample:nope".to_string(),
            },
        );
        assert_eq!(code(&outcome), "PATCH005");
    }

    #[test]
    fn the_root_is_not_removable() {
        let model = sample_model();
        let (outcome, _) = edit(
            &model,
            Edit::Remove {
                target: "model:sample".to_string(),
            },
        );
        assert_eq!(code(&outcome), "PATCH002");
    }

    #[test]
    fn field_doc_is_settable() {
        let model = accept(
            &sample_model(),
            add(
                "model:sample",
                "type",
                "Operands",
                &[("shape", "struct:a=String")],
            ),
        );
        let model = accept(
            &model,
            Edit::Set {
                target: "field:sample:Operands.a".to_string(),
                attrs: vec![("doc".to_string(), "Left operand.".to_string())],
            },
        );
        let fields = match &model.type_def("Operands").unwrap().shape {
            TypeShape::Struct(fields) => fields,
            other => panic!("expected a struct, found {other:?}"),
        };
        assert_eq!(fields[0].doc, "Left operand.");
    }
}
