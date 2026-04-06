//! Integration tests for `lexical::index_format`.
//!
//! Each test exercises behaviour at the I/O boundary. No implementation details
//! are probed; only externally observable results are asserted.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use rskim_search::lexical::{
    index_format::{DeltaReader, DeltaWriter, IndexReader, Tombstones},
    IndexHeader, Ngram, PostingEntry, INDEX_FORMAT_VERSION, INDEX_MAGIC,
};

// ============================================================================
// Helpers
// ============================================================================

fn make_header(ngram_count: u64, file_count: u64) -> IndexHeader {
    IndexHeader {
        magic: INDEX_MAGIC,
        version: INDEX_FORMAT_VERSION,
        ngram_count,
        file_count,
        created_at: 0,
    }
}

fn make_posting(doc_id: u32, field_id: u8, position: u32, tf: u16) -> PostingEntry {
    PostingEntry {
        doc_id,
        field_id,
        position,
        tf,
    }
}

fn ngram(text: &[u8]) -> Ngram {
    Ngram::from_bytes(text)
}

// ============================================================================
// 1. write_index + IndexReader — empty index
// ============================================================================

/// An empty index (0 ngrams, 0 files) produces valid files that open cleanly.
#[test]
fn write_and_open_empty_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let header = make_header(0, 0);
    let entries: Vec<(Ngram, Vec<PostingEntry>)> = vec![];

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open failed");
    assert_eq!(reader.header().ngram_count, 0);
    assert_eq!(reader.header().file_count, 0);
    assert_eq!(reader.header().magic, INDEX_MAGIC);
    assert_eq!(reader.header().version, INDEX_FORMAT_VERSION);
}

// ============================================================================
// 2. write_index + IndexReader — single ngram, single posting
// ============================================================================

/// Minimal roundtrip: one ngram, one posting entry.
#[test]
fn write_and_lookup_single_ngram() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"fn");
    let posting = make_posting(0, 2 /* SymbolName */, 100, 3);

    let entries = vec![(ng, vec![posting])];
    let header = make_header(1, 1);

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open failed");
    let results = reader.lookup(ng).expect("lookup returned None");

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_id, 0);
    assert_eq!(results[0].field_id, 2);
    assert_eq!(results[0].position, 100);
    assert_eq!(results[0].tf, 3);
}

// ============================================================================
// 3. write_index + IndexReader — multiple ngrams, binary search
// ============================================================================

/// Multiple ngrams are written and each can be found by binary search.
#[test]
fn write_and_lookup_multiple_ngrams() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng_fn = ngram(b"fn");
    let ng_let = ngram(b"le");
    let ng_pub = ngram(b"pu");

    // Sort by hash to satisfy write_index's precondition
    let mut entries = vec![
        (ng_fn, vec![make_posting(0, 1, 10, 1)]),
        (ng_let, vec![make_posting(1, 4, 20, 2)]),
        (ng_pub, vec![make_posting(2, 0, 30, 1)]),
    ];
    entries.sort_by_key(|(n, _)| n.as_u64());
    // Update ngram_count to match
    let header = make_header(entries.len() as u64, 3);

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open failed");

    // All three ngrams must be found
    assert!(reader.lookup(ng_fn).is_some(), "fn ngram missing");
    assert!(reader.lookup(ng_let).is_some(), "let ngram missing");
    assert!(reader.lookup(ng_pub).is_some(), "pub ngram missing");

    // An ngram not in the index must return None
    let absent = ngram(b"zz");
    // Only test if hash doesn't accidentally collide with one of the above
    if absent.as_u64() != ng_fn.as_u64()
        && absent.as_u64() != ng_let.as_u64()
        && absent.as_u64() != ng_pub.as_u64()
    {
        assert!(
            reader.lookup(absent).is_none(),
            "absent ngram should be None"
        );
    }
}

// ============================================================================
// 4. write_index + IndexReader — large posting list (10K entries)
// ============================================================================

