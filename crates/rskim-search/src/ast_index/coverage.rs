//! AST coverage reporting for the size-cap accounting contract (#405).
//!
//! # Coverage taxonomy
//!
//! For an indexed file with language `L` and size `S`:
//!
//! | `ast_size_limit(L)` | Condition    | State           |
//! |---------------------|--------------|-----------------|
//! | `None`              | any          | NON-PARTICIPANT |
//! | `Some(cap)`         | `S <= cap`   | SIZE-ELIGIBLE   |
//! | `Some(cap)`         | `S > cap`    | SIZE-EXCLUDED   |
//! | `Some(cap)`         | `S` unknown  | UNDETERMINED    |
//!
//! # AD-405-3: One struct, one shape, all three surfaces
//!
//! `AstCoverage` is the single Serialize struct used on every surface:
//! `--stats --json`, standalone `--ast` `AstJsonEnvelope`, and compound
//! `QueryOutput`.  All three carry the same five keys so consumers never have to
//! switch shapes based on the invocation mode.
//!
//! # AD-405-5: Bounded excluded sample
//!
//! The `excluded` field is a path-sorted sample of at most
//! `AST_COVERAGE_EXCLUDED_SAMPLE_CAP` (10) elements.  `size_excluded_files` is
//! the authoritative total; `excluded` is a bounded display sample.  This keeps
//! per-invocation allocation O(10) even on a repo with millions of excluded files
//! (avoids PF-012 determinism and per-query bloat).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use rskim_core::{Language, ast_size_limit};

/// Maximum number of excluded files in the bounded sample (AD-405-5).
pub const AST_COVERAGE_EXCLUDED_SAMPLE_CAP: usize = 10;

/// Bundles the two `u64` measurements for a size-excluded file so that the
/// positional parameters of [`insert_into_bounded_sample`] cannot be silently
/// transposed.
struct ExcludedSizes {
    size_bytes: u64,
    limit_bytes: u64,
}

/// A single file that is present in the index but exceeds the AST size cap.
///
/// Fields are intentionally kept flat (no nesting) so the three JSON envelopes
/// that carry `AstCoverage` all share the identical deserialization schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstExcludedFile {
    /// Repo-relative path of the file.
    pub path: String,
    /// Language name (lowercase, e.g. `"rust"`).
    pub lang: String,
    /// File size in bytes at the time the manifest was written.
    pub size_bytes: u64,
    /// The AST size cap that rejected this file.
    pub limit_bytes: u64,
    /// Reason code — always `"ast_size_cap"` for files excluded by the size cap.
    pub reason: String,
}

/// AST size-coverage summary for one index.
///
/// ## AD-405-3: One struct, one shape, all three surfaces
///
/// Used on `--stats --json`, standalone `--ast` JSON, and compound `--ast` JSON
/// (`QueryOutput`).  The `#[serde(skip_serializing_if = "Option::is_none")]`
/// wrapper in each envelope suppresses the whole object when all counts are zero.
///
/// ## Field notes
///
/// - `size_excluded_files` is the AUTHORITATIVE total.  `excluded` is a bounded
///   path-sorted SAMPLE of at most `AST_COVERAGE_EXCLUDED_SAMPLE_CAP` elements.
/// - `undetermined_files` counts entries whose size is `None` in the manifest for
///   a tree-sitter language.  Never folded into eligible or excluded.
/// - Files whose language yields `ast_size_limit(L) == None` (JSON/YAML/TOML)
///   are NON-PARTICIPANTS and are counted in NONE of the three fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AstCoverage {
    /// Files that are AST-eligible (language has a grammar, size ≤ cap).
    pub size_eligible_files: u64,
    /// Total files that exceed the AST size cap (authoritative; may exceed `excluded.len()`).
    pub size_excluded_files: u64,
    /// Files with tree-sitter language but unknown size in the manifest.
    pub undetermined_files: u64,
    /// Per-language breakdown of excluded files.
    ///
    /// Keys are language names (lowercase, e.g. `"rust"`).
    pub excluded_by_lang: BTreeMap<String, u64>,
    /// Bounded path-sorted sample of excluded files (≤ `AST_COVERAGE_EXCLUDED_SAMPLE_CAP`).
    ///
    /// `size_excluded_files` is the authoritative total; this list is display-only.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<AstExcludedFile>,
}

