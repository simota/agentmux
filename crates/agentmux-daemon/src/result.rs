use crate::*;

pub(crate) fn trim_result_detection_tail(output_tail: &mut String, max_tail_bytes: usize) {
    if output_tail.len() <= max_tail_bytes {
        return;
    }

    let keep_from = output_tail
        .char_indices()
        .rev()
        .find_map(|(index, _)| (output_tail.len() - index <= max_tail_bytes).then_some(index))
        .unwrap_or(0);
    output_tail.drain(..keep_from);
}

/// Stable content hash of a parsed result, scoped by the emitting agent name.
/// Built from the canonical JSON serialization so identical results hash equal.
pub(crate) fn result_content_hash(agent_name: &str, result: &AgentResult) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    agent_name.hash(&mut hasher);
    // serde_json serialization of AgentResult is deterministic for a given value
    // (struct field order is fixed), so it is a stable dedup key.
    if let Ok(canonical) = serde_json::to_string(result) {
        canonical.hash(&mut hasher);
    }
    hasher.finish()
}

