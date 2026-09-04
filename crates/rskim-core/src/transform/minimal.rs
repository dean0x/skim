//! Minimal mode transformation
//!
//! ARCHITECTURE: Strip non-doc comments at module/class level while keeping all code intact.
//! Preserves doc comments, comments inside function bodies, and shebangs.
//!
//! Token reduction target: 15-30%

use crate::transform::literals::{collect_literal_ranges, in_protected, map_ranges_to_output, merge_ranges};
use crate::transform::utils::is_function_scope_kind;
use crate::{Language, Result, SkimError, TransformConfig};
use tree_sitter::{Node, Tree};

/// Maximum AST recursion depth to prevent stack overflow attacks
pub(crate) const MAX_AST_DEPTH: usize = 500;

/// Maximum number of AST nodes to prevent memory exhaustion
pub(crate) const MAX_AST_NODES: usize = 100_000;

/// Per-file comment-classification tables, precomputed once before the walk.
///
/// Bundled into one `Copy` struct rather than passed as separate scalars so
/// `is_removable_comment` stays at 6 parameters — a 7th would sit on clippy's
/// `too_many_arguments` threshold. This follows the in-repo precedent of a
/// params struct over an `#[allow]` (see `cmd/heatmap/insights.rs`).
///
/// Both fields are *per-file* tables that replace per-node sibling walks; see
/// PF-020 for why a `TSNode` walk is never O(1).
#[derive(Clone, Copy, Default)]
pub(crate) struct CommentClassification<'a> {
    /// End byte of the last header comment, precomputed by `compute_header_end_byte`.
    /// 0 when there are no header comments or the language has no header-comment convention.
    pub(crate) header_end_byte: usize,
    /// Sorted start bytes of every Go doc comment, precomputed by
    /// `compute_go_doc_comment_starts`. Empty for every non-Go language.
    pub(crate) go_doc_comment_starts: &'a [usize],
}

/// Bundled parameters for the recursive comment walker to avoid parameter explosion
pub(crate) struct CommentWalkContext<'a> {
    pub(crate) ranges: &'a mut Vec<(usize, usize)>,
    pub(crate) node_count: &'a mut usize,
    pub(crate) classification: CommentClassification<'a>,
}

/// Transform source by stripping non-doc comments and normalizing blank lines
///
/// Two-pass algorithm:
/// 1. Walk AST collecting byte ranges of non-doc comment nodes to remove
///    (skip doc comments, skip comments inside function bodies, skip shebangs)
/// 2. Adjust ranges for full-line removal, then remove from source
/// 3. Trim trailing whitespace and normalize blank lines (3+ consecutive -> 2)
pub(crate) fn transform_minimal(
    source: &str,
    tree: &Tree,
    language: Language,
    _config: &TransformConfig,
) -> Result<String> {
    let root = tree.root_node();
    // Precompute the module-header boundary in a single O(N) forward pass.
    // This replaces the per-node O(N) backward walk that produced an overall O(N³).
    let header_end_byte = compute_header_end_byte(root, source, language);
    // Precompute Go doc-comment starts in a single O(N) TreeCursor pass. This
    // replaces the per-node forward sibling walk that was Θ(M³/3) for a run of
    // M contiguous comments in one sibling group.
    let go_doc_comment_starts = compute_go_doc_comment_starts(root, source, language);

    let mut ranges_to_remove: Vec<(usize, usize)> = Vec::new();
    let mut node_count: usize = 0;
    let mut ctx = CommentWalkContext {
        ranges: &mut ranges_to_remove,
        node_count: &mut node_count,
        classification: CommentClassification {
            header_end_byte,
            go_doc_comment_starts: &go_doc_comment_starts,
        },
    };
    collect_removable_comments(root, source, language, &mut ctx, 0, false)?;

    // Build the newline offset table once for the whole file: O(N).
    // Each adjust_range_for_line_removal call then resolves line boundaries in
    // O(log N) via binary search instead of O(start) via rfind, reducing the
    // total from O(N²) to O(N log N) across N ranges.
    let newlines = build_newline_table(source);

    // Adjust ranges for full-line removal, then merge to produce a sorted,
    // non-overlapping set.  merge_ranges handles overlaps that line-level
    // adjustment introduces (adjacent AST nodes that expand to the same line)
    // and exact duplicates — satisfying map_ranges_to_output's precondition.
    let final_ranges: Vec<(usize, usize)> = merge_ranges(
        ctx.ranges
            .iter()
            .map(|&(start, end)| adjust_range_for_line_removal(source, start, end, &newlines))
            .collect(),
    );

    // Collect literal-fragment ranges from the source tree before removal so
    // trim_and_normalize can skip trailing-space trimming inside literals.
    let literal_ranges = collect_literal_ranges(tree, language)?;
    let protected = map_ranges_to_output(&literal_ranges, &final_ranges);

    let after_removal = remove_ranges(source, &final_ranges)?;
    let normalized = trim_and_normalize(&after_removal, &protected);

    Ok(normalized)
}

/// Recursively collect byte ranges of comment nodes that should be removed
///
/// Collects raw (unadjusted) byte ranges. Line-level adjustment is applied
/// by the caller after collection, matching the pattern used by pseudo.rs.
///
/// # Security
/// - Enforces MAX_AST_DEPTH to prevent stack overflow
/// - Enforces MAX_AST_NODES to prevent memory exhaustion
pub(crate) fn collect_removable_comments(
    node: Node,
    source: &str,
    language: Language,
    ctx: &mut CommentWalkContext<'_>,
    depth: usize,
    in_function_body: bool,
) -> Result<()> {
    // SECURITY: Prevent stack overflow from deeply nested AST
    if depth > MAX_AST_DEPTH {
        return Err(SkimError::ParseError(format!(
            "Maximum AST depth exceeded: {} (possible malicious input)",
            MAX_AST_DEPTH
        )));
    }

    // AST node count over the cap: typically a legitimate but very large generated
    // file, not an attack. Signal a complexity limit so the dispatcher degrades to
    // a lossless raw passthrough instead of failing the command. (#317)
    *ctx.node_count += 1;
    if *ctx.node_count > MAX_AST_NODES {
        return Err(SkimError::ComplexityLimit {
            what: "AST nodes",
            count: *ctx.node_count,
            max: MAX_AST_NODES,
        });
    }

    if is_removable_comment(
        node,
        source,
        language,
        ctx.classification,
        depth,
        in_function_body,
    ) {
        ctx.ranges.push((node.start_byte(), node.end_byte()));
    }

    // Descendants are inside a function body if any strict ancestor (including
    // this node) is a body/function-definition node. O(1) per node — avoids
    // the O(depth) ts_node_parent() call that made the old is_inside_function_body
    // ancestor walk O(N²) across N root-level nodes.
    let child_in_body = in_function_body || is_function_scope_kind(node.kind(), language);

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_removable_comments(child, source, language, ctx, depth + 1, child_in_body)?;
    }

    Ok(())
}

/// Check if a comment node is a shebang line (e.g., `#!/usr/bin/env python3`)
///
/// Shebangs must start at byte 0 and begin with `#!`.
fn is_shebang(node: Node, source: &str) -> bool {
    if node.start_byte() != 0 {
        return false;
    }
    node.utf8_text(source.as_bytes())
        .map(|text| text.starts_with("#!"))
        .unwrap_or(false)
}

/// Check if a node kind represents a comment in the given language
pub(crate) fn is_comment_node(kind: &str, language: Language) -> bool {
    match language {
        Language::TypeScript
        | Language::JavaScript
        | Language::Python
        | Language::Go
        | Language::C
        | Language::Cpp
        | Language::CSharp
        | Language::Ruby
        | Language::Sql
        | Language::Bash => kind == "comment",
        Language::Rust | Language::Java | Language::Kotlin => {
            kind == "line_comment" || kind == "block_comment"
        }
        Language::Swift => kind == "comment" || kind == "multiline_comment",
        // Markdown, JSON, YAML, TOML don't have comment nodes to strip
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => false,
    }
}

/// Check if a comment node should be removed (not a doc comment, shebang, or in-body comment)
///
/// Combines `is_comment_node` with doc-comment filtering, shebang detection, and
/// function-body detection. Returns true if the node is a comment that should be stripped.
///
/// `classification` carries the per-file precomputed tables — the module-header
/// boundary from `compute_header_end_byte` and the sorted Go doc-comment start
/// bytes from `compute_go_doc_comment_starts`. `CommentClassification::default()`
/// disables both.
///
/// `depth` is the recursion depth from the root (root = 0, root children = 1, …).
/// `in_function_body` is a threaded flag — true when any strict ancestor of this
/// node is a function body or function definition node (maintained by the walker).
/// Both replace O(depth) `parent()` calls that were O(N²) across N root-level nodes.
///
/// Used by both minimal mode (via `collect_removable_comments`) and pseudo mode
/// (inlined into `collect_noise_ranges` for single-pass processing).
pub(crate) fn is_removable_comment(
    node: Node,
    source: &str,
    language: Language,
    classification: CommentClassification<'_>,
    depth: usize,
    in_function_body: bool,
) -> bool {
    if !is_comment_node(node.kind(), language) {
        return false;
    }
    let should_preserve = is_shebang(node, source)
        || in_function_body
        || is_doc_comment(node, source, language, classification.go_doc_comment_starts)
        || is_module_header_comment(node, language, classification.header_end_byte, depth);
    !should_preserve
}

