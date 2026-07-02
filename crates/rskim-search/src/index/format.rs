//! Pure codec for the two-file mmap'd n-gram index format.
//!
//! # File layout
//!
//! ## `index.skidx`
//!
//! ```text
//! [SkidxHeader: 62 bytes]
//! [SkidxEntry × ngram_count: 16 bytes each]   ← v3: ngram_key widened to u32
//! [FileMetaEntry × file_count: 37 bytes each]
//! ```
//!
//! ## `index.skpost`
//!
//! ```text
//! [PostingEntry ... concatenated posting lists]   ← v5: variable-length (delta+varint)
//! ```
//!
//! # Encoding
//!
//! All multi-byte integers are little-endian.  The header checksum covers
//! `postings_buf` + the entry array + the file-metadata array (in that order,
//! matching builder.rs and the verification in reader.rs open()).  This means
//! any bit-flip in `.skpost` is detected during `NgramIndexReader::open`
//! before a query can run (#364).
//!
//! ## Posting codec (v5, AD-LXPOST-1 / AD-LXFMT-4)
//!
//! Each posting entry in `.skpost` is variable-length:
//!
//! ```text
//! [varint delta_doc_id][u8 field_id][varint delta_position][varint delta_token_position]
//! ```
//!
//! - `delta_doc_id`: delta from the previous `doc_id` in the posting list
//!   (absolute for the first entry). Encoded as a little-endian base-128 varint.
//! - `field_id`: 1 byte, unchanged from v3 (bounded by `FIELD_COUNT = 8`).
//! - `delta_position`: delta from the previous `position` within the same
//!   `(doc_id, field_id)` run. The position accumulator resets to 0 whenever
//!   `doc_id` OR `field_id` changes, so the first occurrence in a new `doc_id`
//!   *or* a new `field_id` encodes `delta_position = position` (absolute). See
//!   the "Position-delta reset" section on [`encode_postings_varint`]. Encoded
//!   as a little-endian base-128 varint.
//! - `delta_token_position`: delta from the previous `token_position` within
//!   the same `(doc_id, field_id)` run (v5, AD-LXFMT-4). Reset in lockstep
//!   with `delta_position` on the same `(doc_id, field_id)` boundary. Encoded
//!   as a little-endian base-128 varint.
//!
//! Rationale: see `AD-LXPOST-1` comment at [`encode_postings_varint`].
//!
//! # Invariants upheld by this module
//!
//! - **No `std::fs` or `std::io::Write`** — every function operates on `&[u8]`
//!   or returns owned byte arrays.  All I/O happens in `builder.rs`/`reader.rs`.
//! - **No `unwrap()` / `expect()` / `panic!()`** outside `#[cfg(test)]`.
//! - **Decode loop is bounded**: `decode_postings_varint` terminates after at
//!   most `data.len()` iterations (each varint consumes ≥1 byte).

pub(crate) use super::lang_map::lang_to_id;
use crate::{
    FIELD_COUNT, SearchError, SearchField,
    weights::{TRIGRAM_WEIGHTS, lookup_weight},
};

