//! Relevance judgment (qrel) generation.
//!
//! Takes a collection of indexed files, extracts named symbols from each using
//! language-specific AST extractors, applies filters, and stratifies the result
//! into a balanced set of `Qrel` values suitable for IR evaluation.
//!
//! # Pipeline
//!
//! 1. Extract symbols from each file; compute DF per symbol name in the same pass
//! 2. Filter: name ≥ 4 bytes, at least one alpha character
//! 3. Deduplicate: first occurrence wins (deterministic if files are sorted)
//! 4. Filter: DF ≤ 5 (exclude overly common symbols)
//! 5. Stratify: ~15 TypeDefinition, ~15 FunctionSignature, ~10 ImportExport, ~10 SymbolName
//! 6. Error if < 10 queries remain

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::bail;

use rskim_search::{FileId, SearchField};

use crate::extract;
use crate::types::Qrel;

/// Maximum document frequency (DF) for a symbol to be included.
///
/// Symbols that appear in more than this many files are too common to be
/// useful as discriminative queries.
const MAX_DF: usize = 5;

/// Minimum symbol name length in bytes.
const MIN_NAME_LEN: usize = 4;

/// Target counts per field type (soft targets for stratification).
const TARGET_TYPE_DEFINITION: usize = 15;
const TARGET_FUNCTION_SIGNATURE: usize = 15;
const TARGET_IMPORT_EXPORT: usize = 10;
const TARGET_SYMBOL_NAME: usize = 10;

/// Minimum queries required after all filtering.
const MIN_QUERIES: usize = 10;

/// An input file for qrel generation.
#[derive(Debug)]
pub struct QrelInput<'a> {
    pub file_id: FileId,
    pub path: PathBuf,
    pub language: rskim_core::Language,
    pub content: &'a str,
}

/// Generate relevance judgments from a collection of indexed files.
///
/// Files must be provided in deterministic (sorted) order so that
/// deduplication is reproducible.
///
/// # Errors
///
/// Returns an error if fewer than `MIN_QUERIES` qrels are generated after
/// all filtering and stratification.
pub fn generate_qrels(files: &[QrelInput<'_>]) -> anyhow::Result<Vec<Qrel>> {
    // Phase 1: Extract all symbols and compute document frequency (DF) in one pass.
    // DF = number of distinct files that define a given name (before deduplication).
    let mut raw_symbols: Vec<(FileId, crate::extract::ExtractedSymbol)> = Vec::new();
    let mut df_map: HashMap<String, HashSet<FileId>> = HashMap::new();

    for file in files {
        let symbols = extract::extract_symbols(&file.path, file.content, file.language);
        for sym in symbols {
            let passes_filter =
                sym.name.len() >= MIN_NAME_LEN && sym.name.chars().any(|c| c.is_alphabetic());
            if passes_filter {
                df_map
                    .entry(sym.name.clone())
                    .or_default()
                    .insert(file.file_id);
                raw_symbols.push((file.file_id, sym));
            }
        }
    }

    // Phase 2 (implicit): filter was applied inline above; raw_symbols contains
    // only symbols that passed name-length and alphabetic checks.

    // Phase 3: Deduplicate — first occurrence of each name wins
    let mut seen_names: HashSet<String> = HashSet::new();
    let deduped: Vec<(FileId, crate::extract::ExtractedSymbol)> = raw_symbols
        .into_iter()
        .filter(|(_, sym)| seen_names.insert(sym.name.clone()))
        .collect();

    // Phase 4: Apply DF filter to deduped candidates.
    // Every name in deduped is guaranteed to be in df_map (built in Phase 1),
    // so the default branch is unreachable in practice.
    let df_filtered: Vec<(FileId, crate::extract::ExtractedSymbol)> = deduped
        .into_iter()
        .filter(|(_, sym)| {
            df_map
                .get(&sym.name)
                .is_none_or(|ids| ids.len() <= MAX_DF)
        })
        .collect();

    // Phase 5: Stratify by field type
    let qrels = stratify(df_filtered);

    // Phase 6: Validate minimum count
    if qrels.len() < MIN_QUERIES {
        bail!(
            "Too few qrels after filtering: {} (minimum required: {}). \
             Consider using a larger corpus or relaxing DF constraints.",
            qrels.len(),
            MIN_QUERIES
        );
    }

    Ok(qrels)
}

/// Stratify candidates into a balanced set of qrels.
///
/// Targets: ~15 TypeDefinition, ~15 FunctionSignature, ~10 ImportExport, ~10 SymbolName.
/// If a field type yields < 5 symbols, backfill from the largest available pool.
fn stratify(candidates: Vec<(FileId, crate::extract::ExtractedSymbol)>) -> Vec<Qrel> {
    let mut by_field: HashMap<SearchField, Vec<(FileId, crate::extract::ExtractedSymbol)>> =
        HashMap::new();

    for (fid, sym) in candidates {
        by_field.entry(sym.field).or_default().push((fid, sym));
    }

    let targets = [
        (SearchField::TypeDefinition, TARGET_TYPE_DEFINITION),
        (SearchField::FunctionSignature, TARGET_FUNCTION_SIGNATURE),
        (SearchField::ImportExport, TARGET_IMPORT_EXPORT),
        (SearchField::SymbolName, TARGET_SYMBOL_NAME),
    ];

    let mut result: Vec<Qrel> = Vec::new();
    let mut deficits: Vec<(SearchField, usize)> = Vec::new();

    for (field, target) in targets {
        let pool = by_field.remove(&field).unwrap_or_default();
        let take = pool.len().min(target);
        let deficit = if take < 5 { target - take } else { 0 };

        for (fid, sym) in pool.iter().take(take) {
            result.push(Qrel {
                query: sym.name.clone(),
                relevant_file_id: *fid,
                field: sym.field,
            });
        }

        if deficit > 0 {
            deficits.push((field, deficit));
        }
    }

    // Backfill: if any field had < 5 symbols, take from the largest available pool
    if !deficits.is_empty() {
        // Find the largest remaining pool across all field types
        let mut overflow_pool: Vec<(FileId, crate::extract::ExtractedSymbol)> =
            by_field.into_values().flatten().collect();
        // Sort for determinism
        overflow_pool.sort_by(|a, b| a.1.name.cmp(&b.1.name));

        let total_deficit: usize = deficits.iter().map(|(_, d)| *d).sum();
        let take = overflow_pool.len().min(total_deficit);

        for (fid, sym) in overflow_pool.iter().take(take) {
            result.push(Qrel {
                query: sym.name.clone(),
                relevant_file_id: *fid,
                field: sym.field,
            });
        }
    }

    result
}

/// Verify all qrel `relevant_file_id` values exist in the provided set of
/// indexed file IDs.
///
/// # Errors
///
/// Returns an error listing which file IDs are missing from the index.
pub fn validate_qrel_coverage(qrels: &[Qrel], indexed_ids: &HashSet<FileId>) -> anyhow::Result<()> {
    let missing: Vec<FileId> = qrels
        .iter()
        .filter(|q| !indexed_ids.contains(&q.relevant_file_id))
        .map(|q| q.relevant_file_id)
        .collect();

    if !missing.is_empty() {
        bail!(
            "Qrel coverage check failed: {} file IDs not found in index: {:?}",
            missing.len(),
            &missing[..missing.len().min(5)]
        );
    }
    Ok(())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)] // test code — unwrap/expect acceptable for test assertions
