//! Handles: stable addresses shared by `query` and `patch`.
//!
//! A handle names one model node independently of its position, so an agent can
//! discover it from [`query`](crate::query) and hand it back to
//! [`patch`](crate::patch). A top-level node reads `kind:model:name`. A node
//! nested in a parent reads `kind:model:parent.name`. The model root, the parent
//! of every top-level addition, reads `model:name`. The grammar `query` mints is
//! the grammar `patch` resolves.

use crate::model::Model;

/// A resolved address into a model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// The model itself — the parent of a top-level addition.
    Model,
    /// A crate, by name — the parent a dependency attaches to.
    Crate(String),
    /// A dependency edge of a crate, naming the depended-on crate.
    Dep { crate_name: String, dep: String },
    /// A service, by name — the parent an operation or port may attach to.
    Service(String),
    /// An inbound adapter, by name.
    Inbound(String),
    /// An operation, by name.
    Operation(String),
    /// A type, by name.
    Type(String),
    /// An outbound port, by name.
    Port(String),
    /// A method of a port.
    Method { port: String, name: String },
    /// A field of a struct type.
    Field { ty: String, name: String },
    /// A variant of an enum type.
    Variant { ty: String, name: String },
}

/// The kind of node an addition creates, naming where it attaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Crate,
    Dep,
    Service,
    Inbound,
    Operation,
    Type,
    Port,
    Method,
    Field,
    Variant,
}

/// Why a handle string could not be parsed into a [`Target`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HandleError {
    /// The handle does not fit `kind:model:name` or `model:name`.
    #[error("malformed handle `{0}`")]
    Malformed(String),
    /// The handle's leading kind is not one Theseus addresses.
    #[error("unknown handle kind `{0}`")]
    UnknownKind(String),
    /// The handle names a different model than the one in hand.
    #[error("handle `{handle}` names model `{found}`, expected `{expected}`")]
    WrongModel {
        handle: String,
        found: String,
        expected: String,
    },
}

impl Target {
    /// Parse a handle string against the model whose nodes it addresses.
    pub fn parse(model: &Model, handle: &str) -> Result<Target, HandleError> {
        let expected = model.name.to_lowercase();
        let parts: Vec<&str> = handle.splitn(3, ':').collect();
        match parts.as_slice() {
            ["model", found] => check_model(handle, found, &expected).map(|()| Target::Model),
            [kind, found, rest] => {
                check_model(handle, found, &expected)?;
                parse_kind(handle, kind, rest)
            }
            _ => Err(HandleError::Malformed(handle.to_string())),
        }
    }

    /// Render this address as its canonical handle string.
    pub fn render(&self, model: &Model) -> String {
        let m = model.name.to_lowercase();
        match self {
            Target::Model => format!("model:{m}"),
            Target::Crate(name) => format!("crate:{m}:{name}"),
            Target::Dep { crate_name, dep } => format!("dep:{m}:{crate_name}.{dep}"),
            Target::Service(name) => format!("service:{m}:{name}"),
            Target::Inbound(name) => format!("inbound:{m}:{name}"),
            Target::Operation(name) => format!("op:{m}:{name}"),
            Target::Type(name) => format!("type:{m}:{name}"),
            Target::Port(name) => format!("port:{m}:{name}"),
            Target::Method { port, name } => format!("method:{m}:{port}.{name}"),
            Target::Field { ty, name } => format!("field:{m}:{ty}.{name}"),
            Target::Variant { ty, name } => format!("variant:{m}:{ty}.{name}"),
        }
    }

    /// The word naming this address's kind, for display and filtering.
    pub fn kind_word(&self) -> &'static str {
        match self {
            Target::Model => "model",
            Target::Crate(_) => "crate",
            Target::Dep { .. } => "dependency",
            Target::Service(_) => "service",
            Target::Inbound(_) => "inbound",
            Target::Operation(_) => "operation",
            Target::Type(_) => "type",
            Target::Port(_) => "port",
            Target::Method { .. } => "method",
            Target::Field { .. } => "field",
            Target::Variant { .. } => "variant",
        }
    }
}