impl AstCoverage {
    /// Returns `true` when this coverage report contains nothing to notice
    /// (all counts zero, no excluded files).
    ///
    /// Used by emission sites to decide whether to print the coverage notice
    /// and whether to include `ast_coverage` in the JSON output.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.size_excluded_files == 0 && self.undetermined_files == 0
    }
}

/// An abstract description of one manifest entry for coverage computation.
///
/// Callers convert their concrete manifest entry type to this flat form.
/// Keeping this type generic avoids a direct dependency between `rskim-search`
/// and the concrete `ManifestEntry` in `rskim/src/cmd/search/manifest.rs`.
///
/// ## AD-405-5: zero-clone iteration
///
/// `path` and `lang` are borrowed `&str` so the O(N) iterator in
/// `FileManifest::ast_coverage` pays no allocation cost.  String heap
/// allocations for the bounded excluded sample happen only after the
/// fast-reject guard inside [`insert_into_bounded_sample`].  Because the
/// caller iterates a `BTreeMap` in ascending (non-decreasing) path order,
/// once the sample is full every subsequent entry is fast-rejected without
/// allocating, so at most `AST_COVERAGE_EXCLUDED_SAMPLE_CAP` (10) allocations
/// occur across all size-excluded entries.  For arbitrary insertion order the
/// allocation count may be higher.
#[derive(Debug, Clone, Copy)]
pub struct CoverageEntry<'a> {
    /// Repo-relative path of the file.
    pub path: &'a str,
    /// Language name as stored in the manifest (e.g. `"rust"`).  May be
    /// unparseable if the manifest was written by an old version.
    pub lang: &'a str,
    /// File size in bytes at the time the manifest was written (`None` if absent).
    pub size: Option<u64>,
}

