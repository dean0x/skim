//! Tests for [`AstIndexReader`].

#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use tempfile::tempdir;

use super::*;
use crate::{
    FileId,
    ast_index::store::builder::AstIndexBuilder,
    ast_index::{
        AstBigram, AstBigramEntry, AstNgramSet, AstTrigram, AstTrigramEntry, DEFAULT_AST_WEIGHT,
        StructuralMetrics,
    },
};
use rskim_core::Language;

// ============================================================================
// Helpers
// ============================================================================

fn make_bigram_set(key: u32, count: u32) -> AstNgramSet {
    AstNgramSet {
        bigrams: vec![AstBigramEntry {
            ngram: AstBigram(key),
            weight: DEFAULT_AST_WEIGHT,
            count,
        }],
        trigrams: vec![],
    }
}

fn make_trigram_set(key: u64, count: u32) -> AstNgramSet {
    AstNgramSet {
        bigrams: vec![],
        trigrams: vec![AstTrigramEntry {
            ngram: AstTrigram(key),
            weight: DEFAULT_AST_WEIGHT,
            count,
        }],
    }
}

/// Build a small 3-file index and return the dir + reader.
fn build_3_file_index() -> (tempfile::TempDir, AstIndexReader) {
    let dir = tempdir().unwrap();
    let bigram_key: u32 = 0x0001_0002;
    let trigram_key: u64 = 0x0001_0002_0003;

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // File 0: has a bigram
    let set0 = make_bigram_set(bigram_key, 3);
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set0,
            50,
            StructuralMetrics::default(),
        )
        .unwrap();

    // File 1: has both bigram and trigram
    let mut set1 = make_bigram_set(bigram_key, 5);
    set1.trigrams.push(AstTrigramEntry {
        ngram: crate::ast_index::ngram::AstTrigram(trigram_key),
        weight: DEFAULT_AST_WEIGHT,
        count: 2,
    });
    set1.bigrams.sort_unstable_by_key(|e| e.ngram.key());
    set1.trigrams.sort_unstable_by_key(|e| e.ngram.key());
    builder
        .add_file_ngrams(
            FileId(1),
            Language::Python,
            &set1,
            80,
            StructuralMetrics::default(),
        )
        .unwrap();

    // File 2: empty (non-TS language)
    let set2 = AstNgramSet::default();
    builder
        .add_file_ngrams(
            FileId(2),
            Language::Go,
            &set2,
            0,
            StructuralMetrics::default(),
        )
        .unwrap();

    let reader = builder.build().unwrap();
    (dir, reader)
}

// ============================================================================
// A1: Roundtrip — build + lookup
// ============================================================================

#[test]
fn a1_roundtrip_bigram_lookup() {
    let (dir, reader) = build_3_file_index();
    let bigram_key: u32 = 0x0001_0002;

    let postings = reader.lookup_bigram(AstBigram(bigram_key)).unwrap();
    assert_eq!(postings.len(), 2, "files 0 and 1 have this bigram");
    assert_eq!(postings[0].doc_id, 0);
    assert_eq!(postings[0].count, 3);
    assert_eq!(postings[1].doc_id, 1);
    assert_eq!(postings[1].count, 5);

    // Independent open from the same directory
    let dir_path = dir.path().to_path_buf();
    let reader2 = AstIndexReader::open(&dir_path).unwrap();
    let postings2 = reader2.lookup_bigram(AstBigram(bigram_key)).unwrap();
    assert_eq!(postings2, postings);
}

#[test]
fn a1_roundtrip_trigram_lookup() {
    let (_dir, reader) = build_3_file_index();
    let trigram_key: u64 = 0x0001_0002_0003;

    let postings = reader.lookup_trigram(AstTrigram(trigram_key)).unwrap();
    assert_eq!(postings.len(), 1, "only file 1 has this trigram");
    assert_eq!(postings[0].doc_id, 1);
    assert_eq!(postings[0].count, 2);
}

// ============================================================================
// A3: Absent key → empty, not error
// ============================================================================

#[test]
fn a3_absent_bigram_returns_empty() {
    let (_, reader) = build_3_file_index();
    let postings = reader.lookup_bigram(AstBigram(0xFFFF_FFFF)).unwrap();
    assert!(postings.is_empty(), "absent bigram should return empty vec");
}

#[test]
fn a3_absent_trigram_returns_empty() {
    let (_, reader) = build_3_file_index();
    let postings = reader
        .lookup_trigram(AstTrigram(0xFFFF_FFFF_FFFF_FFFF))
        .unwrap();
    assert!(
        postings.is_empty(),
        "absent trigram should return empty vec"
    );
}

// ============================================================================
// A5/C6: lang_id recovery via the public `language()` accessor
// ============================================================================

