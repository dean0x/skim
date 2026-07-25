//! Live-zone selection and candidate-set computation (#304 Phase 2).
//!
//! # Live-zone definition
//!
//! The live zone is the set of content blocks that belong to messages AFTER the
//! last `assistant` message in the conversation. These blocks are candidates for
//! compression because they represent the current user input — not established
//! context that the model has already processed.
//!
//! If the final message has role `assistant`, the live zone is EMPTY (no
//! compression — the request is complete from the model's perspective).
//!
//! If there is NO `assistant` message at all, the whole array is treated as live
//! (auto-resolved, per the no-prior-context heuristic in #301 boundary rule D7).
//!
//! # AD-004 — block_id grammar
//!
//! The block_id grammar is defined by `rskim_llm` as an internal `format!`
//! string: `m{i}` / `m{i}b{j}` / `m{i}b{j}l{k}`. This module parses the
//! leading `m{N}` token to extract `msg_idx`. The parse is DEFENSIVE:
//!
//! - A block_id that does not match `^m\d+...` is treated as non-live
//!   → passthrough (fail-safe, never dropped, never errored).
//!
//! This string-parse coupling is a known risk (R3 in the plan). A follow-up
//! ticket #343 will publish a typed `msg_idx` accessor on `BlockDescriptor`
//! so this parse can be deleted. Until then, this module is the single point
//! of coupling to the `format!` grammar and the regression test in
//! `tests/zone_tests.rs` pins the grammar so any drift in `rskim_llm` breaks
//! the test immediately.
//!
//! # AD-002 — Local zone computation
//!
//! `rskim_contract::zone::{apply_live_zone_edits, locate_hot_zone_range}` are
//! stubs returning `None` pending #306/#307's offset model. This module computes
//! the live zone itself from the read-only `body.messages()` slice, which is
//! sufficient because block-leaf substitution preserves hot-zone bytes by
//! exclusion (AD-003): we simply never call `mutate_block` for a hot-zone id.
//!
//! # AD-003 — Hot-zone safety by exclusion
//!
//! Because `mutate_block` only ever edits the named block, NOT selecting a
//! hot-zone block_id is sufficient to guarantee byte-identity for all prior
//! turns (AC14/AC18). The negative test in `tests/zone_tests.rs` plants
//! compressible-looking blocks in prior turns and asserts they are untouched.
//!
//! # AC-27 — Three-way candidate join
//!
//! A block is a candidate iff ALL three conditions hold:
//! 1. Its `msg_idx` is in the live zone (parsed from the block_id).
//! 2. Its `BlockDescriptor.mutable == true` (from `list_blocks`).
//! 3. It appears in the `classify_body` results (i.e., it has a classifiable
//!    text payload).
//!
//! A block present in only one or two of these collections, or with
//! `mutable == false`, is forwarded byte-identical (not a candidate).

use rskim_llm::{Classification, ParsedBody, classify_body, list_blocks};

/// A block selected as a candidate for compression.
///
/// A candidate block has passed the three-way join: it is in the live zone,
/// mutable, and classified. The caller receives both the classification
/// (for engine routing) and the block_id (for `mutate_block`).
#[derive(Debug, Clone)]
pub(crate) struct Candidate {
    /// The block_id (as returned by `list_blocks` and `classify_body`).
    pub(crate) block_id: String,
    /// The classification result (used to select the compression engine).
    pub(crate) classification: Classification,
}

