//! Invocation projection: one modeled operation as its own command line.
//!
//! A `Cli` inbound's surface is generated from the model, so the command that
//! drives one operation is itself a projection: the inbound's crate and binary,
//! the operation as the subcommand, and one kebab-case flag per request field.
//! [`cli_invocation`] builds that projection and validates the supplied field
//! values against the request contract, so a driver executes exactly the
//! surface the model declares — the crate, the binary, the subcommand, and the
//! flags are the model's; only the field values are the caller's.

use serde_json::Value;

use crate::{
    label::{optional_inner, vec_inner},
    model::{Model, Operation, Service, Transport, TypeShape},
};

/// One modeled operation, projected as the command line of its service's `Cli`
/// inbound: `cargo run -p {crate_name} --bin {binary} -- {argv…}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInvocation {
    /// The inbound's crate, the `-p` target.
    pub crate_name: String,
    /// The inbound's binary, named for the inbound.
    pub binary: String,
    /// The subcommand and its flags, everything after `--`.
    pub argv: Vec<String>,
}

/// Why an operation could not be projected as an invocation.
#[derive(Debug, thiserror::Error)]
pub enum InvokeError {
    /// The model has no operation by that name.
    #[error("no operation named `{0}`")]
    UnknownOperation(String),
    /// No `Cli` inbound drives the operation's service.
    #[error("no Cli inbound drives service `{0}`, so `{1}` has no command line")]
    NoCliInbound(String, String),
    /// The input names a field the request does not declare.
    #[error("`{operation}` takes no field `{field}`; its fields are {fields}")]
    UnknownField {
        operation: String,
        field: String,
        fields: String,
    },
    /// A required field is missing from the input.
    #[error("`{operation}` requires field `{field}`")]
    MissingField { operation: String, field: String },
    /// A field value does not fit its contract shape.
    #[error("field `{field}` takes {expected}, got `{got}`")]
    UnsupportedValue {
        field: String,
        expected: &'static str,
        got: String,
    },
    /// The input is not a JSON object.
    #[error("the input must be a JSON object of field values, got `{0}`")]
    InputShape(String),
}

/// Project `operation` as the command line of its service's `Cli` inbound,
/// with `input` supplying the request's field values as a JSON object. `Null`
/// stands for an empty input.
pub fn cli_invocation(
    model: &Model,
    operation: &str,
    input: &Value,
) -> Result<CliInvocation, InvokeError> {
    let (service, op) = operation_of(model, operation)?;
    let inbound = model
        .inbounds
        .iter()
        .find(|inbound| inbound.transport == Transport::Cli && inbound.service == service.name)
        .ok_or_else(|| InvokeError::NoCliInbound(service.name.clone(), operation.to_string()))?;

    let mut argv = vec![op.name.clone()];
    argv.extend(field_flags(model, op, input)?);
    Ok(CliInvocation {
        crate_name: inbound.crate_name.clone(),
        binary: inbound.name.clone(),
        argv,
    })
}

fn operation_of<'a>(
    model: &'a Model,
    name: &str,
) -> Result<(&'a Service, &'a Operation), InvokeError> {
    model
        .services
        .iter()
        .find_map(|service| {
            service
                .operations
                .iter()
                .find(|op| op.name == name)
                .map(|op| (service, op))
        })
        .ok_or_else(|| InvokeError::UnknownOperation(name.to_string()))
}

/// The flags for an operation's request fields: `--field value` per supplied
/// field, in the contract's field order, validated the way the generated
/// parser will parse them — `bool` a bare flag, `Vec<…>` repeatable,
/// `Option<…>` omittable, anything else required.
fn field_flags(model: &Model, op: &Operation, input: &Value) -> Result<Vec<String>, InvokeError> {
    let fields = request_fields(model, op);
    let supplied = match input {
        Value::Null => serde_json::Map::new(),
        Value::Object(map) => map.clone(),
        other => return Err(InvokeError::InputShape(other.to_string())),
    };

    for key in supplied.keys() {
        if !fields.iter().any(|(name, _)| name == key) {
            return Err(InvokeError::UnknownField {
                operation: op.name.clone(),
                field: key.clone(),
                fields: if fields.is_empty() {
                    "none".to_string()
                } else {
                    fields
                        .iter()
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            });
        }
    }

    let mut argv = Vec::new();
    for (name, ty) in &fields {
        let flag = format!("--{}", name.replace('_', "-"));
        let value = supplied.get(name);
        if ty == "bool" {
            match value {
                Some(Value::Bool(true)) => argv.push(flag),
                Some(Value::Bool(false)) | None => {}
                Some(other) => {
                    return Err(unsupported(name, "true or false", other));
                }
            }
        } else if vec_inner(ty).is_some() {
            match value {
                Some(Value::Array(items)) => {
                    for item in items {
                        argv.push(flag.clone());
                        argv.push(scalar(name, item)?);
                    }
                }
                Some(item) => {
                    argv.push(flag);
                    argv.push(scalar(name, item)?);
                }
                None => {}
            }
        } else if optional_inner(ty).is_some() {
            if let Some(item) = value {
                argv.push(flag);
                argv.push(scalar(name, item)?);
            }
        } else {
            let item = value.ok_or_else(|| InvokeError::MissingField {
                operation: op.name.clone(),
                field: name.clone(),
            })?;
            argv.push(flag);
            argv.push(scalar(name, item)?);
        }
    }
    Ok(argv)
}

/// A field value as one argv entry: a string as itself, a number or boolean as
/// its text. Structured values have no command-line form.
fn scalar(field: &str, value: &Value) -> Result<String, InvokeError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        other => Err(unsupported(field, "a string, number, or boolean", other)),
    }
}

