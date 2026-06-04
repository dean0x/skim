# Reliability Review Report

**Branch**: feature/184-ast-node-frequency-research -> main
**Date**: 2026-05-31

## Issues in Your Changes (BLOCKING)

### HIGH

**`walk_tree` is recursive despite "iterative" doc claim -- stack overflow on depth-500 ASTs** - `crates/rskim-research/src/ast_extract.rs:115`
**Confidence**: 90%
- Problem: The function is documented as "Iterative tree walk using `TreeCursor` to avoid recursion depth limits" (line 108) but the implementation is recursive -- `walk_tree` calls itself at line 171 with `depth + 1`. While `MAX_AST_DEPTH=500` bounds the recursion depth, each stack frame carries 10 arguments (cursor, vocab, two HashSets, a bool, two `&mut u32`, a usize, and two `Option<u16>`). At approximately 200-400 bytes per frame, 500 deep is ~100-200 KiB of stack, which is within the default 8 MiB thread stack but dangerously close to causing issues on non-default configurations (e.g., rayon worker threads with smaller stacks, or WebAssembly runtimes with 1 MiB stacks). The mismatch between documentation and implementation is itself a reliability concern -- a future developer trusting the "iterative" doc may reduce MAX_AST_DEPTH or not consider stack safety.
- Fix: Either (a) correct the doc comment to say "Recursive tree walk with bounded depth" or (b) convert to a truly iterative implementation using an explicit stack `Vec<(usize, Option<NodeKindId>, Option<NodeKindId>)>` and `cursor.goto_first_child()`/`cursor.goto_next_sibling()`/`cursor.goto_parent()` in a loop. Option (a) is minimal, option (b) eliminates the stack overflow risk entirely.

**`NodeKindVocabulary::get_or_insert` uses `debug_assert` for u16 overflow -- silent wrapping in release builds** - `crates/rskim-research/src/ast_types.rs:157`
**Confidence**: 92%
- Problem: The u16 overflow guard is `debug_assert!`, which is compiled out in release builds. If the vocabulary somehow exceeds 65,535 entries (acknowledged as unlikely per the comment, but not impossible with 14 languages across 40 repos), the `as NodeKindId` cast at line 162 silently wraps around to 0, causing vocabulary collisions and corrupt bigram/trigram data. Per the feature knowledge: "NodeKindId is u16 -- should have overflow guard (debug_assert was added in fix commit)" -- this acknowledges the concern but the fix is insufficient for release-mode safety. `applies ADR-001` -- this was noticed and should be fixed now rather than deferred.
- Fix: Replace `debug_assert!` with a proper `assert!` or return a `Result<NodeKindId, Error>`:
```rust
pub fn get_or_insert(&mut self, kind: &str) -> NodeKindId {
    if let Some(&id) = self.kind_to_id.get(kind) {
        return id;
    }
    assert!(
        self.id_to_kind.len() < usize::from(NodeKindId::MAX),
        "NodeKindVocabulary overflow: {} kinds exceeds u16::MAX",
        self.id_to_kind.len()
    );
    let id = self.id_to_kind.len() as NodeKindId;
    self.kind_to_id.insert(kind.to_string(), id);
    self.id_to_kind.push(kind.to_string());
    id
}
```

### MEDIUM

**`lang_total_nodes` accumulator can overflow u32 across large corpus** - `crates/rskim-research/src/ast_extract.rs:246`
**Confidence**: 82%
- Problem: `lang_total_nodes` is `u32` and accumulates `result.node_count` (also `u32`, bounded by `MAX_AST_NODES=100_000`) across all files for a language. With 40 repos, each potentially yielding thousands of files, the accumulator could exceed `u32::MAX` (~4.3 billion). For example, 50,000 files x 100,000 nodes = 5 billion, which overflows. In debug builds this panics; in release builds it silently wraps, producing incorrect corpus statistics.
- Fix: Use `u64` for corpus-level accumulators (`lang_total_nodes`, `lang_error_nodes`), or use `saturating_add`:
```rust
let mut lang_total_nodes: u64 = 0;
let mut lang_error_nodes: u64 = 0;
// ... later:
lang_error_nodes += u64::from(result.error_node_count);
lang_total_nodes += u64::from(result.node_count);
```
This also requires updating `AstLanguageStats` fields from `u32` to `u64`, or capping with `u32::try_from(lang_total_nodes).unwrap_or(u32::MAX)`.