/// Magic bytes at the start of every `.skidx` file.
pub(crate) const SKIDX_MAGIC: &[u8; 4] = b"SKIX";
/// Current on-disk format version.  Increment on any breaking change.
///
/// v1 → v2: `SkidxHeader` gained `avg_field_lengths: [f32; 8]` (+32 bytes),
/// and `FileMetaEntry` gained `field_lengths: [u32; 8]` (+32 bytes).
/// v1 indexes are rejected with a clear error message containing "format version".
///
/// v2 → v3 (#355 Part B): `SkidxEntry.ngram_key` widened from `u16` to `u32`
/// (bigram → trigram). `SKIDX_ENTRY_SIZE` grows from 14 → 16 bytes.
/// `PostingEntry` is UNCHANGED in v3. Old v2 indexes self-heal via the staleness
/// check (the stale reader rejects the wrong version and triggers a full rebuild).
///
/// v3 → v4 (#358 Item 2):
///
/// # AD-LXFMT-3
///
/// `PostingEntry` encoding changed from fixed 9-byte to variable-length
/// delta+varint. The `.skpost` blob is no longer a flat array of fixed-size
/// structs; instead each entry is `[varint delta_doc_id][u8 field_id][varint
/// delta_position]`. See [`encode_postings_varint`] and
/// [`decode_postings_varint`].
///
/// Sequential bumps (#355 merges first, then #358):
/// - v2 → v3: owned by #355 Part B (trigram key widen, `SkidxEntry` change)
/// - v3 → v4: owned by #358 Item 2 (posting codec / `PostingEntry` change)
///
/// Old v3 indexes self-heal: `decode_header` rejects version ≠ 5 with
/// "unsupported format version … please rebuild" so the staleness check
/// triggers a full rebuild on first query after upgrade.
///
/// v4 → v5 (#392 / #380 Phase 2):
///
/// # AD-LXFMT-4
///
/// `PostingEntry` gains a `token_position: u32` field (the word-token ordinal
/// of the trigram's first byte) to support word/token-distance phrase / --near
/// search. On disk it is appended as a 4th tail varint per entry:
/// `[varint delta_doc_id][u8 field_id][varint delta_position][varint delta_token_position]`.
/// The 3-field v4 prefix is byte-identical, so snippet/field_id logic is
/// unchanged. Old v4 indexes self-heal: `decode_header` rejects version ≠ 5.
pub(crate) const FORMAT_VERSION: u16 = 5;
/// Size in bytes of [`SkidxHeader`] on disk.
///
/// v1 was 30 bytes; v2 adds 32 bytes for `avg_field_lengths: [f32; 8]`.
/// v3 header layout is unchanged from v2 (62 bytes).
pub(crate) const SKIDX_HEADER_SIZE: usize = 62;
/// Size in bytes of a single [`SkidxEntry`] on disk.
///
/// v2: 14 bytes (`ngram_key: u16` + `posting_offset: u64` + `posting_length: u32`).
/// v3: 16 bytes (`ngram_key: u32` + `posting_offset: u64` + `posting_length: u32`).
pub(crate) const SKIDX_ENTRY_SIZE: usize = 16;
/// Size in bytes of a single [`FileMetaEntry`] on disk.
///
/// v1 was 5 bytes; v2 adds 32 bytes for `field_lengths: [u32; 8]`.
pub(crate) const FILE_META_SIZE: usize = 37;
/// BM25 term-frequency saturation parameter.
#[cfg(test)]
const BM25_K1: f32 = 1.2;
/// BM25 document-length normalisation parameter.
#[cfg(test)]
const BM25_B: f32 = 0.75;

/// Fixed-size header at the start of every `.skidx` file.
///
/// Layout (62 bytes, all integers little-endian):
/// ```text
/// [0..4]   magic              4 bytes
/// [4..6]   version            2 bytes
/// [6..10]  ngram_count        4 bytes
/// [10..14] file_count         4 bytes
/// [14..22] postings_file_size 8 bytes
/// [22..26] avg_doc_length     4 bytes (f32 LE)
/// [26..58] avg_field_lengths  32 bytes ([f32; 8] LE)
/// [58..62] checksum           4 bytes (CRC32)
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SkidxHeader {
    /// Must equal [`SKIDX_MAGIC`].
    pub magic: [u8; 4],
    /// Must equal [`FORMAT_VERSION`].
    pub version: u16,
    /// Number of distinct n-gram entries in the lookup table.
    pub ngram_count: u32,
    /// Number of files in the index.
    pub file_count: u32,
    /// Total byte size of the companion `.skpost` file.
    pub postings_file_size: u64,
    /// Average document byte length, used for BM25 normalisation.
    pub avg_doc_length: f32,
    /// Average per-field byte length across all documents (BM25F normalisation).
    ///
    /// Indexed by [`crate::SearchField`] discriminant.
    pub avg_field_lengths: [f32; FIELD_COUNT],
    /// CRC32 over `.skpost` + entry array + file-metadata array bytes (v4, #364).
    pub checksum: u32,
}

