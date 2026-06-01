//! Bigram extraction from source files.

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::types::{CorpusStats, LanguageCount, SourceFile};

/// Encode two bytes into a `u16` bigram key.
///
/// The high byte is `b1`, the low byte is `b2`.
#[must_use]
pub fn encode_bigram(b1: u8, b2: u8) -> u16 {
    (u16::from(b1) << 8) | u16::from(b2)
}

/// Decode a `u16` bigram key back into its two component bytes.
#[must_use]
pub fn decode_bigram(bigram: u16) -> (u8, u8) {
    ((bigram >> 8) as u8, (bigram & 0xFF) as u8)
}

/// Format a bigram for display.
///
/// Printable ASCII bytes are shown as characters; non-printable bytes as `\xNN`.
#[must_use]
pub fn bigram_to_display(bigram: u16) -> String {
    let (b1, b2) = decode_bigram(bigram);
    let fmt = |b: u8| -> String {
        if b.is_ascii_graphic() || b == b' ' {
            String::from(b as char)
        } else {
            format!("\\x{b:02X}")
        }
    };
    format!("{}{}", fmt(b1), fmt(b2))
}

/// Extract the set of unique byte-pair bigrams from `content`.
///
/// Returns an empty set for inputs with fewer than two bytes.
#[must_use]
pub fn extract_bigrams(content: &str) -> HashSet<u16> {
    let bytes = content.as_bytes();
    let mut set = HashSet::new();
    for window in bytes.windows(2) {
        set.insert(encode_bigram(window[0], window[1]));
    }
    set
}

/// Compute SHA-256 of `content` for deduplication purposes.
#[must_use]
pub fn content_hash(content: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hasher.finalize().into()
}

/// Compute document-frequency counts across the corpus, deduplicated by SHA-256.
///
/// Returns `(document_frequency_map, corpus_stats)` where the DF map maps each
/// bigram `u16` key to the number of unique files that contain it.
#[must_use]
pub fn extract_bigrams_from_corpus(files: &[SourceFile]) -> (HashMap<u16, u32>, CorpusStats) {
    let mut seen_hashes: HashSet<[u8; 32]> = HashSet::new();
    let mut df_map: HashMap<u16, u32> = HashMap::new();
    let mut lang_counts: HashMap<String, u32> = HashMap::new();
    let mut total_files_seen: u32 = 0;
    let mut unique_file_count: u32 = 0;
    let mut total_bigrams: u64 = 0;

    for file in files {
        total_files_seen += 1;
        let hash = content_hash(&file.content);
        if !seen_hashes.insert(hash) {
            // Duplicate content — skip.
            continue;
        }

        unique_file_count += 1;
        let lang_str = format!("{:?}", file.language);
        *lang_counts.entry(lang_str).or_default() += 1;

        let bigrams = extract_bigrams(&file.content);
        total_bigrams += bigrams.len() as u64;
        for bigram in bigrams {
            *df_map.entry(bigram).or_default() += 1;
        }
    }

    let language_breakdown = {
        let mut breakdown: Vec<_> = lang_counts
            .into_iter()
            .map(|(language, file_count)| LanguageCount {
                language,
                file_count,
            })
            .collect();
        breakdown.sort_by_key(|b| std::cmp::Reverse(b.file_count));
        breakdown
    };

    let unique_bigrams = df_map.len();

    let stats = CorpusStats {
        total_files: unique_file_count,
        total_bigrams,
        unique_bigrams,
        deduplicated_files: total_files_seen - unique_file_count,
        language_breakdown,
    };

    (df_map, stats)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    // ---- encode / decode roundtrip ----

    #[test]
    fn encode_decode_roundtrip_all_bytes() {
        for b1 in 0u8..=255 {
            for b2 in 0u8..=255 {
                let encoded = encode_bigram(b1, b2);
                let (d1, d2) = decode_bigram(encoded);
                assert_eq!((d1, d2), (b1, b2), "roundtrip failed for ({b1},{b2})");
            }
        }
    }

    // ---- extract_bigrams ----

    #[test]
    fn empty_string_yields_empty_set() {
        assert!(extract_bigrams("").is_empty());
    }

    #[test]
    fn single_byte_yields_empty_set() {
        assert!(extract_bigrams("a").is_empty());
    }

    #[test]
    fn two_bytes_yields_one_bigram() {
        let set = extract_bigrams("fn");
        assert_eq!(set.len(), 1);
        assert!(set.contains(&encode_bigram(b'f', b'n')));
    }

    #[test]
    fn fn_main_yields_correct_bigrams() {
        // "fn main()" → byte pairs: fn, n , " m", ma, ai, in, n(, ()
        let set = extract_bigrams("fn main()");
        let expected: HashSet<u16> = [
            encode_bigram(b'f', b'n'),
            encode_bigram(b'n', b' '),
            encode_bigram(b' ', b'm'),
            encode_bigram(b'm', b'a'),
            encode_bigram(b'a', b'i'),
            encode_bigram(b'i', b'n'),
            encode_bigram(b'n', b'('),
            encode_bigram(b'(', b')'),
        ]
        .into_iter()
        .collect();
        assert_eq!(set, expected);
    }

    #[test]
    fn repeated_bytes_dedup_within_file() {
        // "aaaa" → only {(a,a)}
        let set = extract_bigrams("aaaa");
        assert_eq!(set.len(), 1);
        assert!(set.contains(&encode_bigram(b'a', b'a')));
    }

    #[test]
    fn utf8_multibyte_no_panic() {
        // "café" has multi-byte UTF-8 — should not panic
        let set = extract_bigrams("café");
        assert!(!set.is_empty());
    }

    // ---- extract_bigrams_from_corpus DF counting ----

    fn make_file(content: &str, language: rskim_core::Language) -> SourceFile {
        SourceFile {
            path: std::path::PathBuf::from("test.rs"),
            language,
            content: content.to_string(),
        }
    }

    #[test]
    fn corpus_df_counting() {
        // 3 files: "fn", "fn foo", "bar"
        // DF(fn) = 2 (both first two files), DF(ba) = 1, DF(ar) = 1
        let files = vec![
            make_file("fn", rskim_core::Language::Rust),
            make_file("fn foo", rskim_core::Language::Rust),
            make_file("bar", rskim_core::Language::Rust),
        ];
        let (df_map, stats) = extract_bigrams_from_corpus(&files);

        assert_eq!(stats.total_files, 3);
        assert_eq!(*df_map.get(&encode_bigram(b'f', b'n')).unwrap(), 2);
        assert_eq!(*df_map.get(&encode_bigram(b'b', b'a')).unwrap(), 1);
    }

    #[test]
    fn sha256_dedup_skips_duplicate_content() {
        // Two identical files → treated as one for DF purposes
        let files = vec![
            make_file("fn main()", rskim_core::Language::Rust),
            make_file("fn main()", rskim_core::Language::Rust),
        ];
        let (df_map, stats) = extract_bigrams_from_corpus(&files);

        // Only 1 unique file
        assert_eq!(stats.total_files, 1);
        assert_eq!(stats.deduplicated_files, 1);

        // DF should be same as if we had 1 file
        let (df_single, _) =
            extract_bigrams_from_corpus(&[make_file("fn main()", rskim_core::Language::Rust)]);
        assert_eq!(df_map, df_single);
    }
}