/// Compute the live zone boundary: the `msg_idx` after which blocks are live.
///
/// Returns `Some(last_assistant_idx)` if an assistant message is present.
/// Returns `None` if no assistant message is found (whole array is live).
///
/// The live zone is `msg_idx > last_assistant_idx`. If the last message has
/// role `assistant`, the live zone is empty (no messages satisfy `> last`).
///
/// # Provider-specific access
///
/// The `ParsedBody` enum does not have a unified `messages()` accessor.
/// This function matches on the provider variant and accesses the messages
/// slice via the provider-specific method (Anthropic: `AnthropicBody::messages()`;
/// OpenAI: `OpenAiBody::messages()`). There is no `ParsedBody::messages()`.
pub(crate) fn last_assistant_index(body: &ParsedBody) -> Option<usize> {
    // There is NO `ParsedBody::messages()` — must match per provider.
    // ParsedBody is #[non_exhaustive] — wildcard arm required for future variants.
    match body {
        ParsedBody::Anthropic(b) => b
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "assistant")
            .map(|(i, _)| i)
            .next_back(),
        ParsedBody::OpenAi(b) => b
            .messages()
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "assistant")
            .map(|(i, _)| i)
            .next_back(),
        // Future provider variants: treat as no assistant present (whole array live).
        _ => None,
    }
}

/// Parse the `msg_idx` from a block_id string.
///
/// The grammar is `m{N}` / `m{N}b{J}` / `m{N}b{J}l{K}` (AD-004 / #343).
///
/// Returns `Some(N)` on success. Returns `None` for any id that does not
/// begin with `m` followed by one or more ASCII digits. Non-matching ids
/// are treated as non-live → passthrough (fail-safe: never dropped, never
/// errored).
///
/// ## Regression note
///
/// This parse is pinned by a test in `tests/zone_tests.rs` that verifies the
/// grammar against ids produced by `rskim_llm::list_blocks` and
/// `rskim_llm::classify_body`. If the grammar in `rskim_llm` changes, that
/// test breaks immediately. See #343 for the follow-up to publish a typed
/// accessor and delete this function.
pub(crate) fn parse_msg_idx(block_id: &str) -> Option<usize> {
    // Must start with 'm'
    let rest = block_id.strip_prefix('m')?;
    // Parse the run of digits that follows (up to the next non-digit or end)
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if digits_end == 0 {
        // 'm' with no following digits — not matching grammar
        return None;
    }
    rest[..digits_end].parse::<usize>().ok()
}