/// Check if a comment node is a doc comment that should be preserved
///
/// Language-specific doc comment detection. See match arms below for
/// per-language rules covering all supported tree-sitter languages.
fn is_doc_comment(
    node: Node,
    source: &str,
    language: Language,
    go_doc_comment_starts: &[usize],
) -> bool {
    let text = match node.utf8_text(source.as_bytes()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    match language {
        Language::TypeScript | Language::JavaScript => {
            // JSDoc comments start with /**
            text.starts_with("/**")
        }
        Language::Python => {
            // Python docstrings are expression_statement > string nodes, NOT comment nodes.
            // All Python `comment` nodes (starting with #) at module level are regular comments.
            false
        }
        Language::Rust => {
            // Rust doc comments: ///, //!, /**, /*!
            text.starts_with("///")
                || text.starts_with("//!")
                || text.starts_with("/**")
                || text.starts_with("/*!")
        }
        Language::Go => {
            // Go doc comments are comments adjacent to a declaration. Resolved
            // once per file into a sorted start-byte table; O(log D) here.
            is_go_doc_comment(node, go_doc_comment_starts)
        }
        Language::Java => {
            // Javadoc comments start with /**
            text.starts_with("/**")
        }
        Language::C | Language::Cpp => {
            // Doxygen comments: /** or ///
            text.starts_with("/**") || text.starts_with("///")
        }
        Language::CSharp => {
            // C# XML doc comments: ///
            text.starts_with("///") || text.starts_with("/**")
        }
        Language::Ruby => {
            // Ruby doesn't have doc comments — all `#` comments are regular.
            // RDoc and YARD conventions use `#` but there's no syntactic distinction.
            false
        }
        Language::Kotlin => {
            // Kotlin doc comments: /** */ (KDoc)
            text.starts_with("/**")
        }
        Language::Swift => {
            // Swift doc comments: /// or /** */
            text.starts_with("///") || text.starts_with("/**")
        }
        Language::Sql => {
            // SQL `--` comments have no doc comment convention
            false
        }
        Language::Bash => {
            // Bash `#` comments have no doc comment convention
            false
        }
        // Markdown, JSON, YAML, TOML don't reach here
        Language::Markdown | Language::Json | Language::Yaml | Language::Toml => false,
    }
}

/// Check if a Go comment is a doc comment (adjacent to a declaration).
///
/// **O(log D) lookup** — the set of Go doc-comment start bytes is precomputed
/// once per file by `compute_go_doc_comment_starts` and passed in as a sorted
/// slice. Classification is a single binary search.
///
/// Replaces a per-node forward `next_named_sibling()` walk that was
/// **Θ(M³/3)** for a run of M contiguous comments in one sibling group: a
/// `TSNode` has no parent pointer, so every `next_named_sibling()` re-derives
/// the parent by descending from the tree root (PF-020). Measured on this
/// branch (DEBUG build, Go leading-comment run before `package main`):
/// N=250 → 665 ms, N=500 → 5231 ms, N=1000 → 41616 ms (α = 2.98).
fn is_go_doc_comment(node: Node, go_doc_comment_starts: &[usize]) -> bool {
    go_doc_comment_starts
        .binary_search(&node.start_byte())
        .is_ok()
}

/// Does the byte gap between `current_end` and `sib_start` contain a blank line?
///
/// Replicates the guard from the original per-node walk **exactly**: when
/// `current_end <= sib_start && sib_start <= source.len()` does not hold, the
/// blank-line test is *skipped* (treated as "no blank line") rather than
/// terminating the run. `source.get()` returning `None` — only reachable if a
/// bound is not a UTF-8 char boundary — is likewise treated as "no blank line";
/// the original indexed `&source[..]` directly and would have panicked there.
///
/// Counting `b'\n'` over bytes is equivalent to counting `'\n'` over chars:
/// 0x0A never occurs inside a multi-byte UTF-8 sequence.
fn go_gap_breaks_run(source: &str, current_end: usize, sib_start: usize) -> bool {
    if current_end <= sib_start
        && sib_start <= source.len()
        && let Some(between) = source.get(current_end..sib_start)
    {
        return between.bytes().filter(|&b| b == b'\n').count() > 1;
    }
    false
}

/// Precompute the start bytes of every Go doc comment in the file.
///
/// Returns a **sorted** `Vec<usize>` of `start_byte()` values, produced by a
/// single `TreeCursor` pass. `is_go_doc_comment` then answers in O(log D) via
/// `binary_search`.
///
/// A sorted `Vec` rather than a `HashSet`: at realistic D, ~log₂(D) integer
/// comparisons beat one SipHash, and it avoids a hash table allocation
/// (minimise-allocation-after-initialisation). Sortedness comes free from
/// pre-order emission and is locked by a `debug_assert!` at the end.
///
/// **Every sibling group is covered, not just the root's children.**
/// `is_function_scope_kind` maps Go to `["block"]`, so comments inside
/// `field_declaration_list`, `interface_type`, and grouped `type (…)` /
/// `const (…)` / `var (…)` / import blocks are *not* treated as in-body and do
/// reach this predicate. In particular a comment inside a grouped `type (…)`
/// precedes a `type_spec` — an `is_go_declaration` kind — so it is a KEEP that a
/// root-children-only precompute would silently misclassify as STRIP.
///
/// **Complexity:** O(N) — each named node is visited exactly once. `TreeCursor`
/// is the only genuinely O(1)-per-step traversal API in tree-sitter (PF-020).
///
/// Non-Go languages return `Vec::new()`, which does not allocate.
pub(crate) fn compute_go_doc_comment_starts(
    root: Node,
    source: &str,
    language: Language,
) -> Vec<usize> {
    if language != Language::Go {
        return Vec::new();
    }
    let mut starts = Vec::new();
    let mut run_starts = Vec::new();
    scan_go_sibling_group(root, source, &mut starts, &mut run_starts, 0);
    debug_assert!(
        starts.windows(2).all(|w| w[0] < w[1]),
        "compute_go_doc_comment_starts must emit strictly ascending start bytes \
         (binary_search depends on it); got {starts:?}"
    );
    starts
}

/// Advance `cursor` to the next *named* sibling at the current level.
///
/// Anonymous siblings are invisible to `next_named_sibling()` and must never
/// terminate a comment run or update the run's `current_end`; skipping them here
/// reproduces that exactly.
fn goto_next_named_sibling(cursor: &mut tree_sitter::TreeCursor<'_>) -> bool {
    while cursor.goto_next_sibling() {
        if cursor.node().is_named() {
            return true;
        }
    }
    false
}

/// Walk one sibling group in pre-order, recording Go doc-comment start bytes.
///
/// Comment nodes are leaves, so a whole comment run can be resolved in place
/// before descending into the node that terminates it — which keeps emission in
/// ascending byte order across the entire file.
fn scan_go_sibling_group(
    parent: Node,
    source: &str,
    starts: &mut Vec<usize>,
    run_starts: &mut Vec<usize>,
    depth: usize,
) {
    // Mirrors the walkers' MAX_AST_DEPTH bound. A tree deeper than this makes
    // `collect_removable_comments` / `collect_noise_ranges` return a ParseError
    // before any of these classifications can be observed.
    if depth > MAX_AST_DEPTH {
        return;
    }

    let mut cursor = parent.walk();
    if !cursor.goto_first_child() {
        return;
    }
    if !cursor.node().is_named() && !goto_next_named_sibling(&mut cursor) {
        return;
    }

    loop {
        if !is_comment_node(cursor.node().kind(), Language::Go) {
            let node = cursor.node();
            scan_go_sibling_group(node, source, starts, run_starts, depth + 1);
            if !goto_next_named_sibling(&mut cursor) {
                return;
            }
            continue;
        }

        // Resolve the whole contiguous comment run in one forward pass.
        //
        // For a run c₀..c₍ₘ₋₁₎ terminated by named non-comment X, the original
        // walk starting at cᵢ returned `is_go_declaration(X)` unless some gap in
        // gᵢ..g₍ₘ₋₁₎ contained a blank line. Since a break disqualifies every
        // earlier comment too, the doc comments form a contiguous SUFFIX of the
        // run, starting just past the LAST break.
        run_starts.clear();
        run_starts.push(cursor.node().start_byte());
        let mut prev_end = cursor.node().end_byte();
        let mut last_break: Option<usize> = None;
        let mut terminator_is_declaration = false;
        let mut have_sibling = true;

        loop {
            if !goto_next_named_sibling(&mut cursor) {
                // Run ends at the end of the sibling list → every cᵢ is false.
                have_sibling = false;
                break;
            }
            let sib = cursor.node();
            if go_gap_breaks_run(source, prev_end, sib.start_byte()) {
                last_break = Some(run_starts.len() - 1);
            }
            if is_comment_node(sib.kind(), Language::Go) {
                run_starts.push(sib.start_byte());
                prev_end = sib.end_byte();
                continue;
            }
            terminator_is_declaration = is_go_declaration(sib.kind());
            break;
        }

        if terminator_is_declaration {
            let first_doc = last_break.map_or(0, |i| i + 1);
            starts.extend_from_slice(&run_starts[first_doc..]);
        }

        if !have_sibling {
            return;
        }
        // The cursor now sits on the terminator; descend into it, then continue.
        let terminator = cursor.node();
        scan_go_sibling_group(terminator, source, starts, run_starts, depth + 1);
        if !goto_next_named_sibling(&mut cursor) {
            return;
        }
    }
}

/// Check if a Go node kind is a declaration type
fn is_go_declaration(kind: &str) -> bool {
    matches!(
        kind,
        "function_declaration"
            | "method_declaration"
            | "type_declaration"
            | "var_declaration"
            | "const_declaration"
            | "type_spec"
    )
}

/// Compute the end byte of the module-level header comment block in a single O(N) forward pass.
///
/// Returns the `end_byte()` of the last comment node that belongs to the
/// contiguous leading comment run at the top of the file, or `0` if there are
/// no header comments.
///
/// A header comment is a root-level named child that:
/// 1. Belongs to a language with a header-comment convention (Python, Ruby, SQL, Bash).
/// 2. Is part of a prefix run of comment nodes with no blank-line break
///    (more than one `\n` in the byte gap between consecutive named children).
///
/// **Complexity:** O(N) — each root-level named child is visited exactly once via a
/// `TreeCursor`, which is the only genuinely O(1)-per-step traversal in tree-sitter.
/// A `TSNode` carries no parent pointer, so `named_child(i)` (O(i) rescan from root)
/// and `next_named_sibling()` (also O(i) — calls `ts_node_parent` which re-walks down
/// from the root) are BOTH O(i) per step and would make the loop O(N²). Profiling at
/// N=8000 confirmed 8003 ms (alpha ≈ 2.0). A `TreeCursor` keeps an explicit ancestor
/// stack, so `goto_next_sibling` is true O(1). Empirically: N=8000 → 30 ms after fix.
///
/// **Equivalence to the old backward walk:** the forward pass produces an identical
/// header boundary because:
/// - The old walk returned `true` iff every preceding named sibling was a comment
///   with no blank-line gap in between.
/// - The forward pass stops at the first non-comment or blank-line gap and records
///   the end of the last accepted comment — exactly the same boundary.
pub(crate) fn compute_header_end_byte(root: Node, source: &str, language: Language) -> usize {
    match language {
        Language::Python | Language::Ruby | Language::Sql | Language::Bash => {}
        _ => return 0,
    }

    let mut header_end: usize = 0;
    let mut prev_end: usize = 0;

    // TreeCursor is the ONLY genuinely O(1)-per-step traversal API in tree-sitter.
    // A TSNode carries no parent pointer, so `ts_node_named_child(root, i)` (O(i)
    // rescan from position 0) and `ts_node_next_named_sibling()` (also O(i) — its
    // first action is ts_node_parent(), which re-walks down from the root) are BOTH
    // O(i) per step and make this loop O(N²). A TreeCursor keeps an explicit ancestor
    // stack, so goto_next_sibling is true O(1).
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        // A gap with more than one newline between the previous node end and this
        // node start means at least one blank line exists — the header block ends.
        // `prev_end > 0` is equivalent to `i > 0` in the old loop: prev_end stays
        // zero until the first comment is accepted, so the gap check is skipped for
        // the very first child (no predecessor to form a gap against).
        if prev_end > 0 {
            let gap_start = prev_end;
            let gap_end = child.start_byte();
            if gap_start < gap_end
                && let Some(gap) = source.get(gap_start..gap_end)
                && gap.bytes().filter(|&b| b == b'\n').count() > 1
            {
                break;
            }
        }

        // Extend the header only for comment nodes; any other node terminates it.
        if is_comment_node(child.kind(), language) {
            header_end = child.end_byte();
            prev_end = child.end_byte();
        } else {
            break;
        }
    }

    header_end
}

/// Build a sorted list of byte positions where `\n` appears in `source`.
///
/// Used by `adjust_range_for_line_removal` to resolve line boundaries in O(log N)
/// per call via binary search, instead of O(start) per call via `rfind`.
///
/// Build cost: O(N) — one pass through every byte.  Binary-search cost per
/// `adjust_range_for_line_removal` call: O(log N).  Total for M ranges:
/// O(N + M log N) instead of O(N · M) when M is proportional to N.
pub(crate) fn build_newline_table(source: &str) -> Vec<usize> {
    source
        .bytes()
        .enumerate()
        .filter_map(|(i, b)| if b == b'\n' { Some(i) } else { None })
        .collect()
}

