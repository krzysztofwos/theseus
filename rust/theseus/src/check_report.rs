use std::fmt;

use serde::{Deserialize, Serialize};

/// The outcome of a completed workspace check, test, or lint command.
///
/// Process-launch and I/O failures remain operation errors. A command that ran
/// to completion reports its exit status here so mutation orchestration can
/// commit or roll back without interpreting human-readable text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckReport {
    pub ok: bool,
    pub detail: String,
}

impl CheckReport {
    pub fn success(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            detail: detail.into(),
        }
    }

    pub fn failure(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for CheckReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.detail)
    }
}