/// One entry in the sorted n-gram lookup table.
///
/// Layout (16 bytes, all integers little-endian):
/// ```text
/// [0..4]   ngram_key       4 bytes  (v3: widened from u16 to u32 — #355 Part B)
/// [4..12]  posting_offset  8 bytes
/// [12..16] posting_length  4 bytes (number of bytes, not entries)
/// ```
///
/// # AD-355-6
///
/// `ngram_key` widened from `u16` (bigram, v2) to `u32` (trigram, v3) in #355 Part B.
/// `PostingEntry` is UNCHANGED. #358 owns the next format bump (v3→v4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SkidxEntry {
    /// The trigram key (`(b1 << 16) | (b2 << 8) | b3`).
    pub ngram_key: u32,
    /// Byte offset into `.skpost` where this n-gram's posting list begins.
    pub posting_offset: u64,
    /// Byte length of this n-gram's posting list in `.skpost`.
    pub posting_length: u32,
}

/// One element in a posting list inside `.skpost`.
///
/// This is the **in-memory** representation.  The **on-disk** encoding is
/// variable-length (v5, AD-LXPOST-1 / AD-LXFMT-4): each entry is written as
/// `[varint delta_doc_id][u8 field_id][varint delta_position][varint delta_token_position]`
/// via [`encode_postings_varint`] and decoded via [`decode_postings_varint`].
/// The fixed-stride 9-byte layout present in v3 no longer applies. The derived
/// `Ord` orders by `(doc_id, field_id, position, token_position)`; since
/// `token_position` is a pure function of `position` within a document, it
/// never actually changes the sort outcome — `builder.rs`'s `sort_unstable()`
/// behavior is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PostingEntry {
    /// The document (file) this posting belongs to.
    pub doc_id: u32,
    /// [`SearchField`] discriminant — validated during decode.
    pub field_id: u8,
    /// Byte position within the document.
    pub position: u32,
    /// Word-token ordinal of the trigram's first byte (v5). Additive coordinate
    /// for word/token-distance phrase / --near search; `position` (byte offset)
    /// is retained for snippets.
    pub token_position: u32,
}

/// Per-file metadata stored in the tail of `.skidx`.
///
/// Layout (37 bytes, all integers little-endian):
/// ```text
/// [0]      lang_id        1 byte
/// [1..5]   doc_length     4 bytes
/// [5..37]  field_lengths  32 bytes ([u32; 8] LE)
/// ```
///
/// # Invariant
///
/// `field_lengths[0..8].iter().sum::<u32>() == doc_length`.
/// Upheld by the builder; validated by the reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileMetaEntry {
    /// Language ID from [`lang_to_id`].
    pub lang_id: u8,
    /// Byte length of the original document.
    pub doc_length: u32,
    /// Per-field byte lengths for BM25F normalisation.
    ///
    /// Indexed by [`crate::SearchField`] discriminant (0 = TypeDefinition … 7 = Other).
    pub field_lengths: [u32; FIELD_COUNT],
}

/// Extract a fixed-size byte array from `data[start..start+N]`.
///
/// Returns [`SearchError::IndexCorrupted`] if the range would overflow `usize`
/// or exceeds `data.len()`, rather than panicking.
fn read_array<const N: usize>(
    data: &[u8],
    start: usize,
    context: &'static str,
) -> crate::Result<[u8; N]> {
    let end = start
        .checked_add(N)
        .ok_or_else(|| SearchError::IndexCorrupted(format!("{context}: offset overflow")))?;
    data.get(start..end)
        .and_then(|s| s.try_into().ok())
        .ok_or_else(|| {
            SearchError::IndexCorrupted(format!(
                "{context}: need {N} bytes at offset {start}, got {}",
                data.len()
            ))
        })
}