/// C6 happy-path: `file_meta(i).language()` returns the language the file was
/// indexed under.  This test exercises the public accessor documented in C6,
/// not the internal `lang_from_id` helper.
#[test]
fn a5_lang_recovery_from_file_meta() {
    let (_, reader) = build_3_file_index();

    // Retrieve through the public C6 accessor, not the internal helper.
    assert_eq!(
        reader.file_meta(0).unwrap().language(),
        Some(Language::Rust),
        "file 0 should be Rust via .language()"
    );
    assert_eq!(
        reader.file_meta(1).unwrap().language(),
        Some(Language::Python),
        "file 1 should be Python via .language()"
    );
    assert_eq!(
        reader.file_meta(2).unwrap().language(),
        Some(Language::Go),
        "file 2 should be Go via .language()"
    );
}

/// C6 None-path: an `AstFileMetaEntry` with an out-of-range `lang_id` must
/// return `None` from `.language()` — this is the future-compat path documented
/// in C6 (an index built by a newer binary may store IDs the current binary does
/// not recognise).
#[test]
fn a5_lang_recovery_unrecognised_lang_id_returns_none() {
    use super::super::format::AstFileMetaEntry;

    // 255 is not assigned in lang_map; it exercises the `_ => None` arm.
    let entry = AstFileMetaEntry {
        lang_id: 255,
        node_count: 0,
        max_depth: 0,
        max_block_stmts: 0,
        max_params: 0,
        branch_count: 0,
    };
    assert_eq!(
        entry.language(),
        None,
        "unrecognised lang_id 255 should return None from .language()"
    );

    // A value just past the current highest assigned ID (16 = Yaml) also
    // exercises the same None arm, guarding against a future off-by-one.
    let entry2 = AstFileMetaEntry {
        lang_id: 17,
        node_count: 0,
        max_depth: 0,
        max_block_stmts: 0,
        max_params: 0,
        branch_count: 0,
    };
    assert_eq!(
        entry2.language(),
        None,
        "unrecognised lang_id 17 should return None from .language()"
    );
}

// ============================================================================
// A6: Send + Sync
// ============================================================================

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn a6_ast_index_reader_is_send_sync() {
    assert_send_sync::<AstIndexReader>();
}

// ============================================================================
// File meta OOB → IndexCorrupted
// ============================================================================

#[test]
fn file_meta_oob_returns_corrupted() {
    let (_, reader) = build_3_file_index();
    // file_count is 3, so index 3 is out of bounds
    let err = reader.file_meta(3).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted, got: {msg}"
    );
}

// ============================================================================
// A7: Empty corpus — post_mmap is None, lookups return empty
// ============================================================================

#[test]
fn a7_empty_corpus_lookup_returns_empty() {
    let dir = tempdir().unwrap();
    let builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    builder.build().unwrap();
    let reader = AstIndexReader::open(dir.path()).unwrap();

    assert!(reader.lookup_bigram(AstBigram(1)).unwrap().is_empty());
    assert!(reader.lookup_trigram(AstTrigram(1)).unwrap().is_empty());
    assert_eq!(reader.file_count(), 0);
}

// ============================================================================
// A8: Multi-language round-trip (Issue 6)
//
// Builds an index spanning 5 languages — Rust, Python, Go, Kotlin, Java —
// covering both common and less-common variants.  Asserts that:
//   (a) `file_meta(i).language()` returns the expected language for each file.
//   (b) postings for a bigram emitted by one language are independently
//       retrievable (the n-gram data is not silently corrupted).
//
// `lang_map` is shared with the lexical index.  A regression in any of the 17
// language IDs would surface here rather than only in tests for the three
// languages covered by the fixture.
// ============================================================================

#[test]
fn a8_multi_language_round_trip() {
    let dir = tempdir().unwrap();
    let bigram_key: u32 = 0x0001_0002;

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // Five languages covering common (Rust, Python, Go) and less-common
    // (Kotlin, Java) variants.  Each file gets the same bigram key so we can
    // verify per-file postings are independently correct.
    let languages = [
        Language::Rust,
        Language::Python,
        Language::Go,
        Language::Kotlin,
        Language::Java,
    ];

    for (i, &lang) in languages.iter().enumerate() {
        let set = make_bigram_set(bigram_key, (i as u32) + 1);
        builder
            .add_file_ngrams(
                FileId(i as u32),
                lang,
                &set,
                10,
                StructuralMetrics::default(),
            )
            .unwrap();
    }

    let reader = builder.build().unwrap();

    // (a) Language round-trip via the public C6 accessor.
    for (i, &expected_lang) in languages.iter().enumerate() {
        assert_eq!(
            reader.file_meta(i as u32).unwrap().language(),
            Some(expected_lang),
            "file {i} language mismatch: expected {expected_lang:?}"
        );
    }

    // (b) Postings are independently retrievable and carry the correct counts.
    let postings = reader.lookup_bigram(AstBigram(bigram_key)).unwrap();
    assert_eq!(
        postings.len(),
        languages.len(),
        "expected one posting per file"
    );
    for (i, posting) in postings.iter().enumerate() {
        assert_eq!(posting.doc_id, i as u32, "posting doc_id mismatch at {i}");
        assert_eq!(
            posting.count,
            (i as u32) + 1,
            "posting count mismatch at {i}"
        );
    }
}

// ============================================================================
// A11: Corruption matrix
// ============================================================================

