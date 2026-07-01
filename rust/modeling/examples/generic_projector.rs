//! Proof of concept for the "generic projector" design (see
//! `docs/affordance-projector.md`).
//!
//! One clean operation contract is projected to several inbound surfaces. The
//! surfaces differ only in each inbound's *affordance policy* — how it supplies
//! the concurrency hash, and whether it offers the alternate input form. The
//! projector is generic. Each transport is a backend. Nothing is hand-written
//! per operation.
//!
//! ```text
//! cargo run -p theseus-modeling --example generic_projector
//! ```

// ----------------------------------------------------------------------------
// The contract — an operation's pure domain input, plus the properties that name
// which affordances apply. No transport detail lives here.
// ----------------------------------------------------------------------------

/// One field of an operation's input.
struct Field {
    name: &'static str,
    ty: &'static str,
}

/// A convenience entry an inbound may offer over the canonical input.
struct AltForm {
    label: &'static str,
    fields: Vec<Field>,
}

/// An operation contract. `input`/`output` are the domain shape. The two flags
/// name the affordances that apply — not how any surface renders them.
struct Operation {
    name: &'static str,
    input: Vec<Field>,
    output: &'static str,
    /// The operation checks an expected model hash (a concurrency guard).
    hash_checked: bool,
    /// An alternate input form exists (a convenience over the canonical input).
    alt_form: Option<AltForm>,
}

// ----------------------------------------------------------------------------
// The affordance policy — how one inbound resolves each affordance. This is the
// per-surface data the model carries on each inbound.
// ----------------------------------------------------------------------------

/// Who supplies the concurrency hash for a hash-checked operation.
enum HashSupply {
    /// The caller passes it, as a flag.
    Caller,
    /// The runtime stamps the live working-model hash. No field is surfaced.
    Auto,
    /// The client passes it out of band, in request metadata.
    Metadata,
}

/// Whether the surface offers the alternate input form.
enum Forms {
    /// Canonical plus any alternate form the operation declares.
    All,
    /// Canonical only.
    Canonical,
}

struct Policy {
    hash: HashSupply,
    forms: Forms,
}

// ----------------------------------------------------------------------------
// The generic projector — one backend per transport. Each renders an operation
// from the contract and the inbound's policy.
// ----------------------------------------------------------------------------

trait TransportBackend {
    fn transport(&self) -> &'static str;
    fn render(&self, op: &Operation, policy: &Policy) -> String;
}

/// A command-line surface: flags from the input fields, plus the alternate-form
/// flags and an `--expect-model-hash` flag when the policy calls for them.
struct CliBackend;

impl TransportBackend for CliBackend {
    fn transport(&self) -> &'static str {
        "Cli"
    }
    fn render(&self, op: &Operation, policy: &Policy) -> String {
        let mut out = format!("theseus {}", op.name);
        for field in &op.input {
            out.push_str(&format!(" --{} <{}>", field.name, field.ty));
        }
        if let (Forms::All, Some(alt)) = (&policy.forms, &op.alt_form) {
            out.push_str(&format!("\n    # or the {} form:", alt.label));
            for field in &alt.fields {
                out.push_str(&format!(" --{}", field.name));
            }
        }
        if op.hash_checked && matches!(policy.hash, HashSupply::Caller) {
            out.push_str("\n    --expect-model-hash <HASH>");
        }
        out
    }
}

/// An agent/MCP tool surface: a JSON tool schema over the input fields. An
/// auto-supplied hash never appears. The alternate form is dropped.
struct AgentBackend;

impl TransportBackend for AgentBackend {
    fn transport(&self) -> &'static str {
        "Agent"
    }
    fn render(&self, op: &Operation, _policy: &Policy) -> String {
        let props: Vec<String> = op
            .input
            .iter()
            .map(|f| format!("\"{}\": {{ \"type\": \"{}\" }}", f.name, json_ty(f.ty)))
            .collect();
        format!(
            "{{ \"name\": \"{}\",\n  \"input_schema\": {{ \"type\": \"object\", \"properties\": {{ {} }} }} }}",
            op.name,
            props.join(", ")
        )
    }
}

/// A gRPC surface: a proto `rpc` and its request message. A metadata-supplied
/// hash is read from request headers, not carried in the message.
struct GrpcBackend;

impl TransportBackend for GrpcBackend {
    fn transport(&self) -> &'static str {
        "Grpc"
    }
    fn render(&self, op: &Operation, policy: &Policy) -> String {
        let message = format!("{}Request", pascal(op.name));
        let mut fields = String::new();
        for (index, field) in op.input.iter().enumerate() {
            fields.push_str(&format!(
                "\n  {} {} = {};",
                proto_ty(field.ty),
                field.name,
                index + 1
            ));
        }
        let mut out = format!(
            "rpc {}({message}) returns ({});\nmessage {message} {{{fields}\n}}",
            pascal(op.name),
            op.output,
        );
        if op.hash_checked && matches!(policy.hash, HashSupply::Metadata) {
            out.push_str("\n// the handler reads `x-expect-model-hash` from request metadata");
        }
        out
    }
}

/// Map a contract type to a JSON-schema type.
fn json_ty(ty: &str) -> &'static str {
    if ty.starts_with('[') {
        "array"
    } else if ty == "bool" {
        "boolean"
    } else {
        "string"
    }
}

/// Map a contract type to a proto type. A `[T]` list becomes `repeated T`.
fn proto_ty(ty: &str) -> String {
    match ty
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
    {
        Some(inner) => format!("repeated {inner}"),
        None if ty == "bool" => "bool".to_string(),
        None => "string".to_string(),
    }
}

/// Uppercase the first character, for a proto message name.
fn pascal(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn show<B: TransportBackend>(op: &Operation, backend: &B, policy: Policy) {
    println!(
        "── {} ──────────────────────────────────",
        backend.transport()
    );
    println!("{}\n", backend.render(op, &policy));
}

fn main() {
    // One clean contract. `edit` and `write` are the whole domain input. The two
    // flags name affordances — a concurrency hash and an alternate entry form —
    // not any surface's rendering of them.
    let patch = Operation {
        name: "patch",
        input: vec![
            Field {
                name: "edit",
                ty: "[Edit]",
            },
            Field {
                name: "write",
                ty: "bool",
            },
        ],
        output: "PatchOutcome",
        hash_checked: true,
        alt_form: Some(AltForm {
            label: "single-edit",
            fields: vec![
                Field {
                    name: "verb",
                    ty: "String",
                },
                Field {
                    name: "target",
                    ty: "String",
                },
                Field {
                    name: "kind",
                    ty: "String",
                },
                Field {
                    name: "name",
                    ty: "String",
                },
                Field {
                    name: "to",
                    ty: "String",
                },
                Field {
                    name: "set",
                    ty: "String",
                },
            ],
        }),
    };

    println!("one contract:  patch(edit: [Edit], write: bool) -> PatchOutcome");
    println!("  properties:  hash_checked, alt_form(single-edit)\n");

    // Three inbounds, three policies over the SAME contract.
    show(
        &patch,
        &CliBackend,
        Policy {
            hash: HashSupply::Caller,
            forms: Forms::All,
        },
    );
    show(
        &patch,
        &AgentBackend,
        Policy {
            hash: HashSupply::Auto,
            forms: Forms::Canonical,
        },
    );
    show(
        &patch,
        &GrpcBackend,
        Policy {
            hash: HashSupply::Metadata,
            forms: Forms::Canonical,
        },
    );
}