fn unsupported(field: &str, expected: &'static str, got: &Value) -> InvokeError {
    InvokeError::UnsupportedValue {
        field: field.to_string(),
        expected,
        got: got.to_string(),
    }
}

/// The `(name, type)` pairs of an operation's request fields. An `Empty` or
/// non-struct request contributes none.
fn request_fields(model: &Model, op: &Operation) -> Vec<(String, String)> {
    match model.type_def(&op.request) {
        Some(def) => match &def.shape {
            TypeShape::Struct(fields) => fields
                .iter()
                .map(|field| (field.name.clone(), field.ty.clone()))
                .collect(),
            _ => Vec::new(),
        },
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Port, Service};
    use serde_json::json;

    fn journal_like() -> Model {
        Model::new("Journal")
            .crate_node("journal", "journal", 1, &[])
            .crate_node("journal-cli", "cli", 2, &["journal"])
            .struct_type("AddRequest", &[("text", "String", "The entry.")])
            .struct_type(
                "FancyRequest",
                &[
                    ("dry_run", "bool", "Preview only."),
                    ("tags", "Vec<String>", "Labels."),
                    ("limit", "Option<u32>", "At most this many."),
                ],
            )
            .service(
                Service::new("Journal")
                    .crate_name("journal")
                    .operation("add", "Record one entry.", "AddRequest", "String")
                    .operation("count", "Count the entries.", "Empty", "String")
                    .operation("fancy", "Exercise the shapes.", "FancyRequest", "String")
                    .port(Port::new("store", "Persists entries.")),
            )
            .inbound("journal", Transport::Cli, "Journal", "journal-cli")
    }

    #[test]
    fn an_empty_request_projects_as_the_bare_subcommand() {
        let invocation = cli_invocation(&journal_like(), "count", &Value::Null).unwrap();
        assert_eq!(invocation.crate_name, "journal-cli");
        assert_eq!(invocation.binary, "journal");
        assert_eq!(invocation.argv, vec!["count"]);
    }

    #[test]
    fn fields_project_as_kebab_flags_in_contract_order() {
        let invocation = cli_invocation(
            &journal_like(),
            "fancy",
            &json!({ "limit": 3, "tags": ["a", "b"], "dry_run": true }),
        )
        .unwrap();
        assert_eq!(
            invocation.argv,
            vec![
                "fancy",
                "--dry-run",
                "--tags",
                "a",
                "--tags",
                "b",
                "--limit",
                "3"
            ]
        );
    }

    #[test]
    fn a_missing_required_field_is_refused() {
        let error = cli_invocation(&journal_like(), "add", &Value::Null).unwrap_err();
        assert!(matches!(error, InvokeError::MissingField { .. }), "{error}");
    }

    #[test]
    fn an_unknown_field_is_refused_with_the_contract() {
        let error = cli_invocation(&journal_like(), "add", &json!({ "body": "x" })).unwrap_err();
        assert!(error.to_string().contains("its fields are text"), "{error}");
    }

    #[test]
    fn a_service_without_a_cli_inbound_is_refused() {
        let mut model = journal_like();
        model.inbounds.clear();
        let error = cli_invocation(&model, "count", &Value::Null).unwrap_err();
        assert!(matches!(error, InvokeError::NoCliInbound(..)), "{error}");
    }

    #[test]
    fn an_unknown_operation_is_refused() {
        let error = cli_invocation(&journal_like(), "ghost", &Value::Null).unwrap_err();
        assert!(matches!(error, InvokeError::UnknownOperation(_)), "{error}");
    }
}