fn build_single_file_index() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    let dir = tempdir().unwrap();
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let set = make_bigram_set(0x0001_0002, 1);
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set,
            10,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder.build().unwrap();
    let idx = std::fs::read(dir.path().join("ast_index.skidx")).unwrap();
    let post = std::fs::read(dir.path().join("ast_index.skpost")).unwrap();
    (dir, idx, post)
}

#[test]
fn a11_flipped_magic_rejected() {
    let (orig_dir, mut idx, post) = build_single_file_index();
    // Overwrite magic
    idx[0] = b'X';
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("bad magic"), "expected 'bad magic' in: {msg}");
    drop(orig_dir);
}

#[test]
fn a11_flipped_version_rejected() {
    let (orig_dir, mut idx, post) = build_single_file_index();
    // Overwrite version (bytes 4..6) with 0
    idx[4] = 0;
    idx[5] = 0;
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("format version"),
        "expected 'format version' in: {msg}"
    );
    drop(orig_dir);
}

#[test]
fn a11_flipped_payload_byte_crc_rejected() {
    let (orig_dir, mut idx, post) = build_single_file_index();
    // Flip the first byte of the payload (byte HEADER_SIZE)
    if idx.len() > HEADER_SIZE {
        idx[HEADER_SIZE] ^= 0xFF;
    }
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("checksum") || msg.contains("Index corrupted"),
        "expected checksum/corrupted error, got: {msg}"
    );
    drop(orig_dir);
}

#[test]
fn a11_truncated_skidx_rejected() {
    let (orig_dir, idx, post) = build_single_file_index();
    let truncated = &idx[..idx.len() - 1];
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), truncated).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Index corrupted") || msg.contains("truncated") || msg.contains("mismatch"),
        "expected corrupted/truncated/mismatch error, got: {msg}"
    );
    drop(orig_dir);
}

#[test]
fn a11_truncated_skpost_rejected() {
    let (orig_dir, idx, post) = build_single_file_index();
    if post.is_empty() {
        // No postings — truncation test not applicable
        drop(orig_dir);
        return;
    }
    let truncated = &post[..post.len() - 1];
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), truncated).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Index corrupted") || msg.contains("mismatch"),
        "expected corrupted/mismatch error, got: {msg}"
    );
    drop(orig_dir);
}

#[test]
fn a11_appended_byte_to_skidx_rejected() {
    let (orig_dir, mut idx, post) = build_single_file_index();
    idx.push(0xFF);
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Index corrupted") || msg.contains("mismatch"),
        "expected corrupted/mismatch error for extra byte, got: {msg}"
    );
    drop(orig_dir);
}

// ============================================================================
// A12: Cross-magic — open a dir with only the lexical index files
// ============================================================================

#[test]
fn a12_cross_magic_lexical_files_rejected() {
    let dir = tempdir().unwrap();
    // Create files named like the lexical index, not the AST index
    std::fs::write(dir.path().join("index.skidx"), b"SKIX\x02\x00").unwrap();
    std::fs::write(dir.path().join("index.skpost"), b"").unwrap();
    // Opening the AST index should fail with Io (file not found) because
    // ast_index.skidx does not exist
    let err = AstIndexReader::open(dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("IO error") || msg.contains("No such file"),
        "expected Io/NotFound error, got: {msg}"
    );
}

// ============================================================================
// A13: Missing files
// ============================================================================

#[test]
fn a13_missing_skidx_returns_io_error() {
    let dir = tempdir().unwrap();
    let err = AstIndexReader::open(dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("IO error") || msg.contains("No such file"),
        "expected IO/not-found error, got: {msg}"
    );
}

// ============================================================================
// A14: Overflow header — huge counts → checked arithmetic → IndexCorrupted
// ============================================================================

#[test]
fn a14_overflow_header_huge_bigram_count() {
    let (orig_dir, mut idx, post) = build_single_file_index();

    // Overwrite bigram_count (bytes 6..10) with u32::MAX
    let huge: u32 = u32::MAX;
    idx[6..10].copy_from_slice(&huge.to_le_bytes());
    // Recompute checksum over the now-modified payload
    // (we need to also update trigram_count and file_count to preserve structure,
    // but for this test we just want to exercise the size-mismatch path)
    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();
    let err = AstIndexReader::open(corrupt_dir.path()).unwrap_err();
    let msg = format!("{err}");
    // Should fail with IndexCorrupted (checksum mismatch or size mismatch),
    // NOT with a panic or OOB access
    assert!(
        msg.contains("Index corrupted") || msg.contains("mismatch") || msg.contains("overflow"),
        "expected safe IndexCorrupted error for overflow header, got: {msg}"
    );
    drop(orig_dir);
}

// ============================================================================
// A16: index size ratio < 2.2× source bytes (raised from <1.8× in v2)
// v1 measured baseline: ~1.23×.  v2 adds structural n-grams and +10 bytes/file
// meta overhead; expected v2 ratio ~1.3×.  Guard raised to <2.2× per PF-005:
// relaxation justified by measured ratio + structural capability expansion.
// Applies ADR-003 (grounded regression guard).  On-disk compression tracked
// in issue #273.
// ============================================================================

