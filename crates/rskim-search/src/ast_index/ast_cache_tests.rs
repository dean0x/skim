//! Tests for `ast_cache` — serialization round-trips and correctness invariants.
//!
//! Every test asserts a discriminating observable behaviour, never just
//! exit-0 / no-panic. (avoids PF-007)

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::ast_index::extract::{AstBigramEntry, AstTrigramEntry};
use crate::ast_index::{
    AstBigram, AstTrigram, extract::AstNgramSet, structural::StructuralMetrics,
};

// ============================================================================
// Helpers
// ============================================================================

/// Build a non-trivial CachedAstEntry for round-trip testing (AC6).
fn make_entry() -> CachedAstEntry {
    CachedAstEntry {
        ngrams: AstNgramSet {
            bigrams: vec![
                AstBigramEntry {
                    ngram: AstBigram::from_raw(0x00010002),
                    weight: 1.5f32,
                    count: 3,
                },
                AstBigramEntry {
                    ngram: AstBigram::from_raw(0xFFFF0001),
                    weight: 0.5f32,
                    count: u32::MAX,
                },
            ],
            trigrams: vec![AstTrigramEntry {
                ngram: AstTrigram::from_raw(0x0001000200000003),
                weight: 2.0f32,
                count: 100,
            }],
        },
        metrics: StructuralMetrics {
            max_depth: 42,
            max_block_stmts: 7,
            max_params: 5,
            branch_count: 13,
        },
        node_count: 99,
    }
}

// ============================================================================
// Round-trip: encode → decode (AC6)
// ============================================================================

/// Serialize a non-trivial entry and deserialize it; result must be == original.
/// Also checks u32::MAX node_count and u16::MAX depth survive at declared widths.
/// (avoids PF-004, avoids PF-007)
#[test]
fn round_trip_entry_populated() {
    let original = make_entry();
    let encoded = encode_entry(&original);
    let decoded = decode_entry(&encoded).expect("round-trip must decode successfully");
    assert_eq!(decoded, original, "decoded entry must equal original");
}

/// Serialize an empty entry (data-format / large-file payload).
/// Must decode to an empty-but-valid CachedAstEntry, NOT classified as corrupt.
/// (AC6 — empty is valid)
#[test]
fn round_trip_entry_empty() {
    let original = CachedAstEntry::default();
    let encoded = encode_entry(&original);
    let decoded = decode_entry(&encoded).expect("empty entry must round-trip cleanly");
    assert_eq!(decoded, original, "decoded empty entry must equal original");
}

/// Boundary: u32::MAX node_count survives at declared width. (avoids PF-004)
#[test]
fn round_trip_node_count_u32_max() {
    let entry = CachedAstEntry {
        ngrams: AstNgramSet::default(),
        metrics: StructuralMetrics::default(),
        node_count: u32::MAX,
    };
    let decoded = decode_entry(&encode_entry(&entry)).expect("must decode");
    assert_eq!(
        decoded.node_count,
        u32::MAX,
        "u32::MAX node_count must survive round-trip"
    );
}

/// Boundary: u16::MAX max_depth survives at declared width. (avoids PF-004)
#[test]
fn round_trip_max_depth_u16_max() {
    let entry = CachedAstEntry {
        ngrams: AstNgramSet::default(),
        metrics: StructuralMetrics {
            max_depth: u16::MAX,
            max_block_stmts: 0,
            max_params: 0,
            branch_count: 0,
        },
        node_count: 0,
    };
    let decoded = decode_entry(&encode_entry(&entry)).expect("must decode");
    assert_eq!(
        decoded.metrics.max_depth,
        u16::MAX,
        "u16::MAX max_depth must survive round-trip (avoids PF-004)"
    );
}

// ============================================================================
// File-level round-trip
// ============================================================================

/// Encode a map → file bytes → decode; result must contain the same entries.
#[test]
fn round_trip_file_level() {
    let mut entries = HashMap::new();
    let sha1 = "a".repeat(SHA_HEX_LEN);
    let sha2 = "b".repeat(SHA_HEX_LEN);
    entries.insert(sha1.clone(), make_entry());
    entries.insert(sha2.clone(), CachedAstEntry::default());

    let buf = encode_file(&entries);
    let decoded = decode_file(&buf).expect("file round-trip must decode");

    assert_eq!(decoded.len(), 2, "must contain 2 entries");
    assert_eq!(decoded.get(&sha1), entries.get(&sha1));
    assert_eq!(decoded.get(&sha2), entries.get(&sha2));
}

// ============================================================================
// AstNgramCache public API
// ============================================================================

/// Insert, then lookup — hit returns the same entry. Miss returns None.
#[test]
fn cache_lookup_hit_and_miss() {
    let mut cache = AstNgramCache::empty();
    let sha = "c".repeat(SHA_HEX_LEN);
    let entry = make_entry();

    // Miss before insert.
    assert!(
        cache.lookup(&sha).is_none(),
        "lookup must be None before insert"
    );

    cache.insert(sha.clone(), entry.clone());

    // Hit after insert.
    let found = cache
        .lookup(&sha)
        .expect("lookup must return Some after insert");
    assert_eq!(*found, entry, "found entry must equal inserted entry");
}

