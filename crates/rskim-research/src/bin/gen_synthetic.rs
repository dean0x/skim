//! Generates a synthetic bigram weight table from fixture files + common code bigrams.
//! Used to bootstrap the checked-in bigram_weights.json without network access.
//!
//! Run: cargo run -p rskim-research --bin gen_synthetic -- <output_path>

use std::collections::HashMap;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    use rskim_research::extract::{encode_bigram, extract_bigrams_from_corpus};
    use rskim_research::idf::compute_weight_table;
    use rskim_research::types::{CorpusStats, LanguageCount, SourceFile, WeightTable};

    let output_path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            // Default: workspace root / crates/rskim-search/data/bigram_weights.json
            rskim_research::codegen::find_workspace_root()
                .map(|root| root.join("crates/rskim-search/data/bigram_weights.json"))
                .unwrap_or_else(|_| PathBuf::from("bigram_weights.json"))
        });

    eprintln!(
        "Generating synthetic weight table -> {}",
        output_path.display()
    );

    // ---- Load fixture files ----
    let fixtures_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let fixture_files = rskim_research::clone::load_fixture_files(&fixtures_dir)?;
    eprintln!("Loaded {} fixture files", fixture_files.len());

    let (fixture_df, _) = extract_bigrams_from_corpus(&fixture_files);

    // ---- Generate common code bigrams ----
    // We generate bigrams from representative code patterns across all 5 target languages.
    let code_samples: &[&str] = &[
        // Rust
        "fn main() { let x = 42; println!(\"{}\", x); }",
        "pub fn parse(input: &str) -> Result<Config, Error> {",
        "impl Iterator for MyIter { type Item = u32;",
        "struct Config { host: String, port: u16, }",
        "enum State { Running, Stopped, Error(String), }",
        "use std::collections::{HashMap, HashSet};",
        "#[derive(Debug, Clone, Serialize, Deserialize)]",
        "async fn fetch(url: &str) -> anyhow::Result<String> {",
        "match self { State::Running => true, State::Stopped => false, }",
        "let mut map: HashMap<String, Vec<u32>> = HashMap::new();",
        "pub trait FileSource: Send + Sync {",
        "impl<T: Debug + Clone> MyStruct<T> {",
        "#[cfg(test)] mod tests { use super::*;",
        "const MAX_SIZE: usize = 1024 * 1024;",
        "type Result<T> = std::result::Result<T, Error>;",
        "for (key, value) in map.iter() {",
        "if let Some(x) = option { process(x); }",
        "let bytes: Vec<u8> = content.as_bytes().to_vec();",
        "#[must_use] pub fn compute(&self) -> f64 {",
        "mod extract; mod idf; mod validate; mod codegen;",
        // TypeScript
        "export function parse(input: string): Result<Config> {",
        "interface User { id: number; name: string; email: string; }",
        "class UserService { private users: User[] = [];",
        "const fetchData = async (url: string): Promise<Response> => {",
        "import { useState, useEffect } from 'react';",
        "type Result<T, E = Error> = { ok: true; value: T } | { ok: false; error: E };",
        "export default function Component({ name }: Props): JSX.Element {",
        "const map = new Map<string, number>();",
        "if (!result.ok) return { ok: false, error: result.error };",
        "export type { User, Config, Result };",
        "const schema = z.object({ name: z.string(), age: z.number() });",
        "switch (action.type) { case 'INCREMENT': return state + 1;",
        "async function* stream(url: string): AsyncIterable<Chunk> {",
        "type DeepPartial<T> = { [K in keyof T]?: DeepPartial<T[K]> };",
        "const arr = items.filter(Boolean).map(x => x.value);",
        // Python
        "def parse_config(path: str) -> dict:",
        "class UserService:",
        "    def __init__(self, db: Database) -> None:",
        "async def fetch(url: str) -> bytes:",
        "from typing import Optional, List, Dict, Any",
        "if __name__ == '__main__':",
        "@dataclass class Config:",
        "    host: str = 'localhost'",
        "for key, value in mapping.items():",
        "return [x for x in items if x is not None]",
        "with open(path, 'r') as f:",
        "import asyncio, logging, json, os",
        "raise ValueError(f'invalid input: {value}')",
        "result = [f(x) for x in range(n) if pred(x)]",
        "logger = logging.getLogger(__name__)",
        // Go
        "func main() {",
        "func (s *Server) Handle(w http.ResponseWriter, r *http.Request) {",
        "type Config struct { Host string `json:\"host\"` Port int `json:\"port\"` }",
        "if err != nil { return fmt.Errorf(\"parse: %w\", err) }",
        "for k, v := range m {",
        "ch := make(chan struct{}, 1)",
        "go func() { defer wg.Done(); process(item) }()",
        "var buf bytes.Buffer",
        "import ( \"fmt\" \"net/http\" \"context\" )",
        "switch t := v.(type) { case string: fmt.Println(t)",
        "ctx, cancel := context.WithTimeout(ctx, 30*time.Second)",
        "w.Header().Set(\"Content-Type\", \"application/json\")",
        "defer rows.Close()",
        "log.Printf(\"starting server on port %d\", port)",
        // Java
        "public class UserService {",
        "    private final UserRepository repository;",
        "    public UserService(UserRepository repository) {",
        "    @Override public List<User> findAll() {",
        "public interface Repository<T, ID> {",
        "import java.util.*;",
        "@SpringBootApplication public class Application {",
        "try { result = parse(input); } catch (ParseException e) {",
        "Optional<User> user = repository.findById(id);",
        "stream().filter(Objects::nonNull).map(User::getName).collect(Collectors.toList())",
        "Map<String, List<Integer>> grouped = new HashMap<>();",
        "log.debug(\"processing {} items\", items.size());",
        "public static void main(String[] args) throws Exception {",
        "@NotNull @Valid @RequestBody CreateUserRequest request",
        // SQL
        "SELECT id, name, email FROM users WHERE active = true",
        "INSERT INTO orders (user_id, amount) VALUES ($1, $2)",
        "UPDATE users SET updated_at = NOW() WHERE id = $1",
        "CREATE TABLE users (id SERIAL PRIMARY KEY, name TEXT NOT NULL)",
        "JOIN orders ON users.id = orders.user_id",
        "GROUP BY user_id HAVING COUNT(*) > 10",
        // Common patterns
        "return Ok(result);",
        "return Err(e);",
        "pub use types::{Config, Error, Result};",
        "let _ = tx.send(item);",
        "assert_eq!(left, right);",
        "assert!(condition, \"message\");",
        "// TODO: implement this",
        "// SAFETY: we hold the lock",
        "cargo test --all-features",
        "cargo build --release",
    ];

    // Extract bigrams from all code samples with a synthetic total_docs count.
    let mut combined_df: HashMap<u16, u32> = fixture_df;
    let sample_files: Vec<SourceFile> = code_samples
        .iter()
        .map(|&s| SourceFile {
            path: PathBuf::from("synthetic.rs"),
            language: rskim_core::Language::Rust,
            content: s.to_string(),
        })
        .collect();

    let (sample_df, _) = extract_bigrams_from_corpus(&sample_files);
    for (bigram, df) in sample_df {
        *combined_df.entry(bigram).or_default() += df;
    }

    // Add ALL printable ASCII pairs to ensure broad coverage.
    // These get a low synthetic DF to give them moderate IDF.
    let synthetic_total_docs: u32 = 50_000;
    for b1 in 0x20u8..=0x7Eu8 {
        for b2 in 0x20u8..=0x7Eu8 {
            let key = encode_bigram(b1, b2);
            // If we don't have data from corpus, assign a moderate DF
            combined_df.entry(key).or_insert(100);
        }
    }

    // Also add bigrams involving common non-printable bytes in source code:
    // newlines (\n=0x0A), tabs (\t=0x09), carriage returns (\r=0x0D)
    for &special in b"\n\t\r" {
        for b2 in 0x20u8..=0x7Eu8 {
            combined_df.entry(encode_bigram(special, b2)).or_insert(500);
        }
        for b1 in 0x20u8..=0x7Eu8 {
            combined_df.entry(encode_bigram(b1, special)).or_insert(500);
        }
    }

    eprintln!("Combined DF map: {} unique bigrams", combined_df.len());

    // Use a threshold of 0.0 to include everything (we want a comprehensive table).
    let weights = compute_weight_table(&combined_df, synthetic_total_docs, 0.0);

    eprintln!("Weight table: {} entries", weights.len());

    // Count language breakdown from fixtures
    let language_breakdown = vec![
        LanguageCount {
            language: "Rust".to_string(),
            file_count: 2,
        },
        LanguageCount {
            language: "TypeScript".to_string(),
            file_count: 1,
        },
        LanguageCount {
            language: "Python".to_string(),
            file_count: 1,
        },
    ];

    let table = WeightTable {
        version: 1,
        generated_at: "synthetic:2026-05-12".to_string(),
        corpus_stats: CorpusStats {
            total_files: fixture_files.len() as u32 + code_samples.len() as u32,
            total_ngrams: combined_df.values().map(|&v| v as u64).sum(),
            unique_ngrams: weights.len(),
            deduplicated_files: 0,
            language_breakdown,
        },
        weights,
    };

    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_string_pretty(&table)?;
    std::fs::write(&output_path, json)?;

    eprintln!(
        "Written: {} entries to {}",
        table.weights.len(),
        output_path.display()
    );

    Ok(())
}