/// Generate a representative Rust source module with `n_fns` functions.
///
/// Delegates to the shared [`crate::test_corpus::gen_representative_rust_module`]
/// helper (hoisted in #358 per quality.md reuse-over-new) so that both the AST
/// index tests and the lexical index tests exercise the same corpus definition.
fn gen_representative_rust_module(file_idx: usize, n_fns: usize) -> String {
    crate::test_corpus::gen_representative_rust_module(file_idx, n_fns)
}

#[test]
fn ast_index_size_ratio() {
    // Build an index from 1000 representative Rust modules (4 functions each,
    // ~480 bytes/file → ~480 KB total source).  Each function has a real
    // multi-statement body so the corpus is representative of real-world Rust.
    //
    // WHY THE ORIGINAL <5% (0.05×) BOUND WAS WRONG
    // -----------------------------------------------
    // The 5% target had no empirical basis and is structurally impossible for
    // AST n-gram indexes.  Two reasons:
    //
    //   1. Dense posting vocabulary: structural AST n-grams (node-kind pairs /
    //      triples) have a tiny vocabulary (~hundreds of distinct keys for Rust)
    //      but every occurrence in every file produces a posting entry.  For a
    //      1 000-file corpus each bigram/trigram yields O(files) postings, so
    //      posting data alone grows at roughly the same order as source bytes —
    //      not a fraction of it.
    //
    //   2. No size test in the lexical sibling: the lexical index (index.rs)
    //      never made a size-ratio claim either.  The 5% figure was copied
    //      from a note about token savings (context-window reduction), not
    //      on-disk index compactness, and was never validated.
    //
    // REALISTIC EXPECTATION
    // ----------------------
    // Measured ratio on this representative corpus: ~1.23× source bytes
    // (skidx ≈ 8 KB of header/entries + skpost ≈ 1.3 MB of raw posting data
    //  vs. ~1.06 MB source).  Industry code-search indexes (Zoekt / Sourcegraph
    //  trigram) typically run 3–5× source, so a raw uncompressed AST posting
    //  file at 1.2× is already competitive.
    //
    // ON-DISK COMPRESSION (delta encoding + VarInt / Roaring Bitmaps) that
    // would push the ratio well below 1× is tracked in issue #273.
    let dir = tempdir().unwrap();

    let n_files = 1000usize;
    let fns_per_file = 4usize;
    let sources: Vec<String> = (0..n_files)
        .map(|i| gen_representative_rust_module(i, fns_per_file))
        .collect();

    let files: Vec<(FileId, &str, Language)> = sources
        .iter()
        .enumerate()
        .map(|(i, s)| (FileId(i as u32), s.as_str(), Language::Rust))
        .collect();

    AstIndexBuilder::build_from_files(dir.path().to_path_buf(), &files).unwrap();

    let idx_len = std::fs::metadata(dir.path().join("ast_index.skidx"))
        .unwrap()
        .len();
    let post_len = std::fs::metadata(dir.path().join("ast_index.skpost"))
        .unwrap()
        .len();
    let total_index_bytes = idx_len + post_len;
    let total_source_bytes: u64 = sources.iter().map(|s| s.len() as u64).sum();

    let ratio = total_index_bytes as f64 / total_source_bytes as f64;
    eprintln!(
        "A16 size ratio: {ratio:.4} \
         (index={total_index_bytes} bytes, source={total_source_bytes} bytes, \
         skidx={idx_len} bytes, skpost={post_len} bytes)"
    );
    // Guard against genuine bloat regressions (e.g. accidental O(files²)
    // posting growth).  Measured v1 baseline: ~1.23× on this representative
    // 1000-file corpus.
    //
    // v2 size note (Wave 3e): AstFileMetaEntry grew from 5 to 15 bytes
    // (+10 bytes/file × 1000 files = +10 KB on a ~1.06 MB source corpus).
    // Additionally, synthetic structural n-grams (EMPTY_BODY, DEEP_NODE,
    // LARGE_BODY, MANY_PARAMS) add posting entries — measured v2 ratio ~1.3×
    // on the same corpus.  The guard is raised from <1.8× to <2.2× to absorb
    // this deliberate capability expansion.
    //
    // Justification for relaxation (PF-005 compliance): the +0.4× headroom
    // covers the synthetic n-gram posting overhead plus the meta entry growth.
    // Any ratio above the v1 ~1.23× baseline is accounted for by the v2
    // structural markers; a genuine O(files²) bloat regression would push
    // the ratio well above 2.2× and still fires.
    //
    // ADR-003: regression guard must be empirically grounded.
    //
    // ON-DISK COMPRESSION (delta encoding + VarInt / Roaring Bitmaps) that
    // would push the ratio well below 1× is tracked in issue #273.
    assert!(
        ratio < 2.2,
        "index size ratio {ratio:.4} exceeds the <2.2× bloat guard \
         (v2 expected ~1.3× with structural markers, v1 baseline ~1.23×). \
         If ratio exceeded: check for O(files²) posting growth or unbounded \
         synthetic n-gram emission. \
         index={total_index_bytes} bytes, source={total_source_bytes} bytes"
    );
}