/// Check if a comment is part of the module-level header comment block.
///
/// **O(1) lookup** — the module-header boundary is precomputed once per file by
/// `compute_header_end_byte` and passed in as `header_end_byte`. Classification
/// is a single integer comparison.
///
/// A module header is a contiguous run of comment nodes at the very top of the
/// file (direct children of the root node) with no blank-line break between them
/// and no preceding non-comment sibling.
///
/// Languages where this applies: Python, Ruby, SQL, and Bash — all use `#` or
/// `--` comments at module level for shebangs, copyright, SPDX,
/// `frozen_string_literal: true`, provenance markers, and FIXTURE/TESTS headers.
/// No doc-comment convention exists for these languages (`is_doc_comment` returns
/// `false`), so without this guard minimal/pseudo would strip them.
///
/// Pass `header_end_byte = 0` to disable (no comments classified as headers).
fn is_module_header_comment(
    node: Node,
    language: Language,
    header_end_byte: usize,
    depth: usize,
) -> bool {
    match language {
        Language::Python | Language::Ruby | Language::Sql | Language::Bash => {}
        _ => return false,
    }
    // Must be a direct child of the root node. Root is walked at depth 0, so its
    // direct children are depth 1. O(1) integer compare — no parent() call needed.
    // A TSNode has no parent pointer; every parent() call re-walks the tree from
    // the root, costing O(depth). Use the threaded depth instead.
    if depth != 1 {
        return false;
    }
    // O(1) integer comparison against the precomputed boundary.
    node.end_byte() <= header_end_byte
}

/// Adjust a range to remove the entire line if the range is the only
/// non-whitespace content on that line.
///
/// If the range occupies the full line (only whitespace before/after on same line),
/// remove the entire line including the newline. Otherwise, just remove the range
/// and any leading whitespace before it on the same line (for inline trailing content).
///
/// `newlines` is the precomputed newline-position table from `build_newline_table`.
/// Passing it eliminates the O(start) `rfind('\n')` scan that made the old
/// implementation O(N²) across N ranges — each call now resolves line boundaries
/// in O(log N) via binary search.
///
/// Used by both minimal mode (comment removal) and pseudo mode (noise removal).
pub(crate) fn adjust_range_for_line_removal(
    source: &str,
    start: usize,
    end: usize,
    newlines: &[usize],
) -> (usize, usize) {
    // Find the start of the line containing this range.
    // Binary-search for the last newline whose position is < start.
    // O(log N) vs O(start) for the old rfind scan.
    let line_start = {
        let i = newlines.partition_point(|&pos| pos < start);
        if i == 0 { 0 } else { newlines[i - 1] + 1 }
    };

    // Find the end of the line containing this range.
    // Binary-search for the first newline whose position is >= end.
    let line_end = {
        let i = newlines.partition_point(|&pos| pos < end);
        if i < newlines.len() {
            newlines[i] + 1
        } else {
            source.len()
        }
    };

    // Check if the range is the only non-whitespace content on the line
    let before_range = &source[line_start..start];
    let after_range = if end < line_end {
        let after_end = if line_end > 0 && source.as_bytes().get(line_end - 1) == Some(&b'\n') {
            line_end - 1
        } else {
            line_end
        };
        &source[end..after_end]
    } else {
        ""
    };

    let only_whitespace_before = before_range.chars().all(|c| c.is_whitespace());
    let only_whitespace_after = after_range.chars().all(|c| c.is_whitespace());

    if only_whitespace_before && only_whitespace_after {
        // Range is the only content on this line - remove the entire line
        (line_start, line_end)
    } else if only_whitespace_after {
        // Inline trailing range: remove leading whitespace before the range too
        let trimmed_start = source[line_start..start].trim_end().len() + line_start;
        (trimmed_start, end)
    } else {
        // Range is in the middle or start of a line with other content - just remove the range
        (start, end)
    }
}

/// Remove collected byte ranges from source
///
/// Builds a new string by copying everything except the removed ranges.
pub(crate) fn remove_ranges(source: &str, ranges: &[(usize, usize)]) -> Result<String> {
    if ranges.is_empty() {
        return Ok(source.to_string());
    }

    let mut result = String::with_capacity(source.len());
    let mut last_pos = 0;

    for &(start, end) in ranges {
        if end < start {
            return Err(SkimError::ParseError(format!(
                "Invalid range: start={} end={}",
                start, end
            )));
        }
        if end > source.len() {
            return Err(SkimError::ParseError(format!(
                "Range exceeds source length: end={} len={}",
                end,
                source.len()
            )));
        }

        // Callers must pass sorted, non-overlapping ranges (established by merge_ranges).
        debug_assert!(
            start >= last_pos,
            "overlapping ranges are impossible after merge_ranges; start={start} last_pos={last_pos}"
        );

        if !source.is_char_boundary(start) || !source.is_char_boundary(end) {
            return Err(SkimError::ParseError(format!(
                "Invalid UTF-8 boundary at range [{}, {})",
                start, end
            )));
        }

        result.push_str(&source[last_pos..start]);
        last_pos = end;
    }

    if !source.is_char_boundary(last_pos) {
        return Err(SkimError::ParseError(format!(
            "Invalid UTF-8 boundary at position {}",
            last_pos
        )));
    }

    result.push_str(&source[last_pos..]);

    Ok(result)
}

