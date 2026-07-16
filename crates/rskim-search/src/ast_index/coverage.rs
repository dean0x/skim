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
#[derive(Debug, Clone)]
pub struct CoverageEntry {
    /// Repo-relative path of the file.
    pub path: String,
    /// Language name as stored in the manifest (e.g. `"rust"`).  May be
    /// unparseable if the manifest was written by an old version.
    pub lang: String,
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
pub fn ast_coverage(entries: impl IntoIterator<Item = CoverageEntry>) -> AstCoverage {
    let mut size_eligible_files: u64 = 0;
    let mut size_excluded_files: u64 = 0;
    let mut undetermined_files: u64 = 0;
    let mut excluded_by_lang: HashMap<String, u64> = HashMap::new();
    // BTreeMap keyed by path so the bounded sample is path-sorted (PF-012).
    let mut sample_map: BTreeMap<String, AstExcludedFile> = BTreeMap::new();

    for entry in entries {
        // Two-tier language resolve (AD-405-6).
        let lang_opt = Language::from_name(&entry.lang)
            .or_else(|| Language::from_path(std::path::Path::new(&entry.path)));

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
                insert_into_bounded_sample(
                    &mut sample_map,
                    entry.path.clone(),
                    AstExcludedFile {
                        path: entry.path,
                        lang: lang_name.to_string(),
                        size_bytes: sz,
                        limit_bytes: cap,
                        reason: "ast_size_cap".to_string(),
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

/// Insert `path`→`file` into `map`, keeping at most `cap` lexicographically
/// smallest entries (AD-405-5).
///
/// Two-phase algorithm:
/// 1. **Fast-reject** — if the map is already full and `path` is ≥ the current
///    maximum, it would be evicted immediately after insertion, so skip it.
/// 2. **Insert** — add the entry unconditionally.
/// 3. **Evict** — if the map now exceeds `cap`, remove the lexicographically
///    largest entry.
///
/// The result is that `map` always holds the `cap` smallest paths seen so far,
/// with O(log cap) cost per call and O(cap) total allocation.
fn insert_into_bounded_sample(
    map: &mut BTreeMap<String, AstExcludedFile>,
    path: String,
    file: AstExcludedFile,
    cap: usize,
) {
    // Phase 1: fast-reject when the map is full and path would be evicted.
    if map.len() >= cap {
        if let Some(last) = map.keys().next_back() {
            if path >= *last {
                return;
            }
        }
    }
    // Phase 2: insert.
    map.insert(path, file);
    // Phase 3: evict the largest entry if we just exceeded the cap.
    if map.len() > cap {
        if let Some(last_key) = map.keys().next_back().cloned() {
            map.remove(&last_key);
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, lang: &str, size: Option<u64>) -> CoverageEntry {
        CoverageEntry {
            path: path.to_string(),
            lang: lang.to_string(),
            size,
        }
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
        let entries: Vec<CoverageEntry> = (0..15u64)
            .map(|i| entry(&format!("file{:02}.rs", 14 - i), "rust", Some(cap + 1 + i)))
            .collect();
        let cov = ast_coverage(entries);

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
