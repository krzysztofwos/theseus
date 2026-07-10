use serde::{Deserialize, Serialize};

use crate::CheckReport;

/// The outcome of authoring a handler or adapter method under a compile gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImplementResult {
    /// Whether the authored source survived the compile gate and was committed.
    pub applied: bool,
    /// Workspace-relative authored file considered by the operation.
    pub path: String,
    /// Human-readable description of what was written or why it was rolled back.
    pub detail: String,
    /// The completed compile check that decided commit versus rollback.
    pub check: CheckReport,
}