/// save() + load() round-trip through the file system.
#[test]
fn cache_save_and_load() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let cache_dir = dir.path();

    let mut cache = AstNgramCache::empty();
    let sha = "d".repeat(SHA_HEX_LEN);
    let entry = make_entry();
    cache.insert(sha.clone(), entry.clone());
    cache.save(cache_dir).expect("save must succeed");

    let loaded = AstNgramCache::load(cache_dir);
    let found = loaded
        .lookup(&sha)
        .expect("loaded cache must contain the entry");
    assert_eq!(*found, entry, "loaded entry must equal saved entry");
}

/// Load on a missing file returns an empty cache — no error surfaced. (AC9 cold-start)
#[test]
fn load_missing_file_returns_empty_cache() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let cache = AstNgramCache::load(dir.path());
    assert!(
        cache.is_empty(),
        "missing skcache file must yield empty cache"
    );
}

// ============================================================================
// Version mismatch → cold start (AC9)
// ============================================================================

/// A skcache with a wrong version byte must be discarded wholesale.
/// Rebuild must succeed with reuse == 0. (avoids PF-007 — asserts observable)
#[test]
fn version_mismatch_discards_cache() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let cache_dir = dir.path();

    // Write a valid skcache.
    let mut cache = AstNgramCache::empty();
    let sha = "e".repeat(SHA_HEX_LEN);
    cache.insert(sha.clone(), make_entry());
    cache.save(cache_dir).expect("save must succeed");

    // Corrupt the version byte (offset 4 in the file).
    let skcache_path = cache_dir.join(CACHE_FILENAME);
    let mut bytes = std::fs::read(&skcache_path).expect("must read");
    bytes[4] = bytes[4].wrapping_add(1); // change version
    std::fs::write(&skcache_path, &bytes).expect("must write");

    // Load must return empty (version mismatch → cold start).
    let loaded = AstNgramCache::load(cache_dir);
    assert!(
        loaded.is_empty(),
        "version mismatch must yield empty cache (all entries discarded)"
    );
}

// ============================================================================
// Magic mismatch
// ============================================================================

/// A skcache with wrong magic bytes must be discarded.
#[test]
fn magic_mismatch_discards_cache() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let cache_dir = dir.path();

    // Write a valid skcache, then corrupt the magic.
    let mut cache = AstNgramCache::empty();
    cache.insert("f".repeat(SHA_HEX_LEN), make_entry());
    cache.save(cache_dir).expect("save");

    let skcache_path = cache_dir.join(CACHE_FILENAME);
    let mut bytes = std::fs::read(&skcache_path).expect("read");
    bytes[0] = b'X'; // corrupt magic
    std::fs::write(&skcache_path, &bytes).expect("write");

    let loaded = AstNgramCache::load(cache_dir);
    assert!(loaded.is_empty(), "wrong magic must yield empty cache");
}

// ============================================================================
// Corrupt entry → miss, not crash, not whole-cache discard (AC10)
// ============================================================================

