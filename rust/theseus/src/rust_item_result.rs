use serde::{Deserialize, Serialize};

use crate::CheckReport;

/// The outcome of editing one authorized top-level Rust item under a compile gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RustItemResult {
    /// Whether the edited source survived the compile gate and was committed.
    pub applied: bool,
    /// Workspace-relative authored Rust file considered by the operation.
    pub path: String,
    /// Stable kind-and-name identity of the inserted or replaced item.
    pub item: String,
    /// Complete-file revision after commit, or the unchanged revision after rollback.
    pub revision: String,
    /// Human-readable description of the edit or rollback.
    pub detail: String,
    /// The completed compile check that decided commit versus rollback.
    pub check: CheckReport,
}