/// Compute `AstCoverage` from an iterator of indexed manifest entries.
///
/// ## Contract
///
/// - **Single O(N) pass** — counts and the bounded excluded sample are computed
///   in one sweep.  Per-invocation allocation is O(`AST_COVERAGE_EXCLUDED_SAMPLE_CAP`),
///   not O(excluded).  (AD-405-3 / AD-405-5 / AC-405-12)
/// - **Walk-skipped files excluded** — callers must pass ONLY indexed entries.
///   Entries from the manifest's `skipped_entries` map (files > 5 MiB walk cap)
///   are disjoint by construction and must NOT be passed here (AC-405-14).
/// - **Language resolve (AD-405-6):** stored `lang` string is parsed first via
///   `Language::from_name`; if that fails, `Language::from_path` is tried on the
///   `path`.  Only if BOTH fail is the file counted as UNDETERMINED.
///
/// ## AD-405-5: Bounded sample algorithm
///
/// We maintain a `BTreeMap<String, AstExcludedFile>` keyed on path.  When the
/// map reaches `AST_COVERAGE_EXCLUDED_SAMPLE_CAP + 1` entries we remove the
/// last entry (lexicographically largest path) to keep the MAP bounded at
/// exactly `cap` entries throughout.  At the end the map contains the
/// lexicographically smallest `cap` paths.  This is O(N log cap) — correct and
/// cheap.  The two-phase insert-then-evict logic is encapsulated in
/// [`insert_into_bounded_sample`] to keep this function readable.
#[must_use]
pub fn ast_coverage<'a>(entries: impl IntoIterator<Item = CoverageEntry<'a>>) -> AstCoverage {
    let mut size_eligible_files: u64 = 0;
    let mut size_excluded_files: u64 = 0;
    let mut undetermined_files: u64 = 0;
    let mut excluded_by_lang: HashMap<String, u64> = HashMap::new();
    // BTreeMap keyed by path so the bounded sample is path-sorted (PF-012).
    let mut sample_map: BTreeMap<String, AstExcludedFile> = BTreeMap::new();

    for entry in entries {
        // Two-tier language resolve (AD-405-6).
        // `entry.path` / `entry.lang` are `&str` (Copy): no allocation here.
        let lang_opt = Language::from_name(entry.lang)
            .or_else(|| Language::from_path(std::path::Path::new(entry.path)));

        let Some(lang) = lang_opt else {
            // Language unknown — UNDETERMINED (not a non-participant, the path
            // might actually be a tree-sitter file we cannot classify).
            // Conservative: count as undetermined rather than silently skip.
            undetermined_files += 1;
            continue;
        };

        let Some(cap) = ast_size_limit(lang) else {
            // JSON / YAML / TOML — NON-PARTICIPANT: not counted in any field.
            continue;
        };

        match entry.size {
            None => {
                // Size absent in manifest — cannot classify; count as undetermined.
                undetermined_files += 1;
            }
            Some(sz) if sz <= cap => {
                // SIZE-ELIGIBLE: boundary is ≤ (a file of exactly `cap` bytes is eligible).
                size_eligible_files += 1;
            }
            Some(sz) => {
                // SIZE-EXCLUDED: sz > cap.
                size_excluded_files += 1;
                let lang_name = lang.as_str();
                *excluded_by_lang.entry(lang_name.to_string()).or_insert(0) += 1;

                // Bounded sample: keep the `AST_COVERAGE_EXCLUDED_SAMPLE_CAP`
                // lexicographically smallest paths (AD-405-5 / PF-012).
                // Pass `&str` fields — String/AstExcludedFile are allocated
                // INSIDE insert_into_bounded_sample, AFTER the fast-reject
                // guard.  Because the BTreeMap-ascending iterator feeds paths
                // in non-decreasing order, at most `cap` allocations occur
                // across all size-excluded entries (AD-405-5 zero-clone
                // iteration).
                insert_into_bounded_sample(
                    &mut sample_map,
                    entry.path,
                    lang_name,
                    ExcludedSizes {
                        size_bytes: sz,
                        limit_bytes: cap,
                    },
                    AST_COVERAGE_EXCLUDED_SAMPLE_CAP,
                );
            }
        }
    }

    // Collect the bounded sample in ascending path order (already sorted by BTreeMap).
    let excluded: Vec<AstExcludedFile> = sample_map.into_values().collect();

    // Convert excluded_by_lang to BTreeMap for deterministic JSON key order.
    let excluded_by_lang: BTreeMap<String, u64> = excluded_by_lang.into_iter().collect();

    AstCoverage {
        size_eligible_files,
        size_excluded_files,
        undetermined_files,
        excluded_by_lang,
        excluded,
    }
}

// ============================================================================
// Internal helpers
// ============================================================================