impl NodeKind {
    /// Parse the node-kind word an `add` names.
    pub fn parse(text: &str) -> Option<NodeKind> {
        Some(match text {
            "crate" => NodeKind::Crate,
            "dep" => NodeKind::Dep,
            "service" => NodeKind::Service,
            "inbound" => NodeKind::Inbound,
            "operation" => NodeKind::Operation,
            "type" => NodeKind::Type,
            "port" => NodeKind::Port,
            "method" => NodeKind::Method,
            "field" => NodeKind::Field,
            "variant" => NodeKind::Variant,
            _ => return None,
        })
    }

    /// The word naming this kind, for display.
    pub fn word(self) -> &'static str {
        match self {
            NodeKind::Crate => "crate",
            NodeKind::Dep => "dependency",
            NodeKind::Service => "service",
            NodeKind::Inbound => "inbound",
            NodeKind::Operation => "operation",
            NodeKind::Type => "type",
            NodeKind::Port => "port",
            NodeKind::Method => "method",
            NodeKind::Field => "field",
            NodeKind::Variant => "variant",
        }
    }
}

fn check_model(handle: &str, found: &str, expected: &str) -> Result<(), HandleError> {
    if found == expected {
        Ok(())
    } else {
        Err(HandleError::WrongModel {
            handle: handle.to_string(),
            found: found.to_string(),
            expected: expected.to_string(),
        })
    }
}

fn parse_kind(handle: &str, kind: &str, rest: &str) -> Result<Target, HandleError> {
    let nested = || {
        rest.split_once('.')
            .map(|(parent, name)| (parent.to_string(), name.to_string()))
            .ok_or_else(|| HandleError::Malformed(handle.to_string()))
    };
    match kind {
        "crate" => Ok(Target::Crate(rest.to_string())),
        "dep" => {
            let (crate_name, dep) = nested()?;
            Ok(Target::Dep { crate_name, dep })
        }
        "service" => Ok(Target::Service(rest.to_string())),
        "inbound" => Ok(Target::Inbound(rest.to_string())),
        "op" => Ok(Target::Operation(rest.to_string())),
        "type" => Ok(Target::Type(rest.to_string())),
        "port" => Ok(Target::Port(rest.to_string())),
        "method" => {
            let (port, name) = nested()?;
            Ok(Target::Method { port, name })
        }
        "field" => {
            let (ty, name) = nested()?;
            Ok(Target::Field { ty, name })
        }
        "variant" => {
            let (ty, name) = nested()?;
            Ok(Target::Variant { ty, name })
        }
        other => Err(HandleError::UnknownKind(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    #[test]
    fn parses_and_renders_top_level_handles() {
        let model = sample_model();
        let target = Target::parse(&model, "op:sample:greet").unwrap();
        assert_eq!(target, Target::Operation("greet".to_string()));
        assert_eq!(target.render(&model), "op:sample:greet");
    }

    #[test]
    fn parses_and_renders_an_inbound_handle() {
        let model = sample_model();
        let inbound = Target::Inbound("agent".to_string());
        let rendered = inbound.render(&model);
        assert_eq!(rendered, "inbound:sample:agent");
        assert_eq!(Target::parse(&model, &rendered).unwrap(), inbound);
    }

    #[test]
    fn parses_nested_handles() {
        let model = sample_model();
        let method = Target::parse(&model, "method:sample:store.write").unwrap();
        assert_eq!(
            method,
            Target::Method {
                port: "store".to_string(),
                name: "write".to_string(),
            }
        );
        assert_eq!(method.render(&model), "method:sample:store.write");
    }

    #[test]
    fn parses_the_model_root() {
        let model = sample_model();
        assert_eq!(
            Target::parse(&model, "model:sample").unwrap(),
            Target::Model
        );
    }

    #[test]
    fn rejects_a_foreign_model() {
        let model = sample_model();
        assert!(matches!(
            Target::parse(&model, "op:other:greet"),
            Err(HandleError::WrongModel { .. })
        ));
    }

    #[test]
    fn rejects_an_unknown_kind_and_a_missing_dot() {
        let model = sample_model();
        assert!(matches!(
            Target::parse(&model, "widget:sample:x"),
            Err(HandleError::UnknownKind(_))
        ));
        assert!(matches!(
            Target::parse(&model, "method:sample:store"),
            Err(HandleError::Malformed(_))
        ));
    }
}