/// Encode a [`SkidxHeader`] into its 62-byte on-disk representation.
pub(crate) fn encode_header(h: &SkidxHeader) -> [u8; SKIDX_HEADER_SIZE] {
    let mut buf = [0u8; SKIDX_HEADER_SIZE];
    buf[0..4].copy_from_slice(&h.magic);
    buf[4..6].copy_from_slice(&h.version.to_le_bytes());
    buf[6..10].copy_from_slice(&h.ngram_count.to_le_bytes());
    buf[10..14].copy_from_slice(&h.file_count.to_le_bytes());
    buf[14..22].copy_from_slice(&h.postings_file_size.to_le_bytes());
    buf[22..26].copy_from_slice(&h.avg_doc_length.to_le_bytes());
    // avg_field_lengths: 8 × f32 LE at bytes [26..58]
    for (i, &v) in h.avg_field_lengths.iter().enumerate() {
        let start = 26 + i * 4;
        buf[start..start + 4].copy_from_slice(&v.to_le_bytes());
    }
    buf[58..62].copy_from_slice(&h.checksum.to_le_bytes());
    buf
}

/// Decode a [`SkidxHeader`] from a byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short,
/// the magic bytes do not match, or the version is not [`FORMAT_VERSION`].
/// Format v1 indexes are rejected with an error message containing "format version".
pub(crate) fn decode_header(data: &[u8]) -> crate::Result<SkidxHeader> {
    if data.len() < SKIDX_HEADER_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "header truncated: need {SKIDX_HEADER_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let magic: [u8; 4] = read_array(data, 0, "header: magic")?;
    if &magic != SKIDX_MAGIC {
        return Err(SearchError::IndexCorrupted(format!(
            "bad magic: expected {:?}, got {:?}",
            SKIDX_MAGIC, magic
        )));
    }
    let version = u16::from_le_bytes(read_array(data, 4, "header: version")?);
    if version != FORMAT_VERSION {
        return Err(SearchError::IndexCorrupted(format!(
            "unsupported format version: {version} (expected {FORMAT_VERSION}); \
             please rebuild the index"
        )));
    }

    // Decode avg_doc_length: f32 LE at bytes [22..26]
    let avg_doc_length = f32::from_le_bytes(read_array(data, 22, "header: avg_doc_length")?);
    if !avg_doc_length.is_finite() || avg_doc_length < 0.0 {
        return Err(SearchError::IndexCorrupted(format!(
            "header: avg_doc_length must be a finite number >= 0.0, got {avg_doc_length}"
        )));
    }

    // Decode avg_field_lengths: FIELD_COUNT × f32 LE at bytes [26..58]
    let mut avg_field_lengths = [0.0f32; FIELD_COUNT];
    for (i, v) in avg_field_lengths.iter_mut().enumerate() {
        let start = 26 + i * 4;
        let raw = f32::from_le_bytes(read_array(data, start, "header: avg_field_lengths")?);
        if !raw.is_finite() || raw < 0.0 {
            return Err(SearchError::IndexCorrupted(format!(
                "header: avg_field_lengths[{i}] must be a finite number >= 0.0, got {raw}"
            )));
        }
        *v = raw;
    }

    Ok(SkidxHeader {
        magic,
        version,
        ngram_count: u32::from_le_bytes(read_array(data, 6, "header: ngram_count")?),
        file_count: u32::from_le_bytes(read_array(data, 10, "header: file_count")?),
        postings_file_size: u64::from_le_bytes(read_array(data, 14, "header: postings_file_size")?),
        avg_doc_length,
        avg_field_lengths,
        checksum: u32::from_le_bytes(read_array(data, 58, "header: checksum")?),
    })
}

/// Encode a [`SkidxEntry`] into its 16-byte on-disk representation.
///
/// # AD-355-6 / #355 Part B
///
/// `ngram_key` is 4 bytes (u32) in v3, up from 2 bytes (u16) in v2.
/// Layout: `[0..4] ngram_key | [4..12] posting_offset | [12..16] posting_length`.
pub(crate) fn encode_entry(e: &SkidxEntry) -> [u8; SKIDX_ENTRY_SIZE] {
    let mut buf = [0u8; SKIDX_ENTRY_SIZE];
    buf[0..4].copy_from_slice(&e.ngram_key.to_le_bytes());
    buf[4..12].copy_from_slice(&e.posting_offset.to_le_bytes());
    buf[12..16].copy_from_slice(&e.posting_length.to_le_bytes());
    buf
}