/// A single ngram with 10,000 posting entries survives a roundtrip.
#[test]
fn write_and_lookup_large_posting_list() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"aa");
    let postings: Vec<PostingEntry> = (0u32..10_000)
        .map(|i| make_posting(i % 1000, 2, i * 4, 1))
        .collect();
    let expected_len = postings.len();

    let header = make_header(1, 1000);
    let entries = vec![(ng, postings)];

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open failed");
    let results = reader.lookup(ng).expect("lookup returned None");

    assert_eq!(results.len(), expected_len);
}

// ============================================================================
// 5. CorruptedIndex — bad magic bytes
// ============================================================================

/// Overwriting magic bytes causes `CorruptedIndex` on open.
#[test]
fn open_with_bad_magic_returns_corrupted_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let header = make_header(0, 0);
    rskim_search::lexical::index_format::write_index(dir.path(), &[], &header)
        .expect("write_index failed");

    // Corrupt the magic bytes
    let skidx_path = dir.path().join("lexical.skidx");
    let mut bytes = std::fs::read(&skidx_path).expect("read file");
    bytes[0] = b'X';
    std::fs::write(&skidx_path, &bytes).expect("write file");

    let result = IndexReader::open(dir.path());
    assert!(result.is_err(), "expected error on bad magic");
    let err_str = match result {
        Err(e) => format!("{e}"),
        Ok(_) => panic!("expected error"),
    };
    assert!(
        err_str.contains("magic") || err_str.to_lowercase().contains("corrupt"),
        "error should mention magic or corrupt, got: {err_str}"
    );
}

// ============================================================================
// 6. CorruptedIndex — truncated file
// ============================================================================

/// A truncated `.skidx` file causes `CorruptedIndex` on open.
#[test]
fn open_truncated_file_returns_corrupted_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let header = make_header(0, 0);
    rskim_search::lexical::index_format::write_index(dir.path(), &[], &header)
        .expect("write_index failed");

    // Truncate to just 10 bytes (less than header size)
    let skidx_path = dir.path().join("lexical.skidx");
    let bytes = std::fs::read(&skidx_path).expect("read file");
    std::fs::write(&skidx_path, &bytes[..10]).expect("write file");

    let result = IndexReader::open(dir.path());
    assert!(result.is_err(), "expected error on truncated file");
}

// ============================================================================
// 7. CorruptedIndex — version mismatch
// ============================================================================

/// A mismatched version field causes `CorruptedIndex` with a clear message.
#[test]
fn open_with_wrong_version_returns_corrupted_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let header = make_header(0, 0);
    rskim_search::lexical::index_format::write_index(dir.path(), &[], &header)
        .expect("write_index failed");

    // Overwrite the version field (bytes 4..8) with version 99
    let skidx_path = dir.path().join("lexical.skidx");
    let mut bytes = std::fs::read(&skidx_path).expect("read file");
    bytes[4..8].copy_from_slice(&99u32.to_le_bytes());
    std::fs::write(&skidx_path, &bytes).expect("write file");

    let result = IndexReader::open(dir.path());
    assert!(result.is_err(), "expected error on version mismatch");
    let err = match result {
        Err(e) => format!("{e}"),
        Ok(_) => panic!("expected error"),
    };
    assert!(
        err.contains("version") || err.to_lowercase().contains("corrupt"),
        "error should mention version, got: {err}"
    );
}

// ============================================================================
// 8. DeltaReader::open — empty delta file returns None
// ============================================================================

/// An empty delta file (0 bytes) causes `DeltaReader::open` to return `None`.
#[test]
fn delta_reader_open_empty_file_returns_none() {
    let dir = tempfile::tempdir().expect("tempdir");

    // Create an empty delta file
    let delta_path = dir.path().join("lexical.delta");
    std::fs::write(&delta_path, b"").expect("create empty delta");

    let result = DeltaReader::open(dir.path()).expect("open should succeed");
    assert!(result.is_none(), "empty delta file must return None");
}