/// Trim trailing whitespace and normalize blank lines in a single pass.
///
/// Combines two operations to avoid an extra allocation:
/// 1. Trims trailing whitespace from each line (unprotected bytes only)
/// 2. Normalizes blank lines: 3+ consecutive blank lines become 2
///
/// **Literal-aware:** byte ranges in `protected` are never trimmed, and a line
/// is only considered blank when the backward-scan reaches the line start —
/// meaning every byte was unprotected whitespace.  A line whose trailing bytes
/// are protected (e.g. a string literal with trailing spaces) is preserved
/// verbatim.
///
/// Index-based iteration (not `.lines()`) so that byte offsets remain
/// correlatable with `protected` range coordinates.  CRLF line endings are
/// normalised to LF.
pub(crate) fn trim_and_normalize(source: &str, protected: &[(usize, usize)]) -> String {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut result = String::with_capacity(source.len());
    let mut consecutive_blanks: usize = 0;
    let mut pos = 0usize;

    while pos < n {
        let line_start = pos;

        // Locate the newline that terminates this line.
        let nl = bytes[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map_or(n, |i| pos + i);
        // CRLF: exclude the `\r` from trimmed content.
        let has_cr = nl > line_start && bytes[nl - 1] == b'\r';
        let content_end = if has_cr { nl - 1 } else { nl };
        pos = if nl < n { nl + 1 } else { n };

        // Compute trim_end: scan backward, skip trailing unprotected whitespace.
        let mut trim_end = content_end;
        while trim_end > line_start {
            let b = bytes[trim_end - 1];
            if (b == b' ' || b == b'\t') && !in_protected(trim_end - 1, protected) {
                trim_end -= 1;
            } else {
                break;
            }
        }

        // A line is blank when every byte was unprotected whitespace.
        // Exception: a truly-empty line (trim_end == line_start, no content bytes)
        // whose START position falls inside a protected range is a blank line that
        // lives inside a multi-line string literal and must NOT be capped.
        // (`trim_end == line_start` implies no protected content bytes; but the
        // position itself may be inside a protected range — e.g. the `\n` of a
        // blank line that is part of a Python """…""" body.)
        let is_blank = trim_end == line_start && !in_protected(line_start, protected);

        if is_blank {
            consecutive_blanks += 1;
            if consecutive_blanks > 2 {
                continue;
            }
        } else {
            consecutive_blanks = 0;
        }

        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&source[line_start..trim_end]);
    }

    if source.ends_with('\n') {
        result.push('\n');
    }

    result
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // acceptable in tests
mod tests {
    use super::*;

    // ========================================================================
    // compute_header_end_byte and is_module_header_comment unit tests
    // ========================================================================
    //
    // These tests exercise the forward-pass precomputation and the O(1) predicate.
    // All cases use Python (simplest grammar for comment positioning); the
    // dispatch rules are identical for Python, Ruby, SQL, and Bash.

    // Helper: parse Python source into a tree-sitter Tree.
    fn parse_python(source: &str) -> Tree {
        let mut parser = crate::Parser::new(Language::Python).unwrap();
        parser.parse(source).unwrap()
    }

    // Helper: find the Nth root-level comment node (0-indexed) in a parsed tree.
    //
    // Uses a TreeCursor, not `named_child(i)` in a loop: `ts_node_named_child`
    // re-scans the child list from position 0 on every call, so the indexed form
    // is O(i) per step and O(N²) overall — the exact pattern the module docblock
    // above forbids (PF-020). Test-only code is not exempt from being a worked
    // example.
    fn nth_root_comment<'a>(tree: &'a Tree, n: usize) -> Node<'a> {
        let root = tree.root_node();
        let mut cursor = root.walk();
        let mut found = 0usize;
        for child in root.named_children(&mut cursor) {
            if child.kind() == "comment" {
                if found == n {
                    return child;
                }
                found += 1;
            }
        }
        panic!("could not find comment #{n} in root named children");
    }

    // ── compute_header_end_byte correctness ─────────────────────────────────

    #[test]
    fn test_compute_header_end_byte_no_leading_comments() {
        // Code node first → header_end = 0 (no header).
        let source = "x = 1\n# trailing comment\n";
        let tree = parse_python(source);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(heb, 0, "code-first file must have header_end_byte = 0");
    }

    #[test]
    fn test_compute_header_end_byte_single_comment() {
        // Single leading comment → header_end = end_byte of that comment.
        let source = "# Copyright 2024 Acme Corp.\nx = 1\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            comment.end_byte(),
            "single leading comment must produce header_end_byte = its end_byte"
        );
    }

    #[test]
    fn test_compute_header_end_byte_contiguous_block() {
        // Three contiguous comments → header_end = end_byte of the third.
        let source =
            "#!/usr/bin/env python3\n# SPDX-License-Identifier: MIT\n# Copyright 2024\nx = 1\n";
        let tree = parse_python(source);
        let third = nth_root_comment(&tree, 2);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            third.end_byte(),
            "three contiguous comments → header_end_byte = end of last"
        );
    }

    #[test]
    fn test_compute_header_end_byte_blank_line_stops_block() {
        // Blank line between first and second comment → only first is the header.
        let source = "# Copyright 2024\n\n# helper used by the CLI\nx = 1\n";
        let tree = parse_python(source);
        let first = nth_root_comment(&tree, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            first.end_byte(),
            "blank-line break must stop header at the first comment"
        );
    }

    #[test]
    fn test_compute_header_end_byte_all_comments_file() {
        // File consisting entirely of comments → all are in the header.
        let source = "# Line 1\n# Line 2\n# Line 3\n";
        let tree = parse_python(source);
        let last = nth_root_comment(&tree, 2);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            last.end_byte(),
            "100%% comment file: all comments are the header, end_byte = last"
        );
    }

    #[test]
    fn test_compute_header_end_byte_cjk_in_comment() {
        // CJK multibyte content in a leading comment — end_byte is a byte position,
        // not a char index. The comparison must not panic on non-char boundaries.
        let source = "# 版权所有 2024 公司名\nx = 1\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        assert_eq!(
            heb,
            comment.end_byte(),
            "CJK comment must produce header_end_byte = its end_byte without panicking"
        );
        // Confirm end_byte is a valid UTF-8 char boundary (source.get must succeed).
        assert!(
            source.is_char_boundary(heb),
            "end_byte {heb} must be a valid UTF-8 char boundary in {source:?}"
        );
    }

    #[test]
    fn test_compute_header_end_byte_non_header_language_returns_zero() {
        // TypeScript is not in the header-language set → always returns 0.
        let ts_source = "// comment\nconst x = 1;\n";
        let mut parser = crate::Parser::new(Language::TypeScript).unwrap();
        let tree = parser.parse(ts_source).unwrap();
        let heb = compute_header_end_byte(tree.root_node(), ts_source, Language::TypeScript);
        assert_eq!(heb, 0, "non-header language must return 0");
    }

    // ── is_module_header_comment O(1) predicate (via compute_header_end_byte) ─

    #[test]
    fn test_is_module_header_comment_at_byte_0() {
        // A single comment at byte 0 with no preceding siblings → header.
        let source = "# Copyright 2024 Acme Corp.\nx = 1\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, 0);
        assert_eq!(
            comment.start_byte(),
            0,
            "test fixture: comment must be at byte 0"
        );
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1 in the walker.
        assert!(
            is_module_header_comment(comment, Language::Python, heb, 1),
            "comment at byte 0 with no preceding siblings must be a module header"
        );
    }

    #[test]
    fn test_is_module_header_comment_after_shebang() {
        // A comment immediately following the shebang (no blank line) → header.
        let source = "#!/usr/bin/env python3\n# SPDX-License-Identifier: MIT\nx = 1\n";
        let tree = parse_python(source);
        let spdx = nth_root_comment(&tree, 1); // index 0 is shebang, 1 is SPDX
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1 in the walker.
        assert!(
            is_module_header_comment(spdx, Language::Python, heb, 1),
            "comment contiguous with shebang must be identified as a module header"
        );
    }

    #[test]
    fn test_is_module_header_comment_after_blank_line_break_is_stripped() {
        // A comment separated from the preceding comment by a blank line → NOT a header.
        let source = "# Copyright 2024\n\n# helper used by the CLI\nx = 1\n";
        let tree = parse_python(source);
        let non_header = nth_root_comment(&tree, 1);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1; the byte comparison gates this (not depth).
        assert!(
            !is_module_header_comment(non_header, Language::Python, heb, 1),
            "comment after a blank-line break must NOT be a module header (should be stripped)"
        );
    }

    #[test]
    fn test_is_module_header_comment_after_code_is_stripped() {
        // A comment that has a code node as its preceding named sibling → NOT a header.
        let source = "x = 1\n# standalone comment after code\n";
        let tree = parse_python(source);
        let comment = nth_root_comment(&tree, 0);
        let heb = compute_header_end_byte(tree.root_node(), source, Language::Python);
        // Root children are at depth 1; heb=0 means no header (byte comparison fails).
        assert!(
            !is_module_header_comment(comment, Language::Python, heb, 1),
            "comment following a code statement must NOT be a module header"
        );
    }

    #[test]
    fn test_is_module_header_comment_inline_in_body_is_unchanged() {
        // A comment inside a function body is at depth > 1 → returns false immediately
        // (depth != 1 guard fires before the byte comparison).
        let source = "def f():\n    # body comment — never a header\n    pass\n";
        let tree = parse_python(source);
        let root = tree.root_node();
        // Walk into the function body to find the comment node.
        let func = root.named_child(0).expect("function_definition expected");
        let body_comment = {
            let mut found = None;
            'outer: for i in 0..func.named_child_count() {
                let child = func.named_child(i).unwrap();
                for j in 0..child.named_child_count() {
                    let grandchild = child.named_child(j).unwrap();
                    if grandchild.kind() == "comment" {
                        found = Some(grandchild);
                        break 'outer;
                    }
                }
                if child.kind() == "comment" {
                    found = Some(child);
                    break;
                }
            }
            found.expect("expected a comment node inside the function body")
        };
        // depth=3 (root→function_definition→block/function_body→comment).
        // Any value != 1 makes the depth guard fire. Use a large heb to confirm
        // the depth guard is the real gate, not the byte comparison.
        let heb = usize::MAX;
        assert!(
            !is_module_header_comment(body_comment, Language::Python, heb, 3),
            "inline body comment must NOT be a module header (depth != 1 guard fires)"
        );
    }

    // ── Performance regression guard ─────────────────────────────────────────

    #[test]
    fn test_large_header_block_linear_time() {
        // CUBIC SMOKE TEST for the O(N³) defect in the old backward-walk
        // implementation of is_module_header_comment.
        //
        // WHAT THIS TEST PROVES: that N=500 comments complete within 200 ms.
        // The old O(N³) code took ~3 s at N=500 in a DEBUG build — well beyond
        // 200 ms. The fixed O(N) code completes in < 10 ms.
        //
        // WHAT THIS TEST DOES NOT PROVE: linear vs quadratic scaling. An O(N²)
        // regression at N=500 would complete in ~31 ms and pass this test. For
        // the doubling-ratio guard that discriminates O(N) from O(N²), see
        // test_quadratic_scaling_guard.
        //
        // Empirical measurement (fix/init-pin-wrappers-header-comments, DEBUG build):
        //   O(N³) unfixed: N=200 → 213ms, N=400 → 1736ms, N=1000 → 23920ms
        //   O(N)  fixed:   N=500 → < 10ms
        //
        // Budget tightened from 2000ms to 200ms: the O(N³) code reliably exceeds
        // 200ms (extrapolated ~3 s at N=500), while the fixed code runs in < 10ms,
        // leaving ~20× CI headroom. 2000ms gave a 222× margin and asserted almost
        // nothing about scaling.
        let n = 500usize;
        let mut source = String::with_capacity(n * 25);
        for i in 0..n {
            source.push_str(&format!("# Header comment {i}\n"));
        }
        source.push_str("x = 1\n");

        let start = std::time::Instant::now();
        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let tree = parser.parse(&source).unwrap();
        let config = TransformConfig::default();
        let result = transform_minimal(&source, &tree, Language::Python, &config);
        let elapsed = start.elapsed();

        assert!(result.is_ok(), "transform must succeed: {:?}", result.err());
        let output = result.unwrap();

        // All 500 header comments must be preserved (they form a contiguous leading block).
        assert!(
            output.contains("# Header comment 0"),
            "first header comment must be preserved; got:\n{output}"
        );
        assert!(
            output.contains(&format!("# Header comment {}", n - 1)),
            "last header comment must be preserved; got:\n{output}"
        );

        assert!(
            elapsed < std::time::Duration::from_millis(200),
            "{n} leading comments must process in < 200ms (got {elapsed:?}); \
             old O(N³) code took ~3 s at N={n} in DEBUG — this indicates a cubic regression. \
             For quadratic regressions, see test_quadratic_scaling_guard."
        );
    }

    // ── Additional correctness: full-transform round-trips ───────────────────

    #[test]
    fn test_all_comments_file_preserved_minimal() {
        // A file that is 100% comments with no code: all are in the header block
        // and must all be preserved by minimal mode.
        let source = "# Line one\n# Line two\n# Line three\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        assert!(
            result.contains("# Line one"),
            "first comment must be preserved in all-comment file; got:\n{result}"
        );
        assert!(
            result.contains("# Line three"),
            "last comment must be preserved in all-comment file; got:\n{result}"
        );
    }

    #[test]
    fn test_comment_after_blank_line_gap_is_stripped_minimal() {
        // Header ends at blank line; the comment after the gap must be stripped.
        let source = "# Header\n\n# Not a header\nx = 1\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        assert!(
            result.contains("# Header"),
            "header comment must be preserved; got:\n{result}"
        );
        assert!(
            !result.contains("# Not a header"),
            "post-gap comment must be stripped; got:\n{result}"
        );
    }

    #[test]
    fn test_non_root_comment_not_treated_as_header_minimal() {
        // A comment inside a function body must be preserved by the body-comment
        // rule (in_function_body flag), not by the header rule; and the depth != 1
        // guard must prevent it from being classified as a header.
        let source = "def f():\n    # body comment\n    pass\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        // Body comments are preserved by the in_function_body threaded flag, not stripped.
        assert!(
            result.contains("# body comment"),
            "in-body comment must be preserved by body-comment rule; got:\n{result}"
        );
    }

    #[test]
    fn test_cjk_header_comment_not_stripped_minimal() {
        // CJK multibyte content in a module header comment — must be preserved
        // and source.get must not panic on the end_byte boundary.
        let source = "# 版权所有 2024\n# SPDX-License-Identifier: MIT\nx = 1\n";
        let tree = parse_python(source);
        let config = TransformConfig::default();
        let result = transform_minimal(source, &tree, Language::Python, &config).unwrap();
        assert!(
            result.contains("# 版权所有 2024"),
            "CJK header comment must be preserved; got:\n{result}"
        );
        assert!(
            result.contains("# SPDX-License-Identifier: MIT"),
            "second header comment (after CJK) must be preserved; got:\n{result}"
        );
    }

    #[test]
    fn test_trim_and_normalize_preserves_two_blanks() {
        let input = "a\n\n\nb\n";
        let result = trim_and_normalize(input, &[]);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_reduces_four_blanks_to_two() {
        let input = "a\n\n\n\n\nb\n";
        let result = trim_and_normalize(input, &[]);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_no_change_needed() {
        let input = "a\n\nb\n";
        let result = trim_and_normalize(input, &[]);
        assert_eq!(result, "a\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_trims_trailing_whitespace() {
        let input = "hello   \nworld  \n";
        let result = trim_and_normalize(input, &[]);
        assert_eq!(result, "hello\nworld\n");
    }

    #[test]
    fn test_trim_and_normalize_combined() {
        // Verify both trimming and normalization happen in one pass
        let input = "hello   \n\n\n\n\nworld  \n";
        let result = trim_and_normalize(input, &[]);
        assert_eq!(result, "hello\n\n\nworld\n");
    }

    #[test]
    fn test_adjust_range_full_line_comment() {
        let source = "code\n// comment\nmore code\n";
        let newlines = build_newline_table(source);
        // "// comment" starts at byte 5, ends at byte 15
        let (start, end) = adjust_range_for_line_removal(source, 5, 15, &newlines);
        // Should remove the entire line including newline
        assert_eq!(start, 5);
        assert_eq!(end, 16); // includes the newline
    }

    // ========================================================================
    // Issue 5: adjust_range_for_line_removal trailing/inline comment branches
    // ========================================================================

    #[test]
    fn test_adjust_range_trailing_comment() {
        let source = "let x = 1; // trailing\nmore code\n";
        let newlines = build_newline_table(source);
        // "// trailing" starts at byte 11, ends at byte 22
        let (start, end) = adjust_range_for_line_removal(source, 11, 22, &newlines);
        // Should remove " // trailing" (the trailing whitespace + comment) but keep "let x = 1;"
        // The function trims whitespace before the comment on the same line
        assert!(start <= 11, "start should be at or before comment start");
        assert_eq!(end, 22);
        // Verify the remaining text makes sense
        let remaining = format!("{}{}", &source[..start], &source[end..]);
        assert!(
            remaining.starts_with("let x = 1;"),
            "should preserve code before trailing comment, got: {:?}",
            remaining
        );
    }

    #[test]
    fn test_adjust_range_inline_comment_with_code_after() {
        // Comment at start of line with code after it -- the "middle" branch
        let source = "/* comment */ let x = 1;\n";
        let newlines = build_newline_table(source);
        // "/* comment */" starts at byte 0, ends at byte 13
        let (start, end) = adjust_range_for_line_removal(source, 0, 13, &newlines);
        // There is non-whitespace after the comment, so just remove the comment itself
        assert_eq!(start, 0);
        assert_eq!(end, 13);
    }

    // ========================================================================
    // Issue 4: remove_ranges error-path tests
    // ========================================================================

    #[test]
    fn test_remove_ranges_end_before_start() {
        let source = "hello world";
        let ranges = vec![(5, 3)]; // end < start
        let result = remove_ranges(source, &ranges);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid range"),
            "Expected 'Invalid range' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_ranges_end_exceeds_source_length() {
        let source = "hello";
        let ranges = vec![(0, 100)]; // end > source.len()
        let result = remove_ranges(source, &ranges);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Range exceeds source length"),
            "Expected 'Range exceeds source length' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_ranges_non_char_boundary() {
        // Multi-byte UTF-8 character: the euro sign takes 3 bytes
        let source = "a\u{20AC}b"; // "a" + euro sign (3 bytes) + "b" = 5 bytes total
        // Byte 2 is in the middle of the euro sign (bytes 1..4)
        let ranges = vec![(2, 4)];
        let result = remove_ranges(source, &ranges);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Invalid UTF-8 boundary"),
            "Expected 'Invalid UTF-8 boundary' error, got: {}",
            err_msg
        );
    }

    #[test]
    fn test_remove_ranges_empty_ranges() {
        let source = "hello world";
        let ranges = vec![];
        let result = remove_ranges(source, &ranges).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_remove_ranges_valid_removal() {
        let source = "hello beautiful world";
        let ranges = vec![(5, 15)]; // remove " beautiful"
        let result = remove_ranges(source, &ranges).unwrap();
        assert_eq!(result, "hello world");
    }

    // ========================================================================
    // Issue 3: Security limit error-path tests
    // ========================================================================

    #[test]
    fn test_max_ast_nodes_limit() {
        // Generate Python source with many expressions to exceed MAX_AST_NODES (100,000).
        // Each line `x = 0 + 1 + 2 + ... + 19` generates ~25 AST nodes (identifiers,
        // operators, integers, expression_statement wrappers), so 4500 lines is enough.
        let mut source = String::new();
        for i in 0..4500 {
            source.push_str("x = ");
            for j in 0..20 {
                if j > 0 {
                    source.push_str(" + ");
                }
                source.push_str(&(i * 20 + j).to_string());
            }
            source.push('\n');
        }

        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let tree = parser.parse(&source).unwrap();
        let config = TransformConfig::default();

        // The transform itself still enforces the cap; the *dispatcher* is what
        // degrades a ComplexityLimit to passthrough (see types.rs). This direct
        // call therefore surfaces the typed cap error.
        let result = transform_minimal(&source, &tree, Language::Python, &config);
        let err = result.expect_err("Expected error when exceeding MAX_AST_NODES");
        assert!(
            err.is_complexity_limit(),
            "Expected a ComplexityLimit error, got: {err}"
        );
    }

    // NOTE: MAX_AST_DEPTH (500) is not tested because tree-sitter grammars impose
    // their own nesting limits that are well below 500 levels. Even deeply nested
    // expressions like `(((((...))))` do not produce 500 levels of AST depth in
    // practice. The depth guard exists as a defense-in-depth measure against
    // hypothetical malicious grammars or future grammar changes.

    // ========================================================================
    // Scaling regression guard — catches regression to O(N²)
    // ========================================================================
    //
    // The single-point timing tests above catch catastrophic regressions but cannot
    // reliably detect a reintroduced O(N²) path if the constant is small.
    // This doubling-ratio test makes the complexity class directly observable:
    //
    //   O(N):  doubling N doubles time → ratio ≈ 2.0
    //   O(N²): doubling N quadruples time → ratio ≈ 4.0
    //
    // Design choice: wall-clock ratio over a deterministic step counter.
    // A per-iteration step counter at the outer loop level would count the same
    // N+1 iterations for BOTH O(N) and O(N²) implementations — the cost difference
    // is internal to tree-sitter's C code (ts_node_parent re-walks from the root,
    // ts_node_named_child re-scans the child list from position 0). These are
    // invisible from Rust without modifying tree-sitter. The ratio test at
    // large-enough N (where signal exceeds scheduling noise) is the practical
    // discriminator.
    //
    // NOTE: transform_minimal is called directly here, not via the CLI binary, so
    // the skim file-level disk cache (which would mask the defect by serving a cached
    // result instead of running the transform) is not involved. The measurements
    // reflect the transform cost directly.
    //
    // Empirical basis (debug build, fix/init-pin-wrappers-header-comments):
    //   O(N²) unfixed: N=1000 → 125.5ms, N=2000 → 508.0ms, ratio = 4.05
    //   O(N)  fixed:   N=2000 → ~12.7ms,  N=8000 → ~30.4ms
    //                  N=4000 → ~18.6ms (linear interpolation from the two points above)
    //   N=4000 vs N=8000 ratio on fixed code:         ~30.4 / ~18.6 ≈ 1.63×
    //   N=4000 vs N=8000 ratio on O(N²) unfixed code: ~8032ms / ~2008ms ≈ 4.0×
    //   Threshold 2.8 ≈ 2^1.5: exponent-space midpoint between 2^1 (linear) and
    //     2^2 (quadratic).  The historical 2.5 was empirically fitted; 2.8 is
    //     derived.  Both give adequate margin (≥ 0.9 from linear upper bound,
    //     ≥ 1.2 below quadratic lower at 4.0×).
    //
    // N sizes chosen so that t1 (N=4000) reliably exceeds 2 ms even on fast debug
    // hardware (~18 ms measured), keeping the noise floor assertion below the expected
    // measurement by ~9×.

    /// Time N=4000 and N=8000 contiguous-leading-comment Python files with
    /// `transform_minimal`.  Parse is hoisted outside the sample loop (per
    /// `scaling_guard` module doc).  Returns (min, median) of 5 samples for each N.
    fn sample_python_minimal(source: &str) -> (f64, f64) {
        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let tree = parser.parse(source).unwrap(); // hoisted outside sample loop
        let config = TransformConfig::default();
        crate::transform::scaling_guard::time_5(|| {
            let start = std::time::Instant::now();
            let r = transform_minimal(source, &tree, Language::Python, &config);
            assert!(
                r.is_ok(),
                "Python minimal transform must succeed: {:?}",
                r.err()
            );
            start.elapsed().as_secs_f64() * 1000.0
        })
    }

    #[test]
    fn test_quadratic_scaling_guard() {
        // WHAT THIS TEST PROVES: that the doubling ratio (N=4000 → N=8000) stays
        // below 2.8×.  An O(N) implementation produces ~1.3–1.6×; O(N²) produces
        // ~4.0×.  Threshold 2.8 ≈ 2^1.5 is the exponent-space midpoint (derived;
        // see the comment block above the helper).
        //
        // Each call returns (min, median) of 5 samples (scaling_guard rule).
        // Ratio uses MIN; noise floor uses MEDIAN.
        // Parse is hoisted outside the sample loop — we time the walk, not the parser.
        //
        // WHAT THIS TEST DOES NOT PROVE: absolute throughput or strict O(N) vs
        // O(N log N). It discriminates linear from quadratic, no finer.
        let make_source = |n: usize| {
            let mut s = String::with_capacity(n * 25 + 16);
            for i in 0..n {
                s.push_str(&format!("# Header comment {i}\n"));
            }
            s.push_str("def f(x): return x\n");
            s
        };
        let source_4k = make_source(4000);
        let source_8k = make_source(8000);

        // Warm up before the first timed run.
        {
            let mut parser = crate::Parser::new(Language::Python).unwrap();
            let tree = parser.parse(&source_4k).unwrap();
            let config = TransformConfig::default();
            let _ = transform_minimal(&source_4k, &tree, Language::Python, &config);
        }

        let (t1_min, t1_median) = sample_python_minimal(&source_4k);
        let (t2_min, _) = sample_python_minimal(&source_8k);

        // Noise floor uses MEDIAN (absolute gate — scaling_guard rule).
        // We FAIL rather than skip: a silently-passing guard provides no protection.
        assert!(
            t1_median >= 2.0,
            "N=4000 median of 5 completed in {t1_median:.3}ms — too fast to measure \
             reliably (expected ≥ 2ms; ~18ms measured on debug builds). Either the \
             transform is being cached/skipped or N should be raised further. \
             DO NOT convert this to a skip — a silently-passing guard provides no protection."
        );

        // Ratio uses MIN (ratio gate — scaling_guard rule).
        // Threshold is 2.8 ≈ 2^1.5, the exponent-space midpoint (derived, not fitted).
        // A3 discrimination evidence: under a quadratic walk the ratio was 3.75×;
        // normal implementation measures ~1.63×.
        let ratio = t2_min / t1_min;
        assert!(
            ratio < 2.8,
            "Doubling N from 4000 to 8000 must produce a ratio below 2.8 (got {ratio:.2}×). \
             O(N) → ~1.3–1.6×; O(N²) → ~4.0× (empirically measured). \
             Threshold 2.8 ≈ 2^1.5 is the exponent-space midpoint between linear and quadratic. \
             This indicates a regression to super-linear scaling. Check that \
             compute_header_end_byte uses a TreeCursor (not next_named_sibling), \
             is_module_header_comment uses depth (not parent() calls), and \
             collect_removable_comments threads in_function_body (not is_inside_function_body). \
             N=4000 min {t1_min:.1}ms, N=8000 min {t2_min:.1}ms."
        );
    }

    // ── build_newline_table correctness ─────────────────────────────────────

    #[test]
    fn test_build_newline_table_empty() {
        let table = build_newline_table("");
        assert!(table.is_empty(), "empty source → empty table");
    }

    #[test]
    fn test_build_newline_table_no_newlines() {
        let table = build_newline_table("hello world");
        assert!(table.is_empty(), "no newlines → empty table");
    }

    #[test]
    fn test_build_newline_table_single_newline() {
        let table = build_newline_table("hello\nworld");
        assert_eq!(table, vec![5], "single newline at byte 5");
    }

    #[test]
    fn test_build_newline_table_multiple_newlines() {
        let table = build_newline_table("a\nb\nc\n");
        assert_eq!(table, vec![1, 3, 5]);
    }

    #[test]
    fn test_build_newline_table_multibyte() {
        // CJK char (3 bytes each): "你" is U+4F60, 3 bytes; newline at byte 3
        let source = "你\nhello\n";
        let table = build_newline_table(source);
        // "你" is bytes 0..3, '\n' at byte 3, "hello" at bytes 4..9, '\n' at byte 9
        assert_eq!(
            table,
            vec![3, 9],
            "CJK multibyte source, newlines at bytes 3 and 9"
        );
    }

    // ========================================================================
    // Go doc-comment precompute — differential equivalence against the original
    // ========================================================================
    //
    // `reference_is_go_doc_comment` below is a BYTE-FOR-BYTE copy of the
    // per-node forward sibling walk that `compute_go_doc_comment_starts`
    // replaced. Keeping it here converts the one-shot byte-identity check done
    // at the time of the change into a PERMANENT invariant: the precompute must
    // agree with the original walk for every comment node in every snippet and
    // fixture below.
    //
    // If this test ever fails, the two traps to re-read are:
    //   1. the `current_end <= sib_start` guard SKIPS the blank-line test when
    //      it fails — it does not terminate the run; and
    //   2. anonymous siblings are invisible to `next_named_sibling()` and must
    //      neither terminate a run nor update `current_end`.

    /// VERBATIM copy of the original O(M³) walk. Do not "clean up" — its value
    /// is that it is textually the pre-change implementation.
    fn reference_is_go_doc_comment(node: Node, source: &str) -> bool {
        let mut current_end = node.end_byte();
        let mut sibling = node.next_named_sibling();

        while let Some(sib) = sibling {
            let sib_start = sib.start_byte();

            if current_end <= sib_start && sib_start <= source.len() {
                let between = &source[current_end..sib_start];
                let newline_count = between.chars().filter(|&c| c == '\n').count();
                if newline_count > 1 {
                    return false;
                }
            }

            if is_comment_node(sib.kind(), Language::Go) {
                current_end = sib.end_byte();
                sibling = sib.next_named_sibling();
                continue;
            }

            return is_go_declaration(sib.kind());
        }

        false
    }

    fn collect_go_comment_nodes<'a>(node: Node<'a>, out: &mut Vec<Node<'a>>) {
        if is_comment_node(node.kind(), Language::Go) {
            out.push(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            collect_go_comment_nodes(child, out);
        }
    }

    /// Assert the precompute agrees with the reference walk on every comment
    /// node in `source`. Returns the number of comment nodes compared.
    fn assert_precompute_matches_reference(label: &str, source: &str) -> usize {
        let mut parser = crate::Parser::new(Language::Go).unwrap();
        let tree = parser.parse(source).unwrap();
        let root = tree.root_node();

        let starts = compute_go_doc_comment_starts(root, source, Language::Go);
        assert!(
            starts.windows(2).all(|w| w[0] < w[1]),
            "[{label}] precomputed starts must be strictly ascending: {starts:?}"
        );

        let mut comments = Vec::new();
        collect_go_comment_nodes(root, &mut comments);

        for c in &comments {
            let expected = reference_is_go_doc_comment(*c, source);
            let actual = is_go_doc_comment(*c, &starts);
            assert_eq!(
                actual,
                expected,
                "[{label}] classification diverged for comment at byte {} ({:?}); \
                 reference walk said {expected}, precompute said {actual}",
                c.start_byte(),
                c.utf8_text(source.as_bytes()).unwrap_or("<invalid utf8>"),
            );
        }
        comments.len()
    }

    /// Degenerate Go snippets. Each exercises a shape that the run-linearisation
    /// could plausibly get wrong.
    const GO_DIFFERENTIAL_SNIPPETS: &[(&str, &str)] = &[
        (
            "comment-at-eof",
            "package main\n\nfunc f() {}\n\n// trailing comment at EOF\n",
        ),
        (
            "comment-before-package",
            "// leading one\n// leading two\npackage main\n",
        ),
        ("decl-function", "package main\n\n// doc\nfunc f() {}\n"),
        (
            "decl-method",
            "package main\n\ntype T struct{}\n\n// doc\nfunc (t T) M() {}\n",
        ),
        ("decl-type", "package main\n\n// doc\ntype T struct{}\n"),
        ("decl-var", "package main\n\n// doc\nvar X = 1\n"),
        ("decl-const", "package main\n\n// doc\nconst C = 1\n"),
        (
            "decl-type-spec-in-group",
            "package main\n\ntype (\n\t// doc for the spec\n\tW struct{ x int }\n)\n",
        ),
        (
            "field-declaration-list",
            "package main\n\ntype T struct {\n\t// field comment\n\tx int\n}\n",
        ),
        (
            "interface-type",
            "package main\n\ntype I interface {\n\t// method comment\n\tM() int\n}\n",
        ),
        (
            "const-group",
            "package main\n\nconst (\n\t// const comment\n\tA = 1\n\tB = 2\n)\n",
        ),
        (
            "block-comments",
            "package main\n\n/* block doc */\nfunc f() {}\n\n/* orphan block */\n\n/* another */\nvar V = 1\n",
        ),
        (
            "blank-line-inside-run",
            "package main\n\n// run a1\n// run a2\n\n// run b1\n// run b2\nfunc f() {}\n",
        ),
        (
            "crlf",
            "package main\r\n\r\n// doc line one\r\n// doc line two\r\nfunc f() {}\r\n\r\n// orphan\r\n\r\nvar V = 1\r\n",
        ),
        (
            "cjk",
            "package main\n\n// 版权所有 2024 公司名\n// これはドキュメントコメントです\nfunc f() {}\n",
        ),
        (
            "malformed-error-nodes",
            "package main\n\n// doc before broken code\nfunc f( { ] ) unclosed\n\n// another comment\ntype T struct{\n",
        ),
        (
            "comments-inside-function-body",
            "package main\n\nfunc f() {\n\t// body comment\n\tx := 1\n\t// another body comment\n\treturn\n}\n",
        ),
        ("only-comments", "// one\n// two\n// three\n"),
        ("empty", ""),
        (
            "import-block",
            "package main\n\nimport (\n\t// import comment\n\t\"fmt\"\n)\n",
        ),
        // ANONYMOUS SIBLING between a comment and its next NAMED sibling.
        //
        // The old walk used `next_named_sibling()`, which skips anonymous nodes
        // entirely; `scan_go_sibling_group` must reproduce that by filtering on
        // `is_named()`.  Here `source_file`'s children are
        // `package_clause, var_declaration, comment, ";"(anonymous), var_declaration`
        // — so the run terminator is the second `var_declaration` (an
        // `is_go_declaration` kind → KEEP).  Without the `is_named()` filter the
        // terminator would be the anonymous `";"` → STRIP, a silent
        // classification flip.
        //
        // Added because the rest of this table has ZERO comments with an
        // anonymous sibling before their next named one: deleting the
        // `is_named()` filter passed the entire suite before this entry existed.
        (
            "anon-semi-between-comment-and-next-named-sibling",
            "package main\n\nvar A = 1 /* doc */ ; var B = 2\n",
        ),
    ];

    #[test]
    fn test_go_doc_comment_precompute_matches_reference_walk_on_snippets() {
        let mut total = 0usize;
        for (label, source) in GO_DIFFERENTIAL_SNIPPETS {
            total += assert_precompute_matches_reference(label, source);
        }
        // Tripwire against a snippet being silently dropped from the table.
        assert!(
            GO_DIFFERENTIAL_SNIPPETS.len() >= 20,
            "the degenerate-snippet table must keep its coverage; got {} entries",
            GO_DIFFERENTIAL_SNIPPETS.len()
        );
        // 33 comment nodes across the 21 snippets as of this commit.
        assert!(
            total >= 30,
            "expected the degenerate snippets to contribute a meaningful number of \
             comment nodes to the differential comparison; got {total}"
        );
    }

    #[test]
    fn test_go_doc_comment_precompute_matches_reference_walk_on_fixtures() {
        const GO_FIXTURES: &[(&str, &str)] = &[
            (
                "simple.go",
                include_str!("../../../../tests/fixtures/go/simple.go"),
            ),
            (
                "comments.go",
                include_str!("../../../../tests/fixtures/go/comments.go"),
            ),
            (
                "large_doc_blocks.go",
                include_str!("../../../../tests/fixtures/go/large_doc_blocks.go"),
            ),
        ];
        let mut total = 0usize;
        for (label, source) in GO_FIXTURES {
            total += assert_precompute_matches_reference(label, source);
        }
        // 869 comment nodes as of this commit (849 of them in large_doc_blocks.go).
        // A floor rather than an exact count so editing a fixture does not fail here.
        assert!(
            total >= 500,
            "expected the Go fixtures to contribute >= 500 comment nodes to the \
             differential comparison; got {total}"
        );
    }

    // ── large_doc_blocks.go section behaviour ────────────────────────────────
    //
    // Each section of the fixture uses a DISTINCT marker so `contains()` is
    // unambiguous. See the section table at the top of the fixture.

    const LARGE_DOC_BLOCKS: &str =
        include_str!("../../../../tests/fixtures/go/large_doc_blocks.go");

    fn transform_go(source: &str, mode_minimal: bool) -> String {
        let mut parser = crate::Parser::new(Language::Go).unwrap();
        let tree = parser.parse(source).unwrap();
        let config = TransformConfig::default();
        if mode_minimal {
            transform_minimal(source, &tree, Language::Go, &config).unwrap()
        } else {
            crate::transform::pseudo::transform_pseudo_with_spans_and_line_map(
                source,
                &tree,
                Language::Go,
                &config,
            )
            .unwrap()
            .0
        }
    }

    #[test]
    fn test_large_doc_blocks_sections_minimal() {
        let out = transform_go(LARGE_DOC_BLOCKS, true);

        // Section A: leading run before `package main` — terminator is
        // package_clause, not a declaration → every line STRIPPED.
        assert!(
            !out.contains("SECTIONA_HEADERRUN"),
            "Section A (run before `package main`) must be stripped entirely"
        );

        // Section B: alternating doc blocks.
        assert!(
            out.contains("SECTIONB_KEEP_0") && out.contains("SECTIONB_KEEP_124"),
            "Section B KEEP blocks (adjacent to a declaration) must be preserved"
        );
        assert!(
            !out.contains("SECTIONB_STRIP_0") && !out.contains("SECTIONB_STRIP_124"),
            "Section B STRIP blocks (blank line before the declaration) must be stripped"
        );

        // Section C: 100-line run with a blank line before the declaration.
        assert!(
            !out.contains("SECTIONC_BLANKBROKEN"),
            "Section C (blank line between run and declaration) must be stripped entirely"
        );

        // Section D: 100-line run immediately followed by a declaration. This is
        // the case a naive scalar-boundary fix gets wrong — the run is long and
        // every one of its 100 lines is a doc comment.
        assert!(
            out.contains("SECTIOND_ADJACENT line 0") && out.contains("SECTIOND_ADJACENT line 99"),
            "Section D (run immediately followed by a declaration) must be preserved in full"
        );

        // Section E: nested sibling groups. The grouped `type (…)` comment
        // precedes a type_spec — an is_go_declaration kind — so it is a KEEP
        // that a ROOT-CHILDREN-ONLY precompute would silently misclassify.
        assert!(
            out.contains("SECTIONE_TYPESPEC"),
            "Section E grouped-type-spec doc comment must be preserved — a \
             root-children-only precompute would wrongly strip it"
        );
        assert!(
            !out.contains("SECTIONE_FIELD"),
            "Section E field_declaration_list comment must be stripped"
        );
        assert!(
            !out.contains("SECTIONE_IFACE"),
            "Section E interface_type comment must be stripped"
        );
        assert!(
            !out.contains("SECTIONE_CONST"),
            "Section E const-group comment must be stripped"
        );
    }

    #[test]
    fn test_large_doc_blocks_sections_pseudo() {
        // Go's pseudo rules strip no node kinds and no keywords, so pseudo mode
        // exercises the same comment classification as minimal. pseudo is the
        // mode the cat/head/tail rewrite selects for regular code files
        // (ADR-008), which makes it the production path for this predicate.
        let out = transform_go(LARGE_DOC_BLOCKS, false);
        assert!(!out.contains("SECTIONA_HEADERRUN"));
        assert!(out.contains("SECTIONB_KEEP_0"));
        assert!(!out.contains("SECTIONB_STRIP_0"));
        assert!(!out.contains("SECTIONC_BLANKBROKEN"));
        assert!(
            out.contains("SECTIOND_ADJACENT line 0") && out.contains("SECTIOND_ADJACENT line 99"),
            "Section D must survive pseudo mode in full"
        );
        assert!(
            out.contains("SECTIONE_TYPESPEC"),
            "nested grouped-type-spec doc comment must survive pseudo mode"
        );
        assert!(!out.contains("SECTIONE_FIELD"));
    }

    // ── Go pseudo-mode comment coverage (previously untested) ────────────────
    //
    // Before this test the only Go pseudo test used a fixture with zero
    // comments, so Go's pseudo comment path had NO coverage at all.

    #[test]
    fn test_go_pseudo_preserves_doc_comments_and_strips_orphans() {
        const COMMENTS_GO: &str = include_str!("../../../../tests/fixtures/go/comments.go");
        let out = transform_go(COMMENTS_GO, false);

        // Doc comments adjacent to a declaration — KEEP.
        assert!(
            out.contains("// Add adds two numbers together."),
            "Go doc comment adjacent to func must survive pseudo; got:\n{out}"
        );
        assert!(
            out.contains("// Calculator is a simple calculator."),
            "Go doc comment adjacent to type must survive pseudo; got:\n{out}"
        );
        assert!(
            out.contains("// NewCalculator creates a new Calculator."),
            "Go doc comment on constructor must survive pseudo; got:\n{out}"
        );

        // Comments inside a function body — KEEP (in_function_body flag).
        assert!(
            out.contains("// This comment is inside a function body (KEEP)"),
            "in-body comment must survive pseudo; got:\n{out}"
        );

        // Orphan comments separated from any declaration — STRIP.
        assert!(
            !out.contains("// This is a standalone comment not adjacent"),
            "orphan comment must be stripped by pseudo; got:\n{out}"
        );
        assert!(
            !out.contains("/* This is a standalone block comment (STRIP) */"),
            "orphan block comment must be stripped by pseudo; got:\n{out}"
        );
        // Struct field comment — not a function body, terminator is a
        // field_declaration (not an is_go_declaration kind) — STRIP.
        assert!(
            !out.contains("// Value field comment inside struct"),
            "struct field comment must be stripped by pseudo; got:\n{out}"
        );
    }

    // ========================================================================
    // Go comment-classification scaling guards
    // ========================================================================
    //
    // All three pre-existing perf guards in this file are PYTHON-only, so a Go
    // regression had no coverage whatsoever: an O(N²) Go regression at N=500
    // finishes in ~31 ms and passes silently.
    //
    // MEASURED SERIES (DEBUG build, this branch, best-of-1 pre-fix / best-of-3
    // post-fix; `transform_minimal` / `transform_pseudo_*` called directly, so
    // the skim file-level disk cache is NOT involved — a warm parser cache
    // hides exactly this defect class, PF-020).
    //
    //   Go leading comment run before `package main`, minimal:
    //     BEFORE (Θ(M³/3)):  N=250 → 665.43 ms | N=500 → 5231.39 ms | N=1000 → 41615.57 ms
    //                        ratios 7.86×, 7.95×   →   α = 2.98
    //     AFTER  (linear):   N=250 →   0.38 ms | N=500 →    0.77 ms | N=1000 →     1.48 ms
    //                        ratios 2.00×, 1.94×   →   α = 0.98        (28118× at N=1000)
    //
    //   Go leading comment run before `package main`, pseudo:
    //     BEFORE:            N=250 → 656.58 ms | N=500 → 5212.06 ms | N=1000 → 41571.69 ms
    //                        ratios 7.94×, 7.98×   →   α = 2.99
    //     AFTER:             N=250 →   0.44 ms | N=500 →    0.85 ms | N=1000 →     1.66 ms
    //                        ratios 1.93×, 1.95×   →   α = 0.96        (25043× at N=1000)
    //
    //   Go 3-line doc blocks each followed by a declaration, minimal:
    //     BEFORE:            n=500 →  21.42 ms | n=1000 →   45.29 ms | n=2000 →   96.01 ms
    //                        ratios 2.11×, 2.12×   →   α = 1.08
    //     AFTER:             n=500 →  12.92 ms | n=1000 →   25.28 ms | n=2000 →   50.32 ms
    //                        ratios 1.96×, 1.99×   →   α = 0.98
    //
    // NOTE, recorded deliberately: the doc-block shape was ALREADY LINEAR
    // before the fix (α = 1.08, not the α ≈ 2 that was predicted). Short runs
    // bound the forward walk to ~3 sibling steps, so the cubic term never
    // develops. Its guard below is therefore a REGRESSION guard, not evidence
    // of the fix. The leading-run shape is the one that demonstrates the defect.
    //
    // AFTER, at the guard sizes used below (best-of-3):
    //   leading-run/minimal: N=2000 → 4.62 ms | N=4000 → 6.48 ms | N=8000 → 11.91 ms
    //   leading-run/pseudo : N=2000 → 3.26 ms | N=4000 → 6.58 ms | N=8000 → 13.27 ms
    //
    // THRESHOLD RATIONALE — the ratio guards use 2.8, not the 2.5 used by the
    // Python `test_quadratic_scaling_guard`. A doubling ratio of 2.8 ≈ 2^1.5 is
    // the exponent-space midpoint between linear (2.0×) and quadratic (4.0×).
    // The Python guard can afford 2.5 because its measured ratio is ~1.63 —
    // fixed overhead pulls it well below 2.0. The Go guards measure ~1.84–2.02,
    // so 2.5 would leave only ~25 % headroom and flake on a loaded machine.

    /// N contiguous comments at the very top, then `package main`.
    /// Worst case for the old walk: every comment walked the whole remaining run.
    fn go_leading_run_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 26 + 64);
        for i in 0..n {
            s.push_str(&format!("// leading run line {i}\n"));
        }
        s.push_str("package main\n\nfunc f() int { return 1 }\n");
        s
    }

    /// `n` doc blocks of 3 comment lines, each immediately followed by a func.
    fn go_doc_blocks_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 120 + 64);
        s.push_str("package main\n\n");
        for i in 0..n {
            s.push_str(&format!("// Fn{i} does a thing.\n"));
            s.push_str(&format!("// More detail about Fn{i}.\n"));
            s.push_str(&format!("// Even more detail about Fn{i}.\n"));
            s.push_str(&format!(
                "func Fn{i}(a int, b int) int {{\n\treturn a + b\n}}\n\n"
            ));
        }
        s
    }

    /// Time `transform_minimal` on `source` with 5 samples.
    /// Parse is hoisted outside the sample loop — we time the walk, not the parser.
    /// Returns `(min, median)` per the `scaling_guard` sampling rule.
    fn time_go_minimal(source: &str) -> (f64, f64) {
        let mut parser = crate::Parser::new(Language::Go).unwrap();
        let tree = parser.parse(source).unwrap(); // hoisted outside sample loop
        let config = TransformConfig::default();
        crate::transform::scaling_guard::time_5(|| {
            let start = std::time::Instant::now();
            let r = transform_minimal(source, &tree, Language::Go, &config);
            assert!(r.is_ok(), "minimal transform must succeed: {:?}", r.err());
            start.elapsed().as_secs_f64() * 1000.0
        })
    }

    /// Time `transform_pseudo` on `source` with 5 samples.
    /// Parse is hoisted outside the sample loop — we time the walk, not the parser.
    /// Returns `(min, median)` per the `scaling_guard` sampling rule.
    fn time_go_pseudo(source: &str) -> (f64, f64) {
        let mut parser = crate::Parser::new(Language::Go).unwrap();
        let tree = parser.parse(source).unwrap(); // hoisted outside sample loop
        let config = TransformConfig::default();
        crate::transform::scaling_guard::time_5(|| {
            let start = std::time::Instant::now();
            let r = crate::transform::pseudo::transform_pseudo_with_spans_and_line_map(
                source,
                &tree,
                Language::Go,
                &config,
            );
            assert!(r.is_ok(), "pseudo transform must succeed: {:?}", r.err());
            start.elapsed().as_secs_f64() * 1000.0
        })
    }

    /// Cheap cubic tripwire. Runs FIRST inside each ratio guard so that a
    /// reintroduced Θ(M³) implementation fails in ~40 s at N=1000 instead of
    /// letting the N=8000 measurement grind for hours.
    /// Uses MEDIAN of 5 samples (absolute gate — scaling_guard rule).
    fn assert_no_cubic_regression(timer: fn(&str) -> (f64, f64), mode: &str) {
        let (_, probe_median) = timer(&go_leading_run_source(1000));
        assert!(
            probe_median < 200.0,
            "[{mode}] N=1000 Go leading comment run median took {probe_median:.1}ms (budget 200ms). \
             The Θ(M³/3) per-node next_named_sibling() walk took 41616ms here; the \
             linear precompute takes ~1.5ms. This is a CUBIC regression — check that \
             is_go_doc_comment does a binary_search over compute_go_doc_comment_starts \
             and does NOT walk siblings (PF-020)."
        );
    }

    #[test]
    fn test_go_leading_comment_run_linear_time() {
        // CUBIC SMOKE TEST. Pre-fix: 41616 ms at N=1000. Post-fix: 1.48 ms.
        // Budget 200 ms leaves ~135× headroom over the fixed code while the
        // cubic code exceeds it by ~208×.
        let n = 1000usize;
        let source = go_leading_run_source(n);
        let (_, elapsed_median) = time_go_minimal(&source);

        // Behaviour assertion alongside the timing: the whole run precedes
        // `package main`, which is NOT an is_go_declaration kind, so every one
        // of the N comments must be stripped.
        let out = transform_go(&source, true);
        assert!(
            !out.contains("// leading run line"),
            "a leading comment run terminated by package_clause must be stripped entirely"
        );

        // Absolute gate: uses MEDIAN of 5 samples (scaling_guard rule).
        assert!(
            elapsed_median < 200.0,
            "{n} leading Go comments must process in < 200ms median (got {elapsed_median:.1}ms); \
             the old Θ(M³/3) walk took 41616ms at N={n}."
        );
    }

    #[test]
    fn test_go_leading_comment_run_scaling_guard() {
        assert_no_cubic_regression(time_go_minimal, "minimal");

        let (t1_min, t1_median) = time_go_minimal(&go_leading_run_source(4000));
        let (t2_min, _) = time_go_minimal(&go_leading_run_source(8000));

        // Noise floor uses MEDIAN (absolute gate — scaling_guard rule).
        // We FAIL rather than skip: a silently-passing scaling guard provides no protection.
        assert!(
            t1_median >= 1.5,
            "N=4000 median of 5 completed in {t1_median:.3}ms — too fast to measure reliably \
             (expected ≥ 1.5ms; ~6.5ms measured on debug builds). Either the \
             transform is being cached/skipped or N must be raised. \
             DO NOT convert this to a skip."
        );

        // Ratio uses MIN (ratio gate — scaling_guard rule).
        // A3 discrimination evidence: under a quadratic walk the ratio was 3.72×;
        // normal implementation measures 1.84×.
        let ratio = t2_min / t1_min;
        assert!(
            ratio < 2.8,
            "Doubling N from 4000 to 8000 must produce a ratio below 2.8 \
             (got {ratio:.2}×; measured 1.84× on the linear implementation). \
             O(N) → ~2.0×; O(N²) → ~4.0×; 2.8 ≈ 2^1.5 is the exponent-space \
             midpoint. N=4000 min {t1_min:.2}ms, N=8000 min {t2_min:.2}ms."
        );
    }

    #[test]
    fn test_go_doc_block_scaling_guard() {
        // n is capped at 2000: each block is ~17 AST nodes, so n=4000 would
        // approach MAX_AST_NODES (100_000) and silently turn this guard into an
        // Err(ComplexityLimit) assertion instead of a timing assertion.
        //
        // This shape was already linear before the fix (α = 1.08) — it is a
        // REGRESSION guard, not evidence of the fix. See the series above.
        let (t1_min, t1_median) = time_go_minimal(&go_doc_blocks_source(1000));
        let (t2_min, _) = time_go_minimal(&go_doc_blocks_source(2000));

        // Noise floor uses MEDIAN (absolute gate — scaling_guard rule).
        assert!(
            t1_median >= 3.0,
            "n=1000 doc blocks median of 5 completed in {t1_median:.3}ms — too fast to measure \
             reliably (expected ≥ 3ms; ~25ms measured). DO NOT convert to a skip."
        );

        // Ratio uses MIN (ratio gate — scaling_guard rule).
        // A3 discrimination evidence: under a quadratic walk the ratio was 3.54×;
        // normal implementation measures 1.99×.
        let ratio = t2_min / t1_min;
        assert!(
            ratio < 2.8,
            "Doubling doc-block count from 1000 to 2000 must produce a ratio \
             below 2.8 (got {ratio:.2}×; measured 1.99×). \
             n=1000 min {t1_min:.2}ms, n=2000 min {t2_min:.2}ms."
        );
    }

    #[test]
    fn test_go_pseudo_leading_comment_run_scaling_guard() {
        // pseudo is the PRODUCTION path: the cat/head/tail rewrite selects
        // --mode=pseudo for regular code files (ADR-008), so this is the mode an
        // agent actually reads Go through. Go's pseudo rules strip no node kinds
        // and no keywords, so this measures the comment walk almost in isolation.
        assert_no_cubic_regression(time_go_pseudo, "pseudo");

        let (t1_min, t1_median) = time_go_pseudo(&go_leading_run_source(4000));
        let (t2_min, _) = time_go_pseudo(&go_leading_run_source(8000));

        // Noise floor uses MEDIAN (absolute gate — scaling_guard rule).
        assert!(
            t1_median >= 1.5,
            "N=4000 pseudo median of 5 completed in {t1_median:.3}ms — too fast to measure \
             reliably (expected ≥ 1.5ms; ~6.6ms measured). DO NOT convert this to a skip."
        );

        // Ratio uses MIN (ratio gate — scaling_guard rule).
        // A3 discrimination evidence: under a quadratic walk the ratio was 3.67×;
        // normal implementation measures 2.02×.
        let ratio = t2_min / t1_min;
        assert!(
            ratio < 2.8,
            "Doubling N from 4000 to 8000 in pseudo mode must produce a ratio \
             below 2.8 (got {ratio:.2}×; measured 2.02×). \
             N=4000 min {t1_min:.2}ms, N=8000 min {t2_min:.2}ms."
        );
    }

    #[test]
    fn test_adjust_range_uses_newline_table_line_start() {
        // Verify that adjust_range_for_line_removal correctly identifies line_start
        // using the newline table (regression test for O(N²) rfind replacement).
        let source = "first line\nsecond line\nthird line\n";
        let newlines = build_newline_table(source);
        // "second line" starts at byte 11, ends at byte 22
        // line_start should be 11 (byte after '\n' at byte 10)
        let (start, end) = adjust_range_for_line_removal(source, 11, 22, &newlines);
        // "second line\n" is the full line, should remove all of it
        assert_eq!(
            start, 11,
            "line start must be byte 11 (start of 'second line')"
        );
        assert_eq!(
            end, 23,
            "line end must be byte 23 (includes the trailing newline)"
        );
    }

    // ========================================================================
    // C2a — trim_and_normalize literal protection unit tests
    // ========================================================================

    #[test]
    fn test_trim_and_normalize_preserves_protected_trailing_spaces() {
        // Protected range [7..9) covers the two spaces inside the string "  ".
        // trim_and_normalize must NOT trim them.
        // Input: `x = "  "\n` — the `  ` is protected
        let input = "x = \"  \"\n";
        // "x = \"" = 5 bytes, spaces at [5..7), closing " at 7
        // Wait: x=0 ' '=1 '='=2 ' '=3 '"'=4 ' '=5 ' '=6 '"'=7 '\n'=8
        // string_content = bytes [5..7)
        let protected = vec![(5, 7)];
        let result = trim_and_normalize(input, &protected);
        // Trailing `"` is at byte 7 — not a space, so trim_end stops there.
        // The two spaces inside the literal are not at the end of the line anyway.
        assert_eq!(result, "x = \"  \"\n");
    }

    #[test]
    fn test_trim_and_normalize_trailing_protected_spaces_preserved() {
        // When a string literal with trailing spaces is the last thing on the line,
        // the spaces must be preserved if they are in a protected range.
        // Input line: `"  "  ` where the 2 spaces inside quotes are protected [1..3)
        // and the 2 spaces outside the closing quote are NOT protected.
        // Expected: the outside trailing spaces are trimmed, the inside ones preserved.
        // Layout: '"'=0 ' '=1 ' '=2 '"'=3 ' '=4 ' '=5 '\n'=6
        let input = "\"  \"  \n";
        let protected = vec![(1, 3)]; // the two spaces inside the quotes
        let result = trim_and_normalize(input, &protected);
        // Outside trailing spaces [4..6) are trimmed; literal content [1..3) is kept.
        assert_eq!(result, "\"  \"\n");
    }

    #[test]
    fn test_trim_and_normalize_protected_spaces_only_line_not_blank() {
        // A line consisting entirely of protected spaces should NOT be blank.
        // Protected: bytes [0..5) = 5 spaces
        let input = "     \n";
        let protected = vec![(0, 5)];
        let result = trim_and_normalize(input, &protected);
        // Not blank → kept verbatim (no trailing space trim since they're all protected)
        assert_eq!(result, "     \n");
    }

    #[test]
    fn test_trim_and_normalize_five_blank_lines_inside_string_preserved() {
        // 5 blank lines inside a string literal must NOT be capped to 2.
        // Each blank line is a single '\n'; together they form string content.
        // We mark all of them as protected.
        let input = "a\n\n\n\n\n\nb\n";
        // Bytes: a=0 \n=1 \n=2 \n=3 \n=4 \n=5 \n=6 b=7 \n=8
        // Mark the 5 inner blank lines [2..7) as protected
        // (representing multi-line string content)
        let protected = vec![(2, 7)];
        let result = trim_and_normalize(input, &protected);
        // All 5 protected blank lines must survive
        assert_eq!(result, "a\n\n\n\n\n\nb\n");
    }

    // ========================================================================
    // C2a — transform_minimal literal-protection integration tests (14 langs)
    // ========================================================================

    fn transform_min(source: &str, language: Language) -> String {
        let mut parser = crate::Parser::new(language).unwrap();
        let tree = parser.parse(source).unwrap();
        let config = crate::TransformConfig::with_mode(crate::Mode::Minimal);
        transform_minimal(source, &tree, language, &config).unwrap()
    }

    #[test]
    fn test_minimal_ts_literal_double_space_preserved() {
        let source = "const INDENT = \"  \";\n";
        let result = transform_min(source, Language::TypeScript);
        assert!(result.contains("\"  \""), "TS minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_js_literal_double_space_preserved() {
        let source = "const INDENT = '  ';\n";
        let result = transform_min(source, Language::JavaScript);
        assert!(result.contains("'  '"), "JS minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_python_literal_double_space_preserved() {
        let source = "INDENT = \"  \"\n";
        let result = transform_min(source, Language::Python);
        assert!(result.contains("\"  \""), "Python minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_rust_literal_double_space_preserved() {
        let source = "const INDENT: &str = \"  \";\n";
        let result = transform_min(source, Language::Rust);
        assert!(result.contains("\"  \""), "Rust minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_go_literal_double_space_preserved() {
        let source = "var INDENT = \"  \"\n";
        let result = transform_min(source, Language::Go);
        assert!(result.contains("\"  \""), "Go minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_java_literal_double_space_preserved() {
        let source = "class A { String s = \"  \"; }\n";
        let result = transform_min(source, Language::Java);
        assert!(result.contains("\"  \""), "Java minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_c_literal_double_space_preserved() {
        let source = "char *s = \"  \";\n";
        let result = transform_min(source, Language::C);
        assert!(result.contains("\"  \""), "C minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_cpp_literal_double_space_preserved() {
        let source = "std::string s = \"  \";\n";
        let result = transform_min(source, Language::Cpp);
        assert!(result.contains("\"  \""), "C++ minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_csharp_literal_double_space_preserved() {
        let source = "string s = \"  \";\n";
        let result = transform_min(source, Language::CSharp);
        assert!(result.contains("\"  \""), "C# minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_ruby_literal_double_space_preserved() {
        let source = "s = \"  \"\n";
        let result = transform_min(source, Language::Ruby);
        assert!(result.contains("\"  \""), "Ruby minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_kotlin_literal_double_space_preserved() {
        let source = "val s = \"  \"\n";
        let result = transform_min(source, Language::Kotlin);
        assert!(result.contains("\"  \""), "Kotlin minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_swift_literal_double_space_preserved() {
        let source = "let s = \"  \"\n";
        let result = transform_min(source, Language::Swift);
        assert!(result.contains("\"  \""), "Swift minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_sql_literal_double_space_preserved() {
        let source = "SELECT '  ' AS s;\n";
        let result = transform_min(source, Language::Sql);
        assert!(result.contains("'  '"), "SQL minimal: got {result:?}");
    }

    #[test]
    fn test_minimal_bash_literal_double_space_preserved() {
        let source = "s=\"  \"\n";
        let result = transform_min(source, Language::Bash);
        assert!(result.contains("\"  \""), "Bash minimal: got {result:?}");
    }

    // ========================================================================
    // C2d — normalize_line_map_blanks literal-protection integration
    // ========================================================================

    #[test]
    fn test_line_map_integrity_with_protected_space_line() {
        use crate::transform::normalize_line_map_blanks;
        // A line consisting entirely of protected spaces must NOT be dropped from
        // the map (it would be treated as blank without literal awareness).
        let text = "a\n     \nb\n";
        // Bytes: a=0 \n=1 ' '=2 ' '=3 ' '=4 ' '=5 ' '=6 \n=7 b=8 \n=9
        let protected = vec![(2, 7)]; // 5 spaces on line 2 are protected
        let map = vec![1, 2, 3];
        let result = normalize_line_map_blanks(text, map, &protected);
        // Line 2 ("     ") is NOT blank (all spaces are protected) → kept
        assert_eq!(
            result,
            vec![1, 2, 3],
            "protected-space line must not be dropped from map"
        );
    }
}