// ============================================================================
// F9: index_version probe — returns the stored version without full open()
//
// Acceptance criterion F9: "index_version(dir) returns 2 for a v2 index AND
// surfaces a v1 index as needing rebuild (tested via a hand-written v1 header
// fixture)."
//
// The function reads only magic (4 bytes) + version (2 bytes) and returns
// Ok(version) for any file with valid SKAX magic. It does NOT reject v1 — that
// is the responsibility of AstIndexReader::open() / decode_header(). The
// probe is a cheap staleness check; the caller decides what to do with the value.
// ============================================================================

/// F9 positive: index_version returns Ok(FORMAT_VERSION) == Ok(2) for a real
/// v2 index built by AstIndexBuilder.
#[test]
fn f9_index_version_returns_2_for_v2_index() {
    let (dir, _reader) = build_3_file_index();
    let version = AstIndexReader::index_version(dir.path()).unwrap();
    assert_eq!(
        version,
        super::super::format::FORMAT_VERSION,
        "index_version must return FORMAT_VERSION (2) for a freshly built index"
    );
    assert_eq!(
        version, 2u16,
        "FORMAT_VERSION is expected to be 2 in this wave"
    );
}

/// F9 negative — v1 fixture:
///   (a) index_version returns Ok(1) — the probe reads the stored version without
///       rejecting it; rejection is the job of open() / decode_header().
///   (b) AstIndexReader::open() on the same fixture fails with "please rebuild
///       the AST index" — the full reader enforces the version gate.
///
/// The v1 header is hand-written to mirror the byte layout used by
/// a3_reader_rejects_v1_header in format_tests.rs: magic b"SKAX" (4 bytes)
/// followed by version=1 as u16-LE (2 bytes), remaining bytes zeroed.
#[test]
fn f9_index_version_surfaces_v1_fixture() {
    let dir = tempdir().unwrap();

    // Hand-craft a minimal v1 index file: magic + version=1, rest zeroed.
    // 48 bytes matches HEADER_SIZE so that open() reaches the version check
    // rather than failing on a short-read.
    let mut v1_header = [0u8; 48];
    v1_header[0..4].copy_from_slice(b"SKAX");
    v1_header[4..6].copy_from_slice(&1u16.to_le_bytes()); // version = 1

    std::fs::write(dir.path().join("ast_index.skidx"), v1_header).unwrap();

    // (a) index_version should read Ok(1) — the probe does not enforce version.
    let version = AstIndexReader::index_version(dir.path()).unwrap();
    assert_eq!(
        version, 1u16,
        "index_version must return Ok(1) for a v1 fixture — it reads the stored \
         version without rejecting it"
    );

    // (b) Full open() must reject v1 with the "please rebuild" hint.
    let err = AstIndexReader::open(dir.path()).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("please rebuild the AST index"),
        "open() on a v1 fixture must contain 'please rebuild the AST index', got: {msg}"
    );
    assert!(
        msg.contains("format version"),
        "open() on a v1 fixture must contain 'format version', got: {msg}"
    );
}

// ============================================================================
// C3: posting bounds / alignment guards in lookup_postings_generic
//
// The existing A11 corruption-matrix tests flip bytes in `.skidx` but do NOT
// recompute the CRC, so the CRC gate in `open()` short-circuits before any
// posting offset is dereferenced.  These tests corrupt TABLE entries AND
// recompute the CRC so that `lookup_bigram` / `lookup_trigram` is actually
// reached, exercising the four distinct guards in `lookup_postings_generic`:
//
//  (a) posting_offset exceeds post_mmap.len() (OOB)
//  (b) posting slice arithmetic overflow: start + length wraps usize
//  (c) posting_length is not a multiple of POSTING_ENTRY_SIZE (misaligned)
//  (d) posting doc_ids are not strictly ascending (non-monotone)
//
// On 64-bit targets `usize::try_from(u64)` never fails, so there is no
// distinct "offset exceeds usize" path.  The OOB guard (a) covers that
// conceptual case on all platforms.
// ============================================================================

use super::super::format::{
    AstBigramTableEntry, AstTrigramTableEntry, BIGRAM_ENTRY_SIZE, HEADER_SIZE, POSTING_ENTRY_SIZE,
    TRIGRAM_ENTRY_SIZE, compute_checksum, encode_bigram_entry, encode_trigram_entry,
};

/// Build a valid single-file index with one bigram and one trigram, return
/// (dir, idx_bytes, post_bytes).  The dir is kept alive by the caller.
fn build_c3_index() -> (tempfile::TempDir, Vec<u8>, Vec<u8>) {
    let dir = tempdir().unwrap();
    let bigram_key: u32 = 0x0001_0002;
    let trigram_key: u64 = 0x0001_0002_0003;

    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    let mut set = make_bigram_set(bigram_key, 1);
    set.trigrams.push(AstTrigramEntry {
        ngram: AstTrigram(trigram_key),
        weight: DEFAULT_AST_WEIGHT,
        count: 2,
    });
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set,
            10,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder.build().unwrap();

    let idx = std::fs::read(dir.path().join("ast_index.skidx")).unwrap();
    let post = std::fs::read(dir.path().join("ast_index.skpost")).unwrap();
    (dir, idx, post)
}

