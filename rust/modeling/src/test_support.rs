//! Shared fixtures for engine unit tests.

use crate::model::{CrateNode, Model, Operation, Port, Service};

/// A small, self-contained model for exercising the engine.
pub(crate) fn sample_model() -> Model {
    Model {
        name: "Sample".to_string(),
        crates: vec![CrateNode {
            name: "sample".to_string(),
            dir: "sample".to_string(),
            layer: 0,
            depends_on: vec![],
        }],
        types: vec![],
        services: vec![Service {
            name: "Sample".to_string(),
            crate_name: "sample".to_string(),
            operations: vec![
                operation("greet", "Greet."),
                operation("status", "Report status."),
            ],
            outbound: vec![Port {
                name: "store".to_string(),
                summary: "A store.".to_string(),
                target: None,
                methods: vec![],
            }],
        }],
        inbounds: vec![],
    }
}

fn operation(name: &str, summary: &str) -> Operation {
    Operation {
        name: name.to_string(),
        summary: summary.to_string(),
        request: "Empty".to_string(),
        response: "Empty".to_string(),
    }
}