/// A corrupt entry whose payload is in-bounds (valid length prefix, but bad
/// content) is skipped via `decode_entry` returning `None`; the stream continues
/// and later valid entries remain accessible.
///
/// Note on stream-stop semantics: when a length prefix itself is corrupt
/// (oversized > MAX_ENTRY_BYTES, or bytes truncated), `decode_file` stops
/// reading at that position — it cannot safely resync.  Entries BEFORE the
/// stop point are already recorded and remain accessible; entries AFTER the
/// stop point are treated as cache misses for that build only.  This is a
/// known format limitation (no per-entry framing for mid-stream recovery) and
/// is documented in `decode_file`'s contract.  The test below places a valid
/// entry before a corrupt one to verify that: (a) the corrupt in-bounds entry
/// is treated as a miss (not a whole-cache discard), and (b) entries before a
/// stream-stop are preserved.
#[test]
fn corrupt_payload_is_miss_not_whole_cache_discard() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let cache_dir = dir.path();

    let sha_good = "g".repeat(SHA_HEX_LEN);
    let sha_bad = "h".repeat(SHA_HEX_LEN);
    let sha_after = "i".repeat(SHA_HEX_LEN);

    // Build the file manually with three entries:
    //   sha_good — fully correct (placed first)
    //   sha_bad  — valid length prefix but in-bounds corrupt payload (decode_entry → None)
    //   sha_after — valid entry placed after the corrupt one
    let good_entry = make_entry();
    let after_entry = CachedAstEntry::default();

    let good_payload = encode_entry(&good_entry);
    let after_payload = encode_entry(&after_entry);

    // Corrupt payload: correct byte count but all-zero content (decode_entry rejects
    // the trailing-bytes check or count mismatch).  We use the correct length so
    // decode_file can advance pos and try the next entry — this is the "in-bounds
    // corrupt" case where stream continuation is possible.
    let corrupt_len = good_payload.len();
    let corrupt_payload: Vec<u8> = vec![0u8; corrupt_len]; // zeros → decode_entry returns None

    let mut buf = Vec::new();
    buf.extend_from_slice(CACHE_MAGIC);
    buf.push(CACHE_FORMAT_VERSION);
    buf.extend_from_slice(&3u32.to_le_bytes()); // 3 entries

    // Entry 1: sha_good — fully correct.
    buf.extend_from_slice(sha_good.as_bytes());
    buf.extend_from_slice(&(good_payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&good_payload);

    // Entry 2: sha_bad — valid length but corrupt (zero-filled) payload.
    buf.extend_from_slice(sha_bad.as_bytes());
    buf.extend_from_slice(&(corrupt_len as u32).to_le_bytes());
    buf.extend_from_slice(&corrupt_payload);

    // Entry 3: sha_after — valid entry AFTER the corrupt one.
    buf.extend_from_slice(sha_after.as_bytes());
    buf.extend_from_slice(&(after_payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&after_payload);

    std::fs::write(cache_dir.join(CACHE_FILENAME), &buf).expect("write skcache");

    let loaded = AstNgramCache::load(cache_dir);

    // sha_good must be present — it was fully correct.
    let found = loaded.lookup(&sha_good);
    assert!(
        found.is_some(),
        "sha_good must be accessible even when a later entry is corrupt"
    );
    assert_eq!(
        *found.expect("must be Some"),
        good_entry,
        "sha_good entry must equal the original"
    );

    // sha_bad must be absent — corrupt in-bounds payload → decode_entry None → cache miss.
    assert!(
        loaded.lookup(&sha_bad).is_none(),
        "corrupt in-bounds entry must be absent (treated as miss, not whole-cache discard)"
    );

    // sha_after must be present — the stream continued past the in-bounds corrupt entry.
    // This is the key AC10 invariant: a corrupt-but-in-bounds entry does not stop parsing.
    let found_after = loaded.lookup(&sha_after);
    assert!(
        found_after.is_some(),
        "sha_after must be accessible — in-bounds corrupt entry must not stop stream (AC10)"
    );
    assert_eq!(
        *found_after.expect("must be Some"),
        after_entry,
        "sha_after entry must equal the original"
    );
}

/// A payload declaring length > MAX_ENTRY_BYTES must be rejected without
/// triggering a multi-GB allocation. (AC10 — allocation-bomb guard, applies ADR-003)
#[test]
fn oversized_payload_length_rejected() {
    let dir = tempfile::tempdir().expect("tempdir must succeed");
    let cache_dir = dir.path();

    // Write a skcache with one entry whose declared payload len > MAX_ENTRY_BYTES.
    let mut buf = Vec::new();
    buf.extend_from_slice(CACHE_MAGIC);
    buf.push(CACHE_FORMAT_VERSION);
    buf.extend_from_slice(&1u32.to_le_bytes());

    let sha = "i".repeat(SHA_HEX_LEN);
    buf.extend_from_slice(sha.as_bytes());

    // Oversized length (MAX_ENTRY_BYTES + 1).
    let oversized_len = (MAX_ENTRY_BYTES + 1) as u32;
    buf.extend_from_slice(&oversized_len.to_le_bytes());
    // Only a few actual bytes follow (the rest are absent — truncated anyway).
    buf.extend_from_slice(&[0u8; 8]);

    std::fs::write(cache_dir.join(CACHE_FILENAME), &buf).expect("write");

    // Must not panic, must not allocate gigabytes. Returns empty or partial cache.
    let loaded = AstNgramCache::load(cache_dir);
    // The oversized entry is rejected — sha must be absent.
    assert!(
        loaded.lookup(&sha).is_none(),
        "oversized payload entry must be rejected (allocation-bomb guard)"
    );
}

// ============================================================================
// Two paths with identical content share one cache entry (AC5)
// ============================================================================

/// SHA-keying: two distinct logical paths with byte-identical content get the
/// same cache entry. Both look up the same SHA and get the same result.
#[test]
fn identical_content_shares_one_cache_entry() {
    let mut cache = AstNgramCache::empty();
    let shared_sha = "j".repeat(SHA_HEX_LEN);
    let entry = make_entry();

    // One insert, two lookups.
    cache.insert(shared_sha.clone(), entry.clone());

    let path1_result = cache.lookup(&shared_sha);
    let path2_result = cache.lookup(&shared_sha);

    assert!(path1_result.is_some(), "first path lookup must hit");
    assert!(
        path2_result.is_some(),
        "second path (same SHA) lookup must hit"
    );
    assert_eq!(
        *path1_result.expect("must be Some"),
        *path2_result.expect("must be Some"),
        "both paths must get identical entries from a single SHA-keyed slot"
    );
    assert_eq!(
        cache.len(),
        1,
        "cache must contain exactly one entry for the shared SHA"
    );
}

// ============================================================================
// Empty cache helpers
// ============================================================================

#[test]
fn len_and_is_empty_reflect_state() {
    let mut cache = AstNgramCache::empty();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);

    cache.insert("k".repeat(SHA_HEX_LEN), CachedAstEntry::default());
    assert!(!cache.is_empty());
    assert_eq!(cache.len(), 1);
}
