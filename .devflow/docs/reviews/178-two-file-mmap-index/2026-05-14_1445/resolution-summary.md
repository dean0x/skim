# Resolution Summary

**Branch**: feat/178-two-file-mmap-index -> main
**Date**: 2026-05-14
**Review**: .docs/reviews/178-two-file-mmap-index/2026-05-14_1445
**Command**: /resolve

## Statistics
| Metric | Value |
|--------|-------|
| Total Issues | 14 |
| Fixed | 11 |
| False Positive | 2 |
| Deferred | 0 |
| Blocked | 0 |
| Accepted (no change needed) | 1 |

## Fixed Issues
| Issue | File:Line | Commit |
|-------|-----------|--------|
| `read_array` panic on OOB slice indexing | `format.rs:141` | `7d6a057` |
| Arithmetic overflow in `expected_idx_size` | `reader.rs:85-87` | `466b365` |
| `postings_file_size as usize` truncation on 32-bit | `reader.rs:94` | `466b365` |
| `start + posting_length` overflow in `lookup_postings` | `reader.rs:160-161` | `466b365` |
| Missing posting alignment validation | `reader.rs:170` | `466b365` |
| Redundant double-iteration over postings | `reader.rs:216-222` | `466b365` |
| Repeated `file_meta_at` decoding per doc per bigram | `reader.rs:224-228` | `466b365` |
| Language filter applied after scoring | `reader.rs:252-261` | `466b365` |
| Missing offset pagination test | `reader_tests.rs` | `466b365` |
| `entries.len() as u32` truncating cast | `builder.rs:238` | `0e8fa49` |
| `total_doc_length as f32` precision loss | `builder.rs:177` | `0e8fa49` |
| `file_count += 1` unchecked overflow | `builder.rs:156` | `0e8fa49` |
| `lib.rs` "NO I/O" doc contradicts index module | `lib.rs:5` | `466b365` |
| Inconsistent clippy allow lists in test files | `*_tests.rs:3` | `466b365` |

## False Positives
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| `postings_buf.len() as u64` truncation | `builder.rs:240` | Widening cast: `usize` always fits `u64` on all supported platforms (32-bit and 64-bit). |
| `tempfile` as production dependency | `Cargo.toml:23` | `tempfile` provides the atomicity guarantee via `NamedTempFile::persist` (rename). Manual implementation with `std::fs::write` + `std::fs::rename` would require reimplementing temp file creation in the same directory, error handling, and cleanup — effectively reimplementing `tempfile`. The dependency weight is minimal (already in lockfile via dev-deps). |

## Accepted (No Code Change)
| Issue | File:Line | Reasoning |
|-------|-----------|-----------|
| `SearchField::from_discriminant` triple-sync | `types.rs:94-106` | The existing doc comment documents the discriminant contract, and the `test_search_field_discriminant_roundtrip` test catches any drift. The manual mapping is a conscious trade-off: no dependency on `num_enum`, compile-time exhaustive match, and test-backed safety. |

## Deferred to Tech Debt

_(none)_

## Blocked

_(none)_
