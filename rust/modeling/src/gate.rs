//! The refusal a write gate reports.
//!
//! An adapter that holds a write permission wraps a port and returns [`Refused`]
//! when the permission is off. The type is shared vocabulary: a transport
//! adapter downcasts it to map a refused write in its own terms — a permission
//! status on the wire, a refusal fed back into a conversation.

/// A write refused by a permission gate.
#[derive(Debug, thiserror::Error)]
#[error("writes are not permitted; rerun with write permission to apply this edit")]
pub struct Refused;