/// A missing delta file causes `DeltaReader::open` to return `None` (not an error).
#[test]
fn delta_reader_open_missing_file_returns_none() {
    let dir = tempfile::tempdir().expect("tempdir");
    let result = DeltaReader::open(dir.path()).expect("open should succeed");
    assert!(result.is_none(), "missing delta must return None");
}

// ============================================================================
// 9. DeltaWriter + DeltaReader — roundtrip
// ============================================================================

/// Entries written by `DeltaWriter` can be retrieved by `DeltaReader::scan`.
#[test]
fn delta_writer_reader_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"if");
    let posting = make_posting(5, 1, 200, 4);

    let mut writer = DeltaWriter::open_or_create(dir.path()).expect("DeltaWriter::open_or_create");
    writer.append(&[(ng, posting)]).expect("append failed");
    drop(writer);

    let reader = DeltaReader::open(dir.path())
        .expect("open failed")
        .expect("reader should be Some");
    let results = reader.scan(ng);

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].doc_id, 5);
    assert_eq!(results[0].field_id, 1);
    assert_eq!(results[0].position, 200);
    assert_eq!(results[0].tf, 4);
}

/// `DeltaWriter::append` can be called multiple times; all entries accumulate.
#[test]
fn delta_writer_appends_accumulate() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ng = ngram(b"xy");

    let p1 = make_posting(0, 0, 10, 1);
    let p2 = make_posting(1, 2, 20, 2);

    {
        let mut writer =
            DeltaWriter::open_or_create(dir.path()).expect("DeltaWriter::open_or_create");
        writer.append(&[(ng, p1)]).expect("append 1");
        // Close and reopen (simulating separate invocations)
        drop(writer);

        let mut writer2 =
            DeltaWriter::open_or_create(dir.path()).expect("DeltaWriter::open_or_create 2");
        writer2.append(&[(ng, p2)]).expect("append 2");
    }

    let reader = DeltaReader::open(dir.path())
        .expect("open failed")
        .expect("reader should be Some");
    let results = reader.scan(ng);

    assert_eq!(results.len(), 2, "both appended entries must be present");
}

/// `DeltaReader::scan` filters by ngram hash; unrelated entries are not returned.
#[test]
fn delta_reader_scan_filters_by_ngram() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ng_a = ngram(b"ab");
    let ng_b = ngram(b"cd");

    // Ensure they have different hashes (almost guaranteed, but guard anyway)
    if ng_a.as_u64() == ng_b.as_u64() {
        return; // hash collision — skip test rather than assert false negative
    }

    let mut writer = DeltaWriter::open_or_create(dir.path()).expect("DeltaWriter::open_or_create");
    writer
        .append(&[(ng_a, make_posting(0, 1, 0, 1))])
        .expect("append a");
    writer
        .append(&[(ng_b, make_posting(1, 2, 10, 2))])
        .expect("append b");
    drop(writer);

    let reader = DeltaReader::open(dir.path()).expect("open").expect("Some");
    let results_a = reader.scan(ng_a);
    let results_b = reader.scan(ng_b);

    assert_eq!(results_a.len(), 1, "only ng_a's entry");
    assert_eq!(results_a[0].doc_id, 0);
    assert_eq!(results_b.len(), 1, "only ng_b's entry");
    assert_eq!(results_b[0].doc_id, 1);
}

// ============================================================================
// 10. Tombstones — empty set save/load roundtrip
// ============================================================================

/// An empty `Tombstones` can be saved and loaded cleanly.
#[test]
fn tombstones_empty_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ts = Tombstones::default();
    ts.save(dir.path()).expect("save failed");

    let loaded = Tombstones::load(dir.path()).expect("load failed");
    assert!(loaded.is_empty());
    assert_eq!(loaded.len(), 0);
}