/// Insert a new excluded-file entry into `map`, keeping at most `cap`
/// lexicographically smallest paths (AD-405-5).
///
/// Accepts borrowed `path` and `lang` (`&str`) so that `String` heap
/// allocation is deferred until AFTER the fast-reject guard.  The two `u64`
/// measurements are bundled in [`ExcludedSizes`] to prevent silent
/// transposition at the call site.
///
/// **Allocation bound:** for non-decreasing (ascending) path input, once the
/// sample is full every subsequent entry hits Phase 1 and returns without
/// allocating, so at most `cap` allocations occur across all calls (AD-405-5).
/// For arbitrary insertion order the allocation count may be higher — up to
/// O(N) in the worst (strictly-descending) case.  The current caller feeds a
/// `BTreeMap`-ascending iterator so the O(cap) bound holds in practice.
///
/// Three-phase algorithm:
/// 1. **Fast-reject** — if the map is already full and `path` is ≥ the
///    current maximum, it would be evicted immediately after insertion, so
///    return early with zero allocation.
/// 2. **Allocate + insert** — only now construct the owned `String` key and
///    `AstExcludedFile` value and insert them.
/// 3. **Evict** — if the map now exceeds `cap`, remove the lexicographically
///    largest entry.
///
/// The result is that `map` always holds the `cap` smallest paths seen so far,
/// with O(log cap) cost per call.
fn insert_into_bounded_sample(
    map: &mut BTreeMap<String, AstExcludedFile>,
    path: &str,
    lang: &str,
    sizes: ExcludedSizes,
    cap: usize,
) {
    // Phase 1: fast-reject when the map is full and path would be evicted.
    // Comparison uses `&str` — no allocation at all for rejected entries.
    if map.len() >= cap
        && let Some(last) = map.keys().next_back()
        && path >= last.as_str()
    {
        return;
    }
    // Phase 2: allocate only after the fast-reject guard.
    let owned_path = path.to_owned();
    let file = AstExcludedFile {
        path: owned_path.clone(),
        lang: lang.to_owned(),
        size_bytes: sizes.size_bytes,
        limit_bytes: sizes.limit_bytes,
        reason: "ast_size_cap".to_owned(),
    };
    map.insert(owned_path, file);
    // Phase 3: evict the largest entry if we just exceeded the cap.
    if map.len() > cap
        && let Some(last_key) = map.keys().next_back().cloned()
    {
        map.remove(&last_key);
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn entry<'a>(path: &'a str, lang: &'a str, size: Option<u64>) -> CoverageEntry<'a> {
        CoverageEntry { path, lang, size }
    }

    /// AC-405-3: Coverage predicate matrix.
    #[test]
    fn coverage_predicate_matrix() {
        let entries = vec![
            entry("a.rs", "rust", Some(1_048_576)), // exactly cap → eligible
            entry("b.rs", "rust", Some(1_048_577)), // 1 byte over → excluded
            entry("c.sql", "sql", Some(153_600)),   // 150 KiB → eligible under 1 MiB cap
            entry("d.sql", "sql", Some(2_097_152)), // 2 MiB → excluded
            entry("e.json", "json", Some(2_097_152)), // non-participant
            entry("f.rs", "rust", None),            // undetermined
        ];
        let cov = ast_coverage(entries);

        assert_eq!(cov.size_eligible_files, 2, "a.rs + c.sql");
        assert_eq!(cov.size_excluded_files, 2, "b.rs + d.sql");
        assert_eq!(cov.undetermined_files, 1, "f.rs");
        assert_eq!(cov.excluded.len(), 2);
        // Boundary: exactly 1048576 bytes is ELIGIBLE (not excluded).
        assert!(
            cov.excluded.iter().all(|e| e.size_bytes > 1_048_576),
            "boundary: cap-exact file must not appear in excluded"
        );
    }

    /// AC-405-3: Language fallback via path extension when lang string is unparseable.
    #[test]
    fn lang_fallback_via_path() {
        // Simulate an old manifest that stored the lang differently.
        let entries = vec![entry("src/weird_name.rs", "UNKNOWN_LANG", Some(2_000_000))];
        let cov = ast_coverage(entries);
        // Language::from_path recognises .rs → Rust → Some(cap); 2 MiB > cap → excluded.
        assert_eq!(cov.size_excluded_files, 1);
        assert_eq!(cov.undetermined_files, 0);
    }

    /// AC-405-3: Boundary is `>` not `>=` (cap-exact is ELIGIBLE).
    #[test]
    fn boundary_is_exclusive() {
        let cap = rskim_core::AST_SIZE_LIMIT_DEFAULT;
        let entries = vec![
            entry("exact.rs", "rust", Some(cap)),        // eligible
            entry("one_over.rs", "rust", Some(cap + 1)), // excluded
        ];
        let cov = ast_coverage(entries);
        assert_eq!(cov.size_eligible_files, 1);
        assert_eq!(cov.size_excluded_files, 1);
    }

    /// AC-405-5 / AD-405-5: Bounded sample is path-sorted and capped at 10.
    #[test]
    fn bounded_sample_is_path_sorted_and_capped() {
        let cap = rskim_core::AST_SIZE_LIMIT_DEFAULT;
        // Create 15 excluded files; paths are out-of-alpha order.
        // Collect owned path strings first so their lifetimes outlive the
        // `CoverageEntry<'_>` borrows produced by the iterator below.
        let paths: Vec<String> = (0..15u64)
            .map(|i| format!("file{:02}.rs", 14 - i))
            .collect();
        let cov = ast_coverage(paths.iter().enumerate().map(|(i, p)| CoverageEntry {
            path: p.as_str(),
            lang: "rust",
            size: Some(cap + 1 + i as u64),
        }));

        assert_eq!(cov.size_excluded_files, 15, "authoritative total");
        assert_eq!(
            cov.excluded.len(),
            AST_COVERAGE_EXCLUDED_SAMPLE_CAP,
            "sample capped at 10"
        );
        // Verify path-sorted order.
        let paths: Vec<&str> = cov.excluded.iter().map(|e| e.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort_unstable();
        assert_eq!(paths, sorted, "sample must be path-sorted ascending");
        // Verify the 10 SMALLEST paths are kept.
        assert_eq!(paths[0], "file00.rs");
        assert_eq!(paths[9], "file09.rs");
    }

    /// AC-405-5: `insert_into_bounded_sample` fast-rejects paths that arrive in
    /// ascending order once the sample cap is reached.
    ///
    /// The existing `bounded_sample_is_path_sorted_and_capped` test feeds paths
    /// in descending order, so every over-cap entry evicts an existing one
    /// (Phase 3) rather than triggering the Phase 1 early-return.  This test
    /// uses strictly ascending paths to exercise the fast-reject branch:
    /// once the sample is full, any new path >= current max must be rejected
    /// without allocating an `AstExcludedFile`.
    #[test]
    fn bounded_sample_fast_rejects_ascending_order() {
        use std::collections::BTreeMap;
        let size_bytes = rskim_core::AST_SIZE_LIMIT_DEFAULT + 1;
        let limit_bytes = rskim_core::AST_SIZE_LIMIT_DEFAULT;
        const CAP: usize = 2;
        let mut map: BTreeMap<String, AstExcludedFile> = BTreeMap::new();
        // Insert 5 paths in strictly ascending (lexicographic) order.
        // After the first CAP insertions the map is full; every subsequent
        // path is lexicographically greater than the current maximum and must
        // be fast-rejected (Phase 1 returns immediately).
        for i in 0u64..5 {
            insert_into_bounded_sample(
                &mut map,
                &format!("file{:02}.rs", i),
                "rust",
                ExcludedSizes {
                    size_bytes,
                    limit_bytes,
                },
                CAP,
            );
        }
        // Only the CAP smallest paths should be retained.
        assert_eq!(map.len(), CAP, "sample must be capped at {CAP}");
        let keys: Vec<&str> = map.keys().map(|s| s.as_str()).collect();
        assert_eq!(keys, vec!["file00.rs", "file01.rs"]);
        // The fast-rejected entries (file02..file04) must be absent.
        assert!(!map.contains_key("file02.rs"));
        assert!(!map.contains_key("file03.rs"));
        assert!(!map.contains_key("file04.rs"));
    }

    /// AC-405-14: Data-format files are NON-PARTICIPANTS (never counted).
    #[test]
    fn data_format_files_are_non_participants() {
        let entries = vec![
            entry("data.json", "json", Some(2_097_152)),
            entry("config.yaml", "yaml", Some(2_097_152)),
            entry("Cargo.toml", "toml", Some(2_097_152)),
        ];
        let cov = ast_coverage(entries);
        assert_eq!(cov.size_eligible_files, 0);
        assert_eq!(cov.size_excluded_files, 0);
        assert_eq!(cov.undetermined_files, 0);
        assert!(cov.excluded.is_empty());
        assert!(cov.is_clean());
    }

    /// AC-405-6: `is_clean()` returns true iff no excluded or undetermined files.
    #[test]
    fn is_clean_logic() {
        let clean = ast_coverage(vec![entry("a.rs", "rust", Some(100))]);
        assert!(clean.is_clean());

        let dirty = ast_coverage(vec![entry(
            "a.rs",
            "rust",
            Some(rskim_core::AST_SIZE_LIMIT_DEFAULT + 1),
        )]);
        assert!(!dirty.is_clean());
    }
}