mod tests {
    use super::*;
    use rskim_core::Language;

    fn make_file<'a>(id: u32, lang: Language, content: &'a str) -> QrelInput<'a> {
        QrelInput {
            file_id: FileId(id),
            path: PathBuf::from(format!("file_{id}.rs")),
            language: lang,
            content,
        }
    }

    #[test]
    fn generates_qrels_from_rust_file() {
        let content = r#"
pub fn add_numbers(a: i32, b: i32) -> i32 { a + b }
pub fn greet_user(name: &str) -> String { format!("{}", name) }
pub struct Calculator { value: i32 }
pub enum Status { Active, Inactive }
pub fn compute_total(x: f64) -> f64 { x * 2.0 }
pub fn validate_input(s: &str) -> bool { !s.is_empty() }
pub struct Logger;
pub struct Buffer { data: Vec<u8> }
pub enum Direction { North, South, East, West }
pub fn parse_config(path: &str) -> Option<String> { None }
pub fn format_output(v: i32) -> String { format!("{}", v) }
"#;
        let file = make_file(0, Language::Rust, content);
        let qrels = generate_qrels(&[file]).unwrap();
        assert!(
            !qrels.is_empty(),
            "should generate at least some qrels from Rust content"
        );
    }

    #[test]
    fn filter_short_names() {
        // "add" is 3 bytes → should be filtered out (< 4 bytes)
        let content = r#"
pub fn add(a: i32, b: i32) -> i32 { a + b }
pub fn longer_name(x: i32) -> i32 { x }
pub struct LargeStruct { data: Vec<u8> }
pub struct SmallType;
pub fn process_data(d: &[u8]) -> Vec<u8> { d.to_vec() }
pub fn validate_item(s: &str) -> bool { true }
pub fn handle_error(e: &str) -> String { e.to_string() }
pub struct EventLoop;
pub enum ColorMode { Light, Dark }
pub fn start_server(port: u16) -> bool { true }
pub fn stop_service(name: &str) {}
"#;
        let file = make_file(0, Language::Rust, content);
        let qrels = generate_qrels(&[file]).unwrap();
        // "add" should not appear in qrels
        let has_add = qrels.iter().any(|q| q.query == "add");
        assert!(!has_add, "'add' (3 bytes) should be filtered out");
    }

    #[test]
    fn deduplication_keeps_first_occurrence() {
        // Two files both define "calculate" — only the first file's ID should appear
        let file0 = make_file(
            0,
            Language::Rust,
            r#"
pub fn calculate_sum(a: i32) -> i32 { a }
pub fn process_data(x: i32) -> i32 { x }
pub struct DataStore { items: Vec<i32> }
pub enum EventType { Created, Deleted }
pub fn handle_request(r: &str) -> bool { true }
pub fn send_response(code: u32) {}
pub fn load_config(path: &str) -> String { String::new() }
pub fn validate_token(t: &str) -> bool { true }
pub fn parse_arguments(args: &[String]) -> bool { true }
pub fn cleanup_resources() {}
"#,
        );
        let file1 = make_file(
            1,
            Language::Rust,
            r#"
pub fn calculate_sum(b: f64) -> f64 { b }
pub fn other_function(y: i32) -> i32 { y }
"#,
        );
        let qrels = generate_qrels(&[file0, file1]).unwrap();
        let calc_qrels: Vec<&Qrel> = qrels
            .iter()
            .filter(|q| q.query == "calculate_sum")
            .collect();
        // Should only appear once (first occurrence)
        assert_eq!(calc_qrels.len(), 1, "should deduplicate 'calculate_sum'");
        // First file wins
        assert_eq!(
            calc_qrels[0].relevant_file_id,
            FileId(0),
            "first file should win deduplication"
        );
    }

    #[test]
    fn max_df_filter_excludes_common_symbols() {
        // Create 6 files all defining the same symbol → DF=6 > MAX_DF=5, should be excluded
        let mut files: Vec<QrelInput> = (0..6u32)
            .map(|i| {
                make_file(
                    i,
                    Language::Rust,
                    r#"
pub fn common_function_name(x: i32) -> i32 { x }
pub fn unique_helper_one() {}
"#,
                )
            })
            .collect();

        // Add one file with truly unique symbols so we pass the minimum check
        files.push(make_file(
            99,
            Language::Rust,
            r#"
pub fn unique_first_function(a: i32) -> i32 { a }
pub fn unique_second_function(b: i32) -> i32 { b }
pub fn unique_third_function(c: i32) -> i32 { c }
pub struct UniqueFirstStruct { x: i32 }
pub struct UniqueSecondStruct { y: i32 }
pub struct UniqueThirdStruct { z: i32 }
pub fn unique_fourth_function(d: i32) -> i32 { d }
pub fn unique_fifth_function(e: i32) -> i32 { e }
pub enum UniqueFirstEnum { A, B }
pub fn unique_sixth_function(f: i32) -> i32 { f }
pub fn unique_seventh_function(g: i32) -> i32 { g }
"#,
        ));

        let qrels = generate_qrels(&files).unwrap();
        let common = qrels.iter().find(|q| q.query == "common_function_name");
        assert!(
            common.is_none(),
            "common_function_name (DF=6) should be excluded by max-DF filter"
        );
    }

    #[test]
    fn error_when_too_few_qrels() {
        // Single file with very few short symbols → should fail
        let file = make_file(0, Language::Rust, "fn x() {}");
        let result = generate_qrels(&[file]);
        assert!(
            result.is_err(),
            "should error when too few qrels are generated"
        );
    }

    #[test]
    fn validate_qrel_coverage_ok() {
        let qrels = vec![
            Qrel {
                query: "foo".to_string(),
                relevant_file_id: FileId(0),
                field: SearchField::FunctionSignature,
            },
            Qrel {
                query: "bar".to_string(),
                relevant_file_id: FileId(1),
                field: SearchField::TypeDefinition,
            },
        ];
        let indexed: HashSet<FileId> = [FileId(0), FileId(1)].into_iter().collect();
        validate_qrel_coverage(&qrels, &indexed).expect("all file IDs present");
    }

    #[test]
    fn validate_qrel_coverage_missing_id() {
        let qrels = vec![Qrel {
            query: "foo".to_string(),
            relevant_file_id: FileId(99),
            field: SearchField::FunctionSignature,
        }];
        let indexed: HashSet<FileId> = [FileId(0), FileId(1)].into_iter().collect();
        let result = validate_qrel_coverage(&qrels, &indexed);
        assert!(result.is_err(), "should fail when file ID is missing");
    }

    #[test]
    fn qrels_have_distinct_field_types() {
        // Use content with at least two field types
        let content = r#"
pub fn compute_value(x: i32) -> i32 { x }
pub fn process_item(s: &str) -> String { s.to_string() }
pub fn handle_event(e: u32) {}
pub struct DataModel { id: u32 }
pub struct UserRecord { name: String }
pub struct ConfigEntry { key: String }
pub fn validate_data(d: &str) -> bool { true }
pub fn format_output(v: i32) -> String { format!("{}", v) }
pub fn load_resource(path: &str) -> Vec<u8> { vec![] }
pub fn save_state(key: &str, val: i32) {}
pub fn init_logger(level: u8) {}
pub enum LogLevel { Debug, Info, Warn, Error }
"#;
        let file = make_file(0, Language::Rust, content);
        let qrels = generate_qrels(&[file]).unwrap();

        let fields: HashSet<SearchField> = qrels.iter().map(|q| q.field).collect();
        assert!(
            fields.len() >= 2,
            "should have at least 2 distinct field types, got: {fields:?}"
        );
    }
}