/// `Tombstones::load` returns empty when no file exists.
#[test]
fn tombstones_load_missing_file_returns_empty() {
    let dir = tempfile::tempdir().expect("tempdir");
    let ts = Tombstones::load(dir.path()).expect("load failed");
    assert!(ts.is_empty());
}

// ============================================================================
// 11. Tombstones — contains with binary search
// ============================================================================

#[test]
fn tombstones_contains_uses_binary_search() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ts = Tombstones::default();
    ts.add(10);
    ts.add(20);
    ts.add(30);
    ts.add(5);
    ts.add(15);

    ts.save(dir.path()).expect("save");
    let loaded = Tombstones::load(dir.path()).expect("load");

    assert!(loaded.contains(5));
    assert!(loaded.contains(10));
    assert!(loaded.contains(15));
    assert!(loaded.contains(20));
    assert!(loaded.contains(30));
    assert!(!loaded.contains(0));
    assert!(!loaded.contains(7));
    assert!(!loaded.contains(100));
}

// ============================================================================
// 12. Tombstones — add is idempotent (no duplicates)
// ============================================================================

#[test]
fn tombstones_add_is_idempotent() {
    let mut ts = Tombstones::default();
    ts.add(42);
    ts.add(42);
    ts.add(42);
    assert_eq!(ts.len(), 1, "duplicate adds must not grow the set");
    assert!(ts.contains(42));
}

// ============================================================================
// 13. Tombstones — large set roundtrip
// ============================================================================

#[test]
fn tombstones_large_set_roundtrip() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut ts = Tombstones::default();

    // Add 1000 doc_ids in reverse order (tests sorting on load)
    for i in (0u32..1000).rev() {
        ts.add(i);
    }
    assert_eq!(ts.len(), 1000);

    ts.save(dir.path()).expect("save");
    let loaded = Tombstones::load(dir.path()).expect("load");

    assert_eq!(loaded.len(), 1000);
    assert!(loaded.contains(0));
    assert!(loaded.contains(500));
    assert!(loaded.contains(999));
    assert!(!loaded.contains(1000));
}

// ============================================================================
// 14. Atomic write — no partial state visible after write
// ============================================================================

/// After `write_index` completes, only the final files exist, not temp files.
#[test]
fn write_index_leaves_no_temp_files() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"rs");
    let entries = vec![(ng, vec![make_posting(0, 0, 0, 1)])];
    let header = make_header(1, 1);

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let tmp_idx = dir.path().join("lexical.skidx.tmp");
    let tmp_post = dir.path().join("lexical.skpost.tmp");

    assert!(
        !tmp_idx.exists(),
        "temp .skidx file must be removed after write"
    );
    assert!(
        !tmp_post.exists(),
        "temp .skpost file must be removed after write"
    );

    // Final files must exist
    assert!(dir.path().join("lexical.skidx").exists());
    assert!(dir.path().join("lexical.skpost").exists());
}

// ============================================================================
// 15. lookup filters invalid field_id
// ============================================================================

/// PostingEntry items with an unknown `field_id` (>=7) are silently skipped.
#[test]
fn lookup_filters_invalid_field_id() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"ba");
    // field_id = 99 is invalid (only 0..=6 are valid)
    let bad_posting = make_posting(0, 99, 0, 1);
    let good_posting = make_posting(0, 2, 10, 1);

    let entries = vec![(ng, vec![bad_posting, good_posting])];
    let header = make_header(1, 1);

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open");
    let results = reader.lookup(ng).expect("lookup returned None");

    // Only the valid posting should be returned
    assert_eq!(results.len(), 1, "invalid field_id entry must be filtered");
    assert_eq!(results[0].field_id, 2);
}

// ============================================================================
// 15b. IndexReader::validate — happy path (valid index passes)
// ============================================================================

/// A freshly written, correct index passes `validate()` without error.
#[test]
fn validate_passes_on_valid_index() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"ok");
    let posting = make_posting(0, 2, 10, 1);
    let entries = vec![(ng, vec![posting])];
    let header = make_header(1, 1);

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open failed");
    let result = reader.validate();
    assert!(
        result.is_ok(),
        "validate() must return Ok on a valid index, got: {result:?}"
    );
}

