//! Stable content hashing of a [`Model`](crate::model::Model).
//!
//! The hash is the agent protocol's anchor: `query` reports it, `patch` checks
//! it (`--expect-model-hash`) before mutating, so concurrent edits collide
//! loudly. FNV-1a over the canonical JSON encoding gives a result that is
//! deterministic across runs and platforms and changes whenever the model does,
//! which is all the protocol needs.

use crate::model::Model;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// Hex-encoded FNV-1a hash of the model's canonical JSON form.
pub fn model_hash(model: &Model) -> String {
    // `Model` serializes deterministically: fields in declaration order, no
    // unordered maps. So the byte stream — and therefore the hash — is stable.
    let canonical = serde_json::to_vec(model).expect("Model always serializes");
    let mut hash = FNV_OFFSET;
    for byte in canonical {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::sample_model;

    #[test]
    fn hash_is_stable_across_calls() {
        let model = sample_model();
        assert_eq!(model_hash(&model), model_hash(&model));
    }

    #[test]
    fn hash_changes_when_model_changes() {
        let model = sample_model();
        let mut mutated = model.clone();
        mutated.name.push('X');
        assert_ne!(model_hash(&model), model_hash(&mutated));
    }
}