/// Rewrite the CRC field (bytes [44..48]) in `idx` to match the current
/// payload (`idx[HEADER_SIZE..]`), so that `open()` passes the CRC gate and
/// the lookup path is reached.
fn recompute_crc(idx: &mut [u8]) {
    let checksum = compute_checksum(&idx[HEADER_SIZE..]);
    idx[44..48].copy_from_slice(&checksum.to_le_bytes());
}

/// Overwrite the bigram TABLE entry at byte offset `HEADER_SIZE` with the
/// given `posting_offset` and `posting_length`, then recompute the CRC.
fn corrupt_bigram_entry(idx: &mut [u8], posting_offset: u64, posting_length: u32) {
    // First bigram entry starts at HEADER_SIZE.  Read the existing key so we
    // preserve it (we only want to corrupt the offset/length fields).
    let key_bytes: [u8; 4] = idx[HEADER_SIZE..HEADER_SIZE + 4].try_into().unwrap();
    let key = u32::from_le_bytes(key_bytes);

    let entry = AstBigramTableEntry {
        key,
        posting_offset,
        posting_length,
    };
    let encoded = encode_bigram_entry(&entry);
    idx[HEADER_SIZE..HEADER_SIZE + BIGRAM_ENTRY_SIZE].copy_from_slice(&encoded);
    recompute_crc(idx);
}

/// Overwrite the trigram TABLE entry (which starts after all bigram entries)
/// with the given `posting_offset` and `posting_length`, then recompute the CRC.
fn corrupt_trigram_entry(idx: &mut [u8], posting_offset: u64, posting_length: u32) {
    // Header encodes bigram_count at bytes [6..10].
    let bc_bytes: [u8; 4] = idx[6..10].try_into().unwrap();
    let bigram_count = u32::from_le_bytes(bc_bytes) as usize;
    let trigram_start = HEADER_SIZE + bigram_count * BIGRAM_ENTRY_SIZE;

    // Read the existing trigram key so we preserve it.
    let key_bytes: [u8; 8] = idx[trigram_start..trigram_start + 8].try_into().unwrap();
    let key = u64::from_le_bytes(key_bytes);

    let entry = AstTrigramTableEntry {
        key,
        posting_offset,
        posting_length,
    };
    let encoded = encode_trigram_entry(&entry);
    idx[trigram_start..trigram_start + TRIGRAM_ENTRY_SIZE].copy_from_slice(&encoded);
    recompute_crc(idx);
}

/// Write modified (idx, post) bytes to a fresh tempdir and attempt to open +
/// lookup; return the `SearchError` message.
fn open_and_lookup_bigram(idx: &[u8], post: &[u8], key: u32) -> String {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("ast_index.skidx"), idx).unwrap();
    std::fs::write(dir.path().join("ast_index.skpost"), post).unwrap();
    let reader = AstIndexReader::open(dir.path()).unwrap();
    let err = reader.lookup_bigram(AstBigram(key)).unwrap_err();
    format!("{err}")
}

fn open_and_lookup_trigram(idx: &[u8], post: &[u8], key: u64) -> String {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("ast_index.skidx"), idx).unwrap();
    std::fs::write(dir.path().join("ast_index.skpost"), post).unwrap();
    let reader = AstIndexReader::open(dir.path()).unwrap();
    let err = reader.lookup_trigram(AstTrigram(key)).unwrap_err();
    format!("{err}")
}

/// (a) posting_offset beyond the end of post_mmap → IndexCorrupted (OOB).
///
/// On 64-bit targets this also covers the conceptual "posting_offset exceeds
/// usize" guard because `usize == u64` there; u64::MAX as the offset exercises
/// both checks simultaneously (it exceeds post_mmap.len() and would overflow
/// `start + length`).
#[test]
fn c3_bigram_oob_posting_offset_returns_index_corrupted() {
    let (_dir, mut idx, post) = build_c3_index();
    // Set posting_offset to a value well beyond the post file's size.
    corrupt_bigram_entry(&mut idx, u64::MAX / 2, POSTING_ENTRY_SIZE as u32);

    let msg = open_and_lookup_bigram(&idx, &post, 0x0001_0002);
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for OOB bigram offset, got: {msg}"
    );
}

#[test]
fn c3_trigram_oob_posting_offset_returns_index_corrupted() {
    let (_dir, mut idx, post) = build_c3_index();
    corrupt_trigram_entry(&mut idx, u64::MAX / 2, POSTING_ENTRY_SIZE as u32);

    let msg = open_and_lookup_trigram(&idx, &post, 0x0001_0002_0003);
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for OOB trigram offset, got: {msg}"
    );
}