/// Compute the candidate set for a parsed request body.
///
/// Returns the list of blocks that satisfy the three-way join:
/// - Live zone: `msg_idx > last_assistant_idx` (or all if no assistant present).
/// - Mutable: `BlockDescriptor.mutable == true`.
/// - Classified: appears in `classify_body` results.
///
/// Any block_id that does not parse as a valid `m{N}...` id is excluded
/// (treated as non-live, fail-safe per AD-004).
///
/// # OpenAI bodies
///
/// For OpenAI bodies, `list_blocks` returns an empty list → the join yields
/// ZERO candidates. This is correct-by-current-implementation (no mutable
/// blocks on OpenAI) and is pinned by a test in `tests/zone_tests.rs` (AC27).
///
/// # Empty live zone
///
/// If the final message has role `assistant`, the live zone is empty → all
/// candidates are excluded → zero candidates (no compression, full passthrough).
pub(crate) fn compute_candidates(body: &ParsedBody) -> Vec<Candidate> {
    // Step 1: determine the live-zone boundary (AD-002).
    let last_assistant = last_assistant_index(body);

    // Step 2: get mutable blocks (AC27: three-way join source 1).
    // OpenAI → empty list → zero candidates, AC17.
    let mut_blocks = list_blocks(body);

    // Fast exit: if there are no mutable blocks (e.g., OpenAI), skip everything.
    if mut_blocks.is_empty() {
        return Vec::new();
    }

    // Build a set of live-zone, mutable block_ids from list_blocks.
    // We keep this as a HashMap for O(1) lookup during the classify join.
    let mutable_live_ids: std::collections::HashSet<&str> = mut_blocks
        .iter()
        .filter(|desc| desc.mutable)
        .filter(|desc| {
            // AD-004: parse msg_idx from block_id; non-matching → non-live → excluded.
            match parse_msg_idx(&desc.id) {
                None => false, // fail-safe: treat as non-live
                Some(msg_idx) => match last_assistant {
                    // No assistant message → whole array is live (D7).
                    None => true,
                    // Final message is assistant → empty live zone.
                    // AD-002: live = msg_idx > last_assistant_idx.
                    Some(last) => msg_idx > last,
                },
            }
        })
        .map(|desc| desc.id.as_str())
        .collect();

    if mutable_live_ids.is_empty() {
        return Vec::new();
    }

    // Step 3: classify all text payloads (AC27: three-way join source 2).
    // Join: keep only blocks that are BOTH in mutable_live_ids AND classified.
    classify_body(body)
        .into_iter()
        .filter(|(id, _)| mutable_live_ids.contains(id.as_str()))
        .map(|(id, classification)| Candidate {
            block_id: id,
            classification,
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    // Minimal valid Anthropic body with multiple messages.
    fn anthropic_body_json(messages: &[(&str, &str)]) -> Vec<u8> {
        let msgs: Vec<String> = messages
            .iter()
            .map(|(role, content)| format!(r#"{{"role":"{role}","content":{content}}}"#))
            .collect();
        format!(
            r#"{{"model":"claude-3-5-sonnet-20241022","messages":[{}],"max_tokens":100}}"#,
            msgs.join(",")
        )
        .into_bytes()
    }

    fn anthropic_with_code(messages: &[(&str, &str)]) -> ParsedBody {
        let json = anthropic_body_json(messages);
        rskim_llm::parse(&json).expect("parse failed")
    }

    // =========================================================================
    // parse_msg_idx tests — pin the AD-004 grammar (regression guard for #343)
    // =========================================================================

    #[test]
    fn parse_msg_idx_simple_m0() {
        assert_eq!(parse_msg_idx("m0"), Some(0));
    }

    #[test]
    fn parse_msg_idx_m1b2() {
        assert_eq!(parse_msg_idx("m1b2"), Some(1));
    }

    #[test]
    fn parse_msg_idx_m0b1l2() {
        assert_eq!(parse_msg_idx("m0b1l2"), Some(0));
    }

    #[test]
    fn parse_msg_idx_large_index() {
        assert_eq!(parse_msg_idx("m42b0"), Some(42));
    }

    #[test]
    fn parse_msg_idx_no_prefix_returns_none() {
        // Fail-safe: non-matching → None → non-live → passthrough.
        assert_eq!(parse_msg_idx("b0"), None);
        assert_eq!(parse_msg_idx(""), None);
        assert_eq!(parse_msg_idx("xm0"), None);
    }

    #[test]
    fn parse_msg_idx_m_only_no_digits_returns_none() {
        // 'm' with no digits following is not a valid id.
        assert_eq!(parse_msg_idx("m"), None);
        assert_eq!(parse_msg_idx("mb0"), None);
    }

    #[test]
    fn parse_msg_idx_real_rskim_llm_ids() {
        // Verify against the IDs produced by rskim_llm::list_blocks and
        // classify_body on a real parsed body (grammar regression pin, #343).
        let body = anthropic_with_code(&[
            ("user", r#""hello""#),
            ("assistant", r#""world""#),
            ("user", r#""follow-up""#),
        ]);
        let descriptors = rskim_llm::list_blocks(&body);
        let classified = rskim_llm::classify_body(&body);

        // Every id from list_blocks must parse as a valid msg_idx.
        for desc in &descriptors {
            let idx = parse_msg_idx(&desc.id);
            assert!(
                idx.is_some(),
                "list_blocks id {:?} did not parse as m{{N}}...",
                desc.id
            );
        }
        // Every id from classify_body must also parse.
        for (id, _) in &classified {
            let idx = parse_msg_idx(id);
            assert!(
                idx.is_some(),
                "classify_body id {:?} did not parse as m{{N}}...",
                id
            );
        }
    }

    // =========================================================================
    // last_assistant_index tests
    // =========================================================================

    #[test]
    fn last_assistant_index_no_assistant_returns_none() {
        let body = anthropic_with_code(&[("user", r#""hello""#)]);
        assert_eq!(last_assistant_index(&body), None);
    }

    #[test]
    fn last_assistant_index_with_assistant() {
        let body = anthropic_with_code(&[
            ("user", r#""hello""#),
            ("assistant", r#""reply""#),
            ("user", r#""follow-up""#),
        ]);
        // Last assistant is at index 1.
        assert_eq!(last_assistant_index(&body), Some(1));
    }

    #[test]
    fn last_assistant_index_final_message_is_assistant() {
        let body = anthropic_with_code(&[("user", r#""hello""#), ("assistant", r#""reply""#)]);
        // Last message is assistant → live zone empty.
        assert_eq!(last_assistant_index(&body), Some(1));
    }

    #[test]
    fn last_assistant_index_multiple_assistants() {
        let body = anthropic_with_code(&[
            ("user", r#""q1""#),
            ("assistant", r#""a1""#),
            ("user", r#""q2""#),
            ("assistant", r#""a2""#),
            ("user", r#""q3""#),
        ]);
        // Last assistant is at index 3.
        assert_eq!(last_assistant_index(&body), Some(3));
    }

    // =========================================================================
    // compute_candidates tests
    // =========================================================================

    #[test]
    fn openai_body_yields_zero_candidates() {
        // AC27 / AC17: OpenAI list_blocks → empty → join yields 0 candidates.
        let json = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"```rust\nfn main(){}\n```"}]}"#;
        let body = rskim_llm::parse(json).expect("parse");
        let candidates = compute_candidates(&body);
        assert_eq!(
            candidates.len(),
            0,
            "OpenAI bodies must yield zero candidates (AC27/AC17)"
        );
    }

    #[test]
    fn final_assistant_message_yields_empty_live_zone() {
        // AC16 arm (a): assistant-final → no candidates.
        let body = anthropic_with_code(&[
            ("user", r#""```rust\nfn main() {}\n```""#),
            ("assistant", r#""reply""#),
        ]);
        let candidates = compute_candidates(&body);
        assert_eq!(
            candidates.len(),
            0,
            "Final-assistant message → empty live zone → zero candidates"
        );
    }

    #[test]
    fn no_assistant_message_whole_array_is_live() {
        // AC16 arm (b): no assistant → whole array live.
        let code = r#""```rust\nfn main() {}\n```""#;
        let body = anthropic_with_code(&[("user", code)]);
        let candidates = compute_candidates(&body);
        // m0 should be a candidate (mutable, live, classified as Code).
        assert!(
            !candidates.is_empty(),
            "With no assistant message, user turn must be live"
        );
        assert!(
            candidates.iter().any(|c| c.block_id == "m0"),
            "m0 must be a candidate"
        );
    }

    #[test]
    fn live_zone_excludes_prior_turns() {
        // AD-003: blocks in hot-zone (prior turns) must NOT be candidates.
        let code_block = r#""```rust\nfn main() {}\n```""#;
        let body = anthropic_with_code(&[
            ("user", code_block),        // m0 — hot zone (prior)
            ("assistant", r#""reply""#), // m1 — assistant
            ("user", code_block),        // m2 — live zone
        ]);
        let candidates = compute_candidates(&body);
        // m0 is in the hot zone → not a candidate.
        assert!(
            !candidates.iter().any(|c| c.block_id == "m0"),
            "m0 (hot zone) must not be a candidate (AD-003)"
        );
        // m2 is in the live zone → should be a candidate.
        assert!(
            candidates.iter().any(|c| c.block_id == "m2"),
            "m2 (live zone) must be a candidate"
        );
    }

    #[test]
    fn non_matching_block_id_treated_as_non_live() {
        // AD-004 fail-safe: a synthetic id that doesn't match m{N}... is excluded.
        // We test this indirectly by verifying parse_msg_idx returns None for
        // non-matching ids (the compute_candidates filter relies on this).
        assert_eq!(parse_msg_idx("synthetic_id"), None);
        assert_eq!(parse_msg_idx("block_0"), None);
        assert_eq!(parse_msg_idx(""), None);
    }
}
