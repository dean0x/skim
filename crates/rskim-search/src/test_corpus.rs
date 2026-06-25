//! Shared test corpus helpers for `rskim-search` integration tests.
//!
//! This module is compiled only in `#[cfg(test)]` contexts.  It provides
//! a realistic Rust source generator used by both the lexical index tests
//! (`index/reader_tests.rs`) and the AST index tests
//! (`ast_index/store/reader_tests.rs`) to produce a representative corpus
//! for size-ratio and latency regression guards.
//!
//! # Why a shared module
//!
//! The generator was originally private to `ast_index/store/reader_tests.rs`.
//! Hoisting it here follows the quality.md reuse-over-new principle: a single
//! implementation, exercised from both test sites, avoids drift between the
//! two corpus definitions.

/// Generate a representative Rust source module with `n_fns` functions.
///
/// Each function has a real multi-statement body (not a one-liner) so the
/// source-bytes-per-file are in the hundreds-to-low-thousands range, matching
/// real-world Rust code.  One-liner micro-files are NOT a valid measure of
/// index compactness because fixed per-file overhead (header, `FileMetaEntry`,
/// per-distinct-key entry rows) dwarfs the tiny source.
///
/// # Parameters
///
/// - `file_idx`: unique index included in function names to prevent cross-file
///   n-gram collisions that would artificially deflate corpus diversity.
/// - `n_fns`: number of functions to generate per file.
///
/// # Corpus diversity
///
/// Each generated function contains variable bindings, a loop, and a
/// conditional -- producing several distinct AST node types and a realistic
/// n-gram vocabulary.  The `file_idx` parameter makes every file's symbol
/// names unique, ensuring distinct trigrams across files (realistic posting
/// list lengths, not artificially sparse).
pub fn gen_representative_rust_module(file_idx: usize, n_fns: usize) -> String {
    let mut out = String::with_capacity(512 * n_fns);
    out.push_str("use std::collections::HashMap;\n\n");
    for f in 0..n_fns {
        // Each function has a multi-statement body: variable bindings, a loop,
        // and a conditional -- producing several distinct AST node types and a
        // realistic n-gram vocabulary.
        out.push_str(&format!(
            "pub fn process_{file_idx}_{f}(input: &[i32]) -> i32 {{\n\
             \x20   let mut acc: i32 = 0;\n\
             \x20   let mut count: i32 = 0;\n\
             \x20   for &val in input.iter() {{\n\
             \x20       acc = acc.wrapping_add(val);\n\
             \x20       count += 1;\n\
             \x20   }}\n\
             \x20   if count == 0 {{\n\
             \x20       return 0;\n\
             \x20   }}\n\
             \x20   acc / count\n\
             }}\n\n"
        ));
    }
    out
}