/// (b) posting slice arithmetic overflow: start + length would overflow usize.
///
/// usize::MAX as the offset makes `start` == usize::MAX; adding any positive
/// length then overflows `checked_add`, triggering the overflow guard.
/// (On 64-bit targets usize::try_from(u64) succeeds for this value, so this
/// specifically exercises the `checked_add` overflow branch.)
#[test]
fn c3_bigram_slice_overflow_returns_index_corrupted() {
    let (_dir, mut idx, post) = build_c3_index();
    // posting_offset == usize::MAX as u64; adding any length overflows start + length.
    corrupt_bigram_entry(&mut idx, usize::MAX as u64, POSTING_ENTRY_SIZE as u32);

    let msg = open_and_lookup_bigram(&idx, &post, 0x0001_0002);
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for bigram slice overflow, got: {msg}"
    );
}

#[test]
fn c3_trigram_slice_overflow_returns_index_corrupted() {
    let (_dir, mut idx, post) = build_c3_index();
    corrupt_trigram_entry(&mut idx, usize::MAX as u64, POSTING_ENTRY_SIZE as u32);

    let msg = open_and_lookup_trigram(&idx, &post, 0x0001_0002_0003);
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for trigram slice overflow, got: {msg}"
    );
}

/// (c) posting_length not a multiple of POSTING_ENTRY_SIZE (8) → IndexCorrupted.
#[test]
fn c3_bigram_misaligned_posting_length_returns_index_corrupted() {
    let (_dir, mut idx, post) = build_c3_index();
    // posting_offset = 0 (valid), posting_length = 3 (not a multiple of 8)
    corrupt_bigram_entry(&mut idx, 0, 3);

    let msg = open_and_lookup_bigram(&idx, &post, 0x0001_0002);
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for misaligned bigram length, got: {msg}"
    );
}

#[test]
fn c3_trigram_misaligned_posting_length_returns_index_corrupted() {
    let (_dir, mut idx, post) = build_c3_index();
    corrupt_trigram_entry(&mut idx, 0, 3);

    let msg = open_and_lookup_trigram(&idx, &post, 0x0001_0002_0003);
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for misaligned trigram length, got: {msg}"
    );
}

/// (e) posting doc_id >= file_count → IndexCorrupted (out-of-range doc_id).
///
/// The CRC covers only `.skidx`, NOT `.skpost`, so corrupting `.skpost` is
/// invisible to CRC validation.  A CRC-valid hostile index can embed a doc_id
/// that is out of range for the `file_meta` / `file_metrics` arrays; this guard
/// catches it before any downstream array access.
#[test]
fn c3_out_of_range_doc_id_returns_index_corrupted() {
    // Build a single-file index (file_count = 1, so doc_id == 1 is out of range).
    let dir = tempdir().unwrap();
    let bigram_key: u32 = 0x0001_0002;
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let set = make_bigram_set(bigram_key, 1);
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set,
            10,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder.build().unwrap();

    let idx = std::fs::read(dir.path().join("ast_index.skidx")).unwrap();
    let mut post = std::fs::read(dir.path().join("ast_index.skpost")).unwrap();

    // Overwrite the sole posting entry's doc_id (first 4 bytes) to 1, which is
    // >= file_count (1).  The CRC does not cover skpost so open() still succeeds.
    assert!(
        post.len() >= POSTING_ENTRY_SIZE,
        "expected at least one posting entry in post file"
    );
    post[0..4].copy_from_slice(&1u32.to_le_bytes()); // doc_id = 1, out of range

    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();

    let reader = AstIndexReader::open(corrupt_dir.path()).unwrap();
    let err = reader.lookup_bigram(AstBigram(bigram_key)).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for out-of-range doc_id, got: {msg}"
    );
    assert!(
        msg.contains("out of range"),
        "expected 'out of range' in error message, got: {msg}"
    );
}

/// (d) posting doc_ids not strictly ascending → IndexCorrupted (non-monotone check).
///
/// Build a two-file index, then manually write a posting list where the second
/// doc_id equals the first (non-strictly-ascending).  The CRC covers only
/// `.skidx`, NOT `.skpost`, so corrupting `.skpost` is invisible to CRC
/// validation and reaches the monotonicity check in `lookup_postings_generic`.
#[test]
fn c3_non_ascending_doc_ids_returns_index_corrupted() {
    // Build a 2-file index so there are 2 postings for the bigram key.
    let dir = tempdir().unwrap();
    let bigram_key: u32 = 0x0001_0002;
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();
    let set = make_bigram_set(bigram_key, 1);
    builder
        .add_file_ngrams(
            FileId(0),
            Language::Rust,
            &set,
            10,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder
        .add_file_ngrams(
            FileId(1),
            Language::Rust,
            &set,
            10,
            StructuralMetrics::default(),
        )
        .unwrap();
    builder.build().unwrap();

    let idx = std::fs::read(dir.path().join("ast_index.skidx")).unwrap();
    let mut post = std::fs::read(dir.path().join("ast_index.skpost")).unwrap();

    // The posting list has two 8-byte entries: (doc_id=0, count=1) (doc_id=1, count=1).
    // Corrupt the second entry's doc_id to 0 — same as the first — so the list
    // is no longer strictly ascending.
    assert!(
        post.len() >= 2 * POSTING_ENTRY_SIZE,
        "expected at least 2 posting entries in post file"
    );
    // Second posting doc_id is at post[8..12] (POSTING_ENTRY_SIZE = 8, doc_id = first 4 bytes).
    post[POSTING_ENTRY_SIZE..POSTING_ENTRY_SIZE + 4].copy_from_slice(&0u32.to_le_bytes());

    let corrupt_dir = tempdir().unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skidx"), &idx).unwrap();
    std::fs::write(corrupt_dir.path().join("ast_index.skpost"), &post).unwrap();

    let reader = AstIndexReader::open(corrupt_dir.path()).unwrap();
    let err = reader.lookup_bigram(AstBigram(bigram_key)).unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("Index corrupted"),
        "expected IndexCorrupted for non-ascending doc_ids, got: {msg}"
    );
}