**`total_files_seen` cast from `usize` to `u32` without bounds check** - `crates/rskim-research/src/ast_extract.rs:220`
**Confidence**: 80%
- Problem: `let total_files_seen: u32 = files.len() as u32;` truncates silently if `files.len()` exceeds `u32::MAX`. While unlikely in practice, this is used only for the progress bar (so the consequence is cosmetic), but the pattern violates the principle that casts should be explicit about their safety.
- Fix: Use `u32::try_from(files.len()).unwrap_or(u32::MAX)` or just use `u64` directly since `ProgressBar::new` accepts `u64`.

## Issues in Code You Touched (Should Fix)

### MEDIUM

**`lang_to_ident` underscore-collapse is fragile for edge-case inputs** - `crates/rskim-research/src/ast_codegen.rs:153`
**Confidence**: 80%
- Problem: The underscore-collapse logic uses `.split("__").collect::<Vec<_>>().join("_")` which only collapses double-underscores, not triple or more. Input like `"C++"` maps to `C__` via the char replacement, which becomes `C_` -- correct. But a hypothetical `"C+++"` would produce `C___` which splits into `["C", "_"]` and joins as `"C__"` -- still double. This is a single-pass collapse, not iterative. The function is only called with known language names from `ast-corpus.toml` (which are validated), so this is low-risk in practice, but the implementation does not match the doc claim of "consecutive underscores are collapsed".
- Fix: Use a regex or a `while` loop to collapse all consecutive underscores:
```rust
fn lang_to_ident(lang: &str) -> String {
    let raw: String = lang.chars()
        .map(|c| match c {
            '+' | '#' | '-' | ' ' => '_',
            _ => c.to_ascii_uppercase(),
        })
        .collect();
    // Collapse all consecutive underscores.
    let mut result = String::with_capacity(raw.len());
    let mut prev_underscore = false;
    for c in raw.chars() {
        if c == '_' {
            if !prev_underscore {
                result.push(c);
            }
            prev_underscore = true;
        } else {
            result.push(c);
            prev_underscore = false;
        }
    }
    result
}
```

## Pre-existing Issues (Not Blocking)

(none)

## Suggestions (Lower Confidence)

- **`walk_tree` early-return on `MAX_AST_NODES` does not restore cursor position** - `crates/rskim-research/src/ast_extract.rs:128` (Confidence: 65%) -- When the node count limit is hit mid-traversal, the function returns without ensuring the cursor is back at the root. Since the caller (`extract_ast_ngrams_from_file`) does not use the cursor after `walk_tree` returns, this is currently safe, but a future refactor could introduce a bug if the cursor is reused.

- **`stabilize()` clones the old_kinds vector unnecessarily** - `crates/rskim-research/src/ast_types.rs:212` (Confidence: 70%) -- `let mut sorted_kinds = old_kinds.clone();` allocates a second copy of all kind strings. Since `old_kinds` is only used for building the remap table (which just needs the original insertion order), the clone could be avoided by sorting indices rather than strings. This is an allocation discipline concern in a function called once per run.

## Summary
| Category | CRITICAL | HIGH | MEDIUM | LOW |
|----------|----------|------|--------|-----|
| Blocking | 0 | 2 | 2 | 0 |
| Should Fix | 0 | 0 | 1 | 0 |
| Pre-existing | 0 | 0 | 0 | 0 |

**Reliability Score**: 7/10
**Recommendation**: CHANGES_REQUESTED

The code demonstrates good reliability practices overall -- bounded iteration (`MAX_AST_DEPTH`, `MAX_AST_NODES`, `MAX_TRIGRAMS_PER_FILE`, `MAX_FILE_SIZE`), git subprocess timeouts (300s with SIGKILL), SHA-256 deduplication, and graceful error handling for parse failures. The two HIGH findings are the recursive `walk_tree` with misleading "iterative" documentation and the `debug_assert` overflow guard that does nothing in release builds. The `u32` accumulator overflow risk across large corpora is a genuine concern for a tool processing 40 repos with 14 languages. All issues are straightforward to fix.