/// Decode a [`SkidxEntry`] from a 16-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_entry(data: &[u8]) -> crate::Result<SkidxEntry> {
    if data.len() < SKIDX_ENTRY_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "entry truncated: need {SKIDX_ENTRY_SIZE} bytes, got {}",
            data.len()
        )));
    }
    Ok(SkidxEntry {
        ngram_key: u32::from_le_bytes(read_array(data, 0, "entry: ngram_key")?),
        posting_offset: u64::from_le_bytes(read_array(data, 4, "entry: posting_offset")?),
        posting_length: u32::from_le_bytes(read_array(data, 12, "entry: posting_length")?),
    })
}

// ============================================================================
// v4 variable-length posting codec (delta + varint)
// ============================================================================

/// Encode a `u32` value as a little-endian base-128 varint into `buf`.
///
/// Returns the number of bytes written (1–5).
///
/// # Encoding
///
/// Each byte carries 7 payload bits.  The MSB (`0x80`) is set on all bytes
/// except the last, which signals "more bytes follow".  Smallest values (0–127)
/// encode to 1 byte; largest `u32` values encode to 5 bytes.
pub(crate) fn encode_varint(mut value: u32, buf: &mut Vec<u8>) -> usize {
    let start = buf.len();
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            buf.push(byte); // final byte: MSB clear
            break;
        }
        buf.push(byte | 0x80); // continuation byte: MSB set
    }
    buf.len() - start
}

/// Decode a little-endian base-128 varint from `data` at `offset`.
///
/// Returns `(value, bytes_consumed)` or [`SearchError::IndexCorrupted`] if
/// the slice is too short or the varint exceeds 5 bytes (which would overflow
/// a `u32`).
///
/// # Decode-loop bound
///
/// The loop runs at most 5 iterations (5 × 7 = 35 bits < 32 bits needed).
/// On the 5th iteration the continuation bit must be clear; if it is set the
/// varint is malformed and the function returns `IndexCorrupted`.
pub(crate) fn decode_varint(data: &[u8], offset: usize) -> crate::Result<(u32, usize)> {
    let mut value: u32 = 0;
    let mut shift = 0u32;
    let mut pos = offset;
    // A u32 varint spans at most 5 bytes (ceil(32/7) = 5).
    // We cap the loop at 5 to keep it bounded (reliability.md: every loop has
    // a fixed upper bound).
    for _ in 0..5usize {
        if pos >= data.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "varint at offset {offset}: truncated (need more bytes, got {})",
                data.len().saturating_sub(offset)
            )));
        }
        let byte = data[pos];
        pos += 1;
        // PF-004: widen to u32 before shifting to prevent u8 overflow.
        value |= (u32::from(byte) & 0x7F) << shift;
        if byte & 0x80 == 0 {
            // Final byte — continuation bit clear.
            return Ok((value, pos - offset));
        }
        shift += 7;
    }
    // If we reach here all 5 bytes had the continuation bit set — overflow.
    Err(SearchError::IndexCorrupted(format!(
        "varint at offset {offset}: overflow (more than 5 bytes / 35 bits for u32)"
    )))
}