// ============================================================================
// AC2 (#286): file_lang_and_node_count matches file_meta, shares byte offsets
// ============================================================================

/// AC2: For every in-range doc_id, `file_lang_and_node_count(d)` returns
/// `(file_meta(d).lang_id, file_meta(d).node_count)`.  For an out-of-range
/// doc_id both return the same `Err(IndexCorrupted)` variant.
///
/// The byte offsets are read through the shared `decode_lang_and_node_count`
/// helper in `format.rs`; this test guards against drift between the two
/// code paths.
#[test]
fn ac2_file_lang_and_node_count_matches_file_meta_real_reader() {
    let dir = tempdir().unwrap();
    let bigram_key: u32 = 0x0001_0002;
    let mut builder = AstIndexBuilder::new(dir.path().to_path_buf()).unwrap();

    // Four files with distinct languages and node counts.
    let fixtures = [
        (Language::Rust, 42u32),
        (Language::Python, 100u32),
        (Language::Go, 7u32),
        (Language::TypeScript, 999u32),
    ];
    for (i, (lang, nc)) in fixtures.iter().enumerate() {
        let set = make_bigram_set(bigram_key, 1);
        builder
            .add_file_ngrams(
                FileId(i as u32),
                *lang,
                &set,
                *nc,
                crate::ast_index::StructuralMetrics::default(),
            )
            .unwrap();
    }
    let reader = builder.build().unwrap();

    // In-range: (lang_id, node_count) must equal file_meta's fields.
    for i in 0..fixtures.len() as u32 {
        let doc_id = i;
        let meta = reader.file_meta(doc_id).unwrap();
        let (lang_id, nc) = reader.file_lang_and_node_count(doc_id).unwrap();
        assert_eq!(
            lang_id, meta.lang_id,
            "lang_id mismatch for doc_id={doc_id}: file_lang_and_node_count={lang_id}, file_meta={}",
            meta.lang_id
        );
        assert_eq!(
            nc, meta.node_count,
            "node_count mismatch for doc_id={doc_id}: file_lang_and_node_count={nc}, file_meta={}",
            meta.node_count
        );
    }

    // Out-of-range (file_count == 4, so doc_id=4 is OOB).
    let oob = fixtures.len() as u32;
    let err_meta = reader.file_meta(oob).unwrap_err();
    let err_lite = reader.file_lang_and_node_count(oob).unwrap_err();
    assert!(
        matches!(err_meta, crate::SearchError::IndexCorrupted(_)),
        "file_meta OOB should be IndexCorrupted: {err_meta:?}"
    );
    assert!(
        matches!(err_lite, crate::SearchError::IndexCorrupted(_)),
        "file_lang_and_node_count OOB should be IndexCorrupted: {err_lite:?}"
    );
}

/// AC2 (format.rs): `decode_file_meta` and `decode_lang_and_node_count` must
/// agree on `lang_id` and `node_count` for a known byte buffer.
#[test]
fn ac2_decode_helpers_agree_on_known_buffer() {
    use super::super::format::{FILE_META_SIZE, decode_file_meta, decode_lang_and_node_count};

    // Craft a valid 15-byte AstFileMetaEntry buffer.
    // lang_id = 11 (Rust), node_count = 42 (LE: 42, 0, 0, 0).
    let mut buf = [0u8; FILE_META_SIZE];
    buf[0] = 11; // lang_id = Rust
    buf[1..5].copy_from_slice(&42u32.to_le_bytes()); // node_count
    buf[5..7].copy_from_slice(&3u16.to_le_bytes()); // max_depth
    buf[7..9].copy_from_slice(&5u16.to_le_bytes()); // max_block_stmts
    buf[9..11].copy_from_slice(&2u16.to_le_bytes()); // max_params
    buf[11..15].copy_from_slice(&1u32.to_le_bytes()); // branch_count

    let meta = decode_file_meta(&buf).unwrap();
    let (lang_id, node_count) = decode_lang_and_node_count(&buf).unwrap();

    assert_eq!(lang_id, meta.lang_id, "lang_id must agree");
    assert_eq!(node_count, meta.node_count, "node_count must agree");
    assert_eq!(lang_id, 11, "expected Rust lang_id=11");
    assert_eq!(node_count, 42, "expected node_count=42");
}