/// An empty index (0 ngrams) also passes `validate()` — no entries to check.
#[test]
fn validate_passes_on_empty_index() {
    let dir = tempfile::tempdir().expect("tempdir");
    let header = make_header(0, 0);
    rskim_search::lexical::index_format::write_index(dir.path(), &[], &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open failed");
    assert!(
        reader.validate().is_ok(),
        "validate() must return Ok on an empty index"
    );
}

/// Manually craft an index whose IndexEntry claims a posting range that
/// extends beyond `.skpost`, then verify that `validate()` catches it.
#[test]
fn validate_detects_out_of_bounds_posting_range() {
    use rskim_search::lexical::{INDEX_HEADER_SIZE, POSTING_ENTRY_SIZE};

    let dir = tempfile::tempdir().expect("tempdir");

    // Write a valid 1-ngram index so the files exist with the right structure.
    let ng = ngram(b"vl");
    let posting = make_posting(0, 2, 0, 1);
    let entries = vec![(ng, vec![posting])];
    let header = make_header(1, 1);
    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    // Corrupt the IndexEntry's posting_length field so it claims far more
    // entries than actually exist in .skpost.
    //
    // IndexEntry layout (20 bytes):
    //   ngram_hash [0..8]  u64 LE
    //   posting_offset [8..12]  u32 LE  (byte offset)
    //   posting_length [12..16]  u32 LE  (entry count)
    //   _pad [16..20]
    let skidx_path = dir.path().join("lexical.skidx");
    let mut bytes = std::fs::read(&skidx_path).expect("read");

    // The first (and only) IndexEntry starts at INDEX_HEADER_SIZE.
    // posting_length occupies bytes [entry_start+12 .. entry_start+16].
    let entry_start = INDEX_HEADER_SIZE;
    // Claim 1 million posting entries — far more than .skpost holds.
    let corrupt_length: u32 = 1_000_000;
    bytes[entry_start + 12..entry_start + 16]
        .copy_from_slice(&corrupt_length.to_le_bytes());
    std::fs::write(&skidx_path, &bytes).expect("write corrupted skidx");

    // Reopen — `open()` only checks structural sizes, not per-entry bounds.
    // This may or may not succeed depending on whether the size check fires.
    let Ok(reader) = IndexReader::open(dir.path()) else {
        // If open() itself rejects the file (unexpected .skidx size), the
        // corruption is caught even earlier — which is equally acceptable.
        return;
    };

    // validate() must catch the out-of-bounds posting range.
    let result = reader.validate();
    assert!(
        result.is_err(),
        "validate() must return Err when a posting range exceeds .skpost, \
         got Ok (POSTING_ENTRY_SIZE={POSTING_ENTRY_SIZE})"
    );
}

// ============================================================================
// 16. lookup filters doc_id >= file_count
// ============================================================================

/// PostingEntry items with `doc_id >= file_count` are silently skipped.
#[test]
fn lookup_filters_doc_id_out_of_range() {
    let dir = tempfile::tempdir().expect("tempdir");

    let ng = ngram(b"ca");
    let oob_posting = make_posting(5, 2, 0, 1); // doc_id=5, file_count=3 → out of range
    let good_posting = make_posting(2, 2, 10, 1); // doc_id=2 < 3 → valid

    let entries = vec![(ng, vec![oob_posting, good_posting])];
    // file_count = 3 → doc_ids 0,1,2 are valid
    let header = make_header(1, 3);

    rskim_search::lexical::index_format::write_index(dir.path(), &entries, &header)
        .expect("write_index failed");

    let reader = IndexReader::open(dir.path()).expect("open");
    let results = reader.lookup(ng).expect("lookup returned None");

    assert_eq!(results.len(), 1, "out-of-range doc_id must be filtered");
    assert_eq!(results[0].doc_id, 2);
}