/// Encode a posting list into `buf` using v5 delta+varint encoding.
///
/// # AD-LXPOST-1
///
/// Delta-encode `doc_id` (store delta from the previous `doc_id`; absolute for
/// the first entry) and encode each delta as a little-endian base-128 varint.
/// Within each `(doc_id, field_id)` run, encode `position` deltas as varints.
/// `field_id` is 1 byte (unchanged; bounded by `FIELD_COUNT = 8`).
///
/// # AD-LXFMT-4 (v5)
///
/// A 4th tail varint, `delta_token_position`, is appended per entry to carry
/// `PostingEntry::token_position`. It resets in lockstep with `delta_position`
/// — see "Position-delta reset" below.
///
/// Layout per posting entry:
/// ```text
/// [varint delta_doc_id][u8 field_id][varint delta_position][varint delta_token_position]
/// ```
///
/// Chosen as the least invasive approach to the reader hot path (sequential
/// decode, low latency regression risk). Roaring/PForDelta revisited only if
/// the measured post-compression ratio still misses the grounded target.
/// Originating tracker: #273.  FORMAT_VERSION v4→v5 owned by #392 / #380
/// Phase 2; v3→v4 owned by #358 Item 2; v2→v3 owned by #355 Part B.
///
/// Note: the sibling AST index (`crates/rskim-search/src/ast_index/store/`)
/// deliberately retains its fixed 8-byte posting codec (doc_id 4B + position
/// 4B, no field_id byte) per AD-LXPOST-1 — the AST posting list is small and
/// latency-critical while the lexical posting list is the size driver.
/// This divergence is intentional, not drift.
///
/// # Sorting requirement
///
/// `postings` must be sorted ascending by `(doc_id, field_id, position)`.
/// The caller (`builder.rs`) sorts each posting list before calling this
/// function, ensuring positions are monotonically non-decreasing within each
/// `(doc_id, field_id)` run so all `delta_position` values are non-negative.
///
/// # Position-delta reset
///
/// `prev_position` AND `prev_token_position` are both reset to 0 whenever
/// `doc_id` OR `field_id` changes. This ensures each `(doc_id, field_id)` run
/// starts fresh, keeping `delta_position` and `delta_token_position`
/// non-negative (and therefore small / 1-byte varint) across field boundaries
/// within the same document.
///
/// Without this reset, the first position (or token_position) in field N+1
/// would be encoded as a delta from the last value of field N — which can
/// wrap to near-u32::MAX and always encodes to the maximum 5-byte varint,
/// defeating compression.
pub(crate) fn encode_postings_varint(postings: &[PostingEntry], buf: &mut Vec<u8>) {
    let mut prev_doc_id: u32 = 0;
    let mut prev_field_id: u8 = 0;
    let mut prev_position: u32 = 0;
    let mut prev_token_position: u32 = 0;
    for p in postings {
        let delta_doc_id = p.doc_id.wrapping_sub(prev_doc_id);
        // Reset position AND token_position accumulators together when doc_id OR
        // field_id changes, so both deltas stay forward (non-wrapping) within a
        // (doc_id, field_id) run.
        if delta_doc_id != 0 || p.field_id != prev_field_id {
            prev_position = 0;
            prev_token_position = 0;
        }
        let delta_position = p.position.wrapping_sub(prev_position);
        let delta_token_position = p.token_position.wrapping_sub(prev_token_position);
        encode_varint(delta_doc_id, buf);
        buf.push(p.field_id);
        encode_varint(delta_position, buf);
        encode_varint(delta_token_position, buf);
        prev_doc_id = p.doc_id;
        prev_field_id = p.field_id;
        prev_position = p.position;
        prev_token_position = p.token_position;
    }
}

/// Decode a v5 variable-length posting list from `data`.
///
/// Returns the decoded [`PostingEntry`] values in the original sort order
/// (ascending by `(doc_id, field_id, position)`).
///
/// # Bounded decode loop
///
/// The outer loop terminates when `offset >= data.len()`.  Each iteration
/// consumes at least 4 bytes (`1-byte varint + 1-byte field_id + 1-byte
/// varint + 1-byte varint`), so the loop runs at most `data.len() / 4` times
/// — bounded by the data size, not an external counter.
///
/// # Capacity hint
///
/// `Vec::with_capacity(data.len() / 4)` pre-sizes the output using the upper
/// bound implied by the minimum-bytes-per-entry guarantee, eliminating
/// geometric reallocation on long posting lists in the query hot path.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if:
/// - A varint is malformed (truncated, > 5 bytes)
/// - `field_id` is not a valid [`SearchField`] discriminant
///
/// # Delta reconstruction
///
/// `doc_id`, `position`, and `token_position` are reconstructed with
/// `wrapping_add`, the exact inverse of the encoder's `wrapping_sub` (see
/// [`encode_postings_varint`]). The round-trip is therefore lossless for
/// every `u32` input — including the maximum `doc_id` gap (`0 → u32::MAX`,
/// covered by `test_posting_codec_max_gap_docid`).  `prev_position` and
/// `prev_token_position` are both reset to 0 whenever `doc_id` OR `field_id`
/// changes, mirroring the encoder's reset (see [`encode_postings_varint`]).
/// Modular arithmetic cannot fail, so no overflow error is raised; a corrupt
/// blob yields wrong-but-bounded `u32` values (used only for scoring), never
/// a panic or out-of-bounds access.
pub(crate) fn decode_postings_varint(data: &[u8]) -> crate::Result<Vec<PostingEntry>> {
    // Pre-size with the upper bound: each v5 entry is at least 4 bytes
    // (1-byte varint + 1-byte field_id + 1-byte varint + 1-byte varint),
    // so at most data.len() / 4 entries can be present.
    let mut postings = Vec::with_capacity(data.len() / 4);
    let mut offset = 0usize;
    let mut prev_doc_id: u32 = 0;
    let mut prev_field_id: u8 = 0;
    let mut prev_position: u32 = 0;
    let mut prev_token_position: u32 = 0;

    while offset < data.len() {
        let entry_start = offset;

        // Decode delta_doc_id varint.
        let (delta_doc_id, n) = decode_varint(data, offset)?;
        offset += n;

        // Decode field_id (1 byte).
        if offset >= data.len() {
            return Err(SearchError::IndexCorrupted(format!(
                "posting at byte {entry_start}: truncated before field_id",
            )));
        }
        let field_id = data[offset];
        offset += 1;
        if SearchField::from_discriminant(field_id).is_none() {
            return Err(SearchError::IndexCorrupted(format!(
                "posting: invalid field_id {field_id} at byte {}",
                offset - 1
            )));
        }

        // Decode delta_position varint.
        let (delta_position, m) = decode_varint(data, offset)?;
        offset += m;

        // Decode delta_token_position varint (v5).
        let (delta_token_position, t) = decode_varint(data, offset)?;
        offset += t;

        // Reconstruct absolute doc_id, position, token_position.
        // Reset position AND token_position accumulators when doc_id OR field_id
        // changes, mirroring the encoder.
        let doc_id = prev_doc_id.wrapping_add(delta_doc_id);
        if delta_doc_id != 0 || field_id != prev_field_id {
            prev_position = 0;
            prev_token_position = 0;
        }
        let position = prev_position.wrapping_add(delta_position);
        let token_position = prev_token_position.wrapping_add(delta_token_position);

        postings.push(PostingEntry {
            doc_id,
            field_id,
            position,
            token_position,
        });

        prev_doc_id = doc_id;
        prev_field_id = field_id;
        prev_position = position;
        prev_token_position = token_position;
    }
    Ok(postings)
}

/// Encode a [`FileMetaEntry`] into its 37-byte on-disk representation.
pub(crate) fn encode_file_meta(m: &FileMetaEntry) -> [u8; FILE_META_SIZE] {
    let mut buf = [0u8; FILE_META_SIZE];
    buf[0] = m.lang_id;
    buf[1..5].copy_from_slice(&m.doc_length.to_le_bytes());
    // field_lengths: 8 × u32 LE at bytes [5..37]
    for (i, &v) in m.field_lengths.iter().enumerate() {
        let start = 5 + i * 4;
        buf[start..start + 4].copy_from_slice(&v.to_le_bytes());
    }
    buf
}

/// Decode a [`FileMetaEntry`] from a 37-byte slice.
///
/// # Errors
///
/// Returns [`SearchError::IndexCorrupted`] if the slice is too short.
pub(crate) fn decode_file_meta(data: &[u8]) -> crate::Result<FileMetaEntry> {
    if data.len() < FILE_META_SIZE {
        return Err(SearchError::IndexCorrupted(format!(
            "file_meta truncated: need {FILE_META_SIZE} bytes, got {}",
            data.len()
        )));
    }
    let mut field_lengths = [0u32; FIELD_COUNT];
    for (i, v) in field_lengths.iter_mut().enumerate() {
        let start = 5 + i * 4;
        *v = u32::from_le_bytes(read_array(data, start, "file_meta: field_lengths")?);
    }
    let doc_length = u32::from_le_bytes(read_array(data, 1, "file_meta: doc_length")?);
    // Validate the documented invariant: field_lengths must sum to doc_length.
    let field_sum: u32 = field_lengths.iter().sum();
    if field_sum != doc_length {
        return Err(SearchError::IndexCorrupted(format!(
            "file_meta: field_lengths sum ({field_sum}) != doc_length ({doc_length})"
        )));
    }
    Ok(FileMetaEntry {
        lang_id: data[0],
        doc_length,
        field_lengths,
    })
}

/// Binary-search `entries_data` for the entry with `ngram_key`.
///
/// `entries_data` must be a byte slice whose length is a multiple of
/// [`SKIDX_ENTRY_SIZE`] and whose entries are sorted ascending by `ngram_key`.
///
/// Returns `Ok(Some(entry))` if found, `Ok(None)` if absent, or
/// [`SearchError::IndexCorrupted`] if the slice is malformed.
pub(crate) fn lookup_ngram(
    entries_data: &[u8],
    ngram_key: u32,
) -> crate::Result<Option<SkidxEntry>> {
    if !entries_data.len().is_multiple_of(SKIDX_ENTRY_SIZE) {
        return Err(SearchError::IndexCorrupted(format!(
            "entries_data length {} is not a multiple of SKIDX_ENTRY_SIZE {}",
            entries_data.len(),
            SKIDX_ENTRY_SIZE
        )));
    }
    let n = entries_data.len() / SKIDX_ENTRY_SIZE;
    let mut lo = 0usize;
    let mut hi = n;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let offset = mid * SKIDX_ENTRY_SIZE;
        // v3: ngram_key is 4 bytes (u32) at offset 0.
        let key = u32::from_le_bytes(read_array(entries_data, offset, "entries: key read")?);
        match key.cmp(&ngram_key) {
            std::cmp::Ordering::Equal => {
                return decode_entry(&entries_data[offset..offset + SKIDX_ENTRY_SIZE]).map(Some);
            }
            std::cmp::Ordering::Less => lo = mid + 1,
            std::cmp::Ordering::Greater => hi = mid,
        }
    }
    Ok(None)
}

/// Compute the BM25 contribution for a single term occurrence.
///
/// `tf` — observed term frequency in this document.
/// `idf` — inverse document frequency weight from the trigram table.
/// `doc_len` — byte length of the document.
/// `avg_doc_len` — average byte length across the corpus.
///
/// Returns the BM25 score contribution as `f64` (accumulated across terms
/// to avoid precision loss).
#[cfg(test)]
#[must_use]
pub(crate) fn bm25_score(tf: f32, idf: f32, doc_len: u32, avg_doc_len: f32) -> f64 {
    let k1 = f64::from(BM25_K1);
    let b = f64::from(BM25_B);
    let tf = f64::from(tf);
    let idf = f64::from(idf);
    let dl = f64::from(doc_len);
    let adl = if avg_doc_len > 0.0 {
        f64::from(avg_doc_len)
    } else {
        1.0
    };
    let norm = 1.0 - b + b * (dl / adl);
    let tf_norm = tf * (k1 + 1.0) / (tf + k1 * norm);
    idf * tf_norm
}

/// Compute IDF weight for a trigram key using the empirical weight table.
///
/// Falls back to the default weight for trigrams not present in the table.
///
/// # AD-355-5 / PF-004
///
/// Key is `u32` (widened from `u16` in #355 Part B) to match [`crate::ngram::Ngram`].
#[must_use]
#[inline]
pub(crate) fn idf_for_key(key: u32) -> f32 {
    lookup_weight(key, TRIGRAM_WEIGHTS)
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
