//! Minimal mode transformation
//!
//! ARCHITECTURE: Strip non-doc comments at module/class level while keeping all code intact.
//! Preserves doc comments, comments inside function bodies, and shebangs.
//!
//! Token reduction target: 15-30%

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

    // Adjust ranges for full-line removal, sort, and dedup
    let mut final_ranges: Vec<(usize, usize)> = ctx
        .ranges
        .iter()
        .map(|&(start, end)| adjust_range_for_line_removal(source, start, end, &newlines))
        .collect();
    final_ranges.sort_unstable_by_key(|&(start, _)| start);
    final_ranges.dedup();

    let after_removal = remove_ranges(source, &final_ranges)?;
    let normalized = trim_and_normalize(&after_removal);

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

        // Skip overlapping ranges, extending the removal window if needed
        if start < last_pos {
            last_pos = last_pos.max(end);
            continue;
        }

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

/// Trim trailing whitespace from each line and normalize blank lines in a single pass
///
/// Combines two operations to avoid an extra allocation:
/// 1. Trims trailing whitespace from each line
/// 2. Normalizes blank lines: 3+ consecutive blank lines become 2
pub(crate) fn trim_and_normalize(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut consecutive_blanks: usize = 0;

    for line in source.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
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
        result.push_str(trimmed);
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

    // Helper: parse Go source into a tree-sitter Tree.
    fn parse_go(source: &str) -> Tree {
        let mut parser = crate::Parser::new(Language::Go).unwrap();
        parser.parse(source).unwrap()
    }

    /// Total AST node count, derived independently of the walkers under test.
    ///
    /// Explicit stack + `Node::children`, so the count never depends on the
    /// traversal the walker performs. That independence is the whole point: it is
    /// what lets `node_count == count_all_nodes(root)` detect a walker that visits
    /// the same node twice, which is invisible in the output.
    fn count_all_nodes(root: Node) -> usize {
        let mut n = 0usize;
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            n += 1;
            let mut c = node.walk();
            for ch in node.children(&mut c) {
                stack.push(ch);
            }
        }
        n
    }

    /// Drive the production comment walker directly, composing `CommentWalkContext`
    /// exactly the way `transform_minimal` does.
    ///
    /// Returns the raw (unadjusted) removal ranges and the number of nodes the
    /// walker visited. The visit count is the instrument a re-walk regression trips:
    /// descending twice leaves the ranges — and therefore the output — unchanged
    /// while doubling the count.
    fn collect_minimal_ranges(
        source: &str,
        tree: &Tree,
        language: Language,
    ) -> (Vec<(usize, usize)>, usize) {
        let root = tree.root_node();
        let header_end_byte = compute_header_end_byte(root, source, language);
        let go_doc_comment_starts = compute_go_doc_comment_starts(root, source, language);
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut node_count: usize = 0;
        let mut ctx = CommentWalkContext {
            ranges: &mut ranges,
            node_count: &mut node_count,
            classification: CommentClassification {
                header_end_byte,
                go_doc_comment_starts: &go_doc_comment_starts,
            },
        };
        collect_removable_comments(root, source, language, &mut ctx, 0, false)
            .expect("comment walk must succeed on these fixtures");
        (ranges, node_count)
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

    // ── Large header block, end to end ───────────────────────────────────────

    /// A large contiguous header block survives `transform_minimal` intact, and the
    /// comment walker visits every AST node exactly once.
    ///
    /// WHAT THIS PINS: the artifact — which comments survive, that nothing is
    /// collected for removal, and that the walk is single-visit.
    ///
    /// WHAT THIS DOES NOT PIN: complexity. It cannot discriminate O(N) from O(N²);
    /// every historical defect here was *output-preserving* (the cost lived in
    /// tree-sitter's C code, not in the Rust statement count), so no behavioural
    /// assertion can see one. That job belongs to
    /// `contract_transform_walkers_use_no_root_descending_node_apis` in
    /// `transform/mod.rs`.
    #[test]
    fn test_python_large_header_block_survives_transform() {
        let n = 512usize;
        let mut source = String::with_capacity(n * 25);
        for i in 0..n {
            source.push_str(&format!("# Header comment {i}\n"));
        }
        source.push_str("x = 1\n");

        let mut parser = crate::Parser::new(Language::Python).unwrap();
        let tree = parser.parse(&source).unwrap();
        let config = TransformConfig::default();
        let result = transform_minimal(&source, &tree, Language::Python, &config);

        assert!(result.is_ok(), "transform must succeed: {:?}", result.err());
        let output = result.unwrap();

        // All header comments must be preserved (they form a contiguous leading block).
        assert!(
            output.contains("# Header comment 0"),
            "first header comment must be preserved; got:\n{output}"
        );
        assert!(
            output.contains(&format!("# Header comment {}", n - 1)),
            "last header comment must be preserved; got:\n{output}"
        );

        let (ranges, node_count) = collect_minimal_ranges(&source, &tree, Language::Python);
        assert!(
            ranges.is_empty(),
            "the whole block is a module header, so nothing is removable; got {ranges:?}"
        );
        assert_eq!(
            node_count,
            count_all_nodes(tree.root_node()),
            "collect_removable_comments must visit each AST node exactly once"
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
        let result = trim_and_normalize(input);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_reduces_four_blanks_to_two() {
        let input = "a\n\n\n\n\nb\n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "a\n\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_no_change_needed() {
        let input = "a\n\nb\n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "a\n\nb\n");
    }

    #[test]
    fn test_trim_and_normalize_trims_trailing_whitespace() {
        let input = "hello   \nworld  \n";
        let result = trim_and_normalize(input);
        assert_eq!(result, "hello\nworld\n");
    }

    #[test]
    fn test_trim_and_normalize_combined() {
        // Verify both trimming and normalization happen in one pass
        let input = "hello   \n\n\n\n\nworld  \n";
        let result = trim_and_normalize(input);
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
    // Module-header precompute — exact artifact, not elapsed time
    // ========================================================================
    //
    // `compute_header_end_byte` and `collect_removable_comments` are pinned by the
    // exact values they produce and the number of node visits they make.
    //
    // Neither this nor any other behavioural assertion can discriminate O(N) from
    // O(N²) here: the historical defect (a per-node backward `parent()` /
    // `named_child(i)` walk) executed the SAME Rust statements and produced
    // BYTE-IDENTICAL output — the extra cost was entirely inside tree-sitter's C
    // code, where a `TSNode` has no parent pointer and every such call re-descends
    // from the root. The construct itself is therefore forbidden at the source level
    // by `contract_transform_walkers_use_no_root_descending_node_apis` in
    // `transform/mod.rs`, and the measured series that motivated the fix lives in
    // the production rustdoc on `compute_header_end_byte` and `is_go_doc_comment`.

    /// Tail appended after the synthetic header block. A constant because the
    /// expected `header_end_byte` is derived from its length.
    const PY_HEADER_TAIL: &str = "def f(x): return x\n";

    /// `n` contiguous leading comments followed by [`PY_HEADER_TAIL`].
    fn python_header_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 25 + PY_HEADER_TAIL.len());
        for i in 0..n {
            s.push_str(&format!("# Header comment {i}\n"));
        }
        s.push_str(PY_HEADER_TAIL);
        s
    }

    /// The module-header precompute lands on an exact byte, classifies the whole
    /// block as KEEP, and is reached in a single visit per AST node.
    ///
    /// WHAT THIS PINS:
    /// 1. `compute_header_end_byte` returns the end byte of the last leading
    ///    comment — computed from the fixture's shape, not from a recorded run.
    /// 2. `collect_removable_comments` collects nothing (every comment is a header)
    ///    and visits each node exactly once — the assertion a re-walk regression
    ///    trips while leaving the output untouched.
    /// 3. The production entry point still composes the two (source-corpus PF-014:
    ///    driving the pieces directly proves the counters; driving
    ///    `transform_minimal` proves production still wires them together).
    ///
    /// WHAT THIS DOES NOT PIN: complexity. It does NOT discriminate O(N) from
    /// O(N²) — the defect was output-preserving. See the section note above and
    /// the contract test in `transform/mod.rs`.
    #[test]
    fn test_python_module_header_precompute_is_exact_and_single_visit() {
        for n in [256usize, 512] {
            let source = python_header_source(n);
            let tree = parse_python(&source);
            let root = tree.root_node();

            // The last comment ends just before the newline that precedes the tail.
            let expected_header_end = source.len() - PY_HEADER_TAIL.len() - 1;
            assert_eq!(
                compute_header_end_byte(root, &source, Language::Python),
                expected_header_end,
                "n={n}: header_end_byte must be the end byte of the last leading comment"
            );

            let (ranges, node_count) = collect_minimal_ranges(&source, &tree, Language::Python);
            assert!(
                ranges.is_empty(),
                "n={n}: a fully contiguous header block yields no removable comments; \
                 got {ranges:?}"
            );
            assert_eq!(
                node_count,
                count_all_nodes(root),
                "n={n}: collect_removable_comments must visit each AST node exactly once"
            );

            let config = TransformConfig::default();
            let out = transform_minimal(&source, &tree, Language::Python, &config).unwrap();
            assert!(
                out.contains("# Header comment 0"),
                "n={n}: first header comment must survive; got:\n{out}"
            );
            assert!(
                out.contains(&format!("# Header comment {}", n - 1)),
                "n={n}: last header comment must survive; got:\n{out}"
            );
            assert_eq!(
                out.lines().count(),
                source.lines().count(),
                "n={n}: nothing is removed, so the line count must be preserved"
            );
        }
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
    // Go comment classification — exact artifacts, not elapsed time
    // ========================================================================
    //
    // These pin what `compute_go_doc_comment_starts` and the two walkers PRODUCE,
    // and how many node visits it takes them.
    //
    // They do NOT discriminate O(N) from O(N²)/Θ(M³), and no behavioural assertion
    // can: the Θ(M³/3) per-node `next_named_sibling()` walk executed the SAME Rust
    // statements as the precompute and produced BYTE-IDENTICAL output. The whole
    // cost difference lived in tree-sitter's C code, where a `TSNode` has no parent
    // pointer and every sibling step rescans the parent's child list from 0. The
    // construct is therefore forbidden at the source level by
    // `contract_transform_walkers_use_no_root_descending_node_apis` in
    // `transform/mod.rs`; the measured before/after series that motivated the fix
    // lives in the production rustdoc on `is_go_doc_comment`.

    /// N contiguous comments at the very top, then `package main`.
    ///
    /// `package_clause` is not an `is_go_declaration` kind, so the entire run is
    /// STRIP — not one comment in it is a doc comment.
    fn go_leading_run_source(n: usize) -> String {
        let mut s = String::with_capacity(n * 26 + 64);
        for i in 0..n {
            s.push_str(&format!("// leading run line {i}\n"));
        }
        s.push_str("package main\n\nfunc f() int { return 1 }\n");
        s
    }

    /// `n` doc blocks of 3 comment lines, each immediately followed by a func.
    ///
    /// Every comment is a doc comment (KEEP): each run terminates on a
    /// `function_declaration`, which IS an `is_go_declaration` kind.
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

    /// Byte offsets of every `//` in `source` — the comment start bytes derived
    /// from the TEXT, independently of the AST walk under test.
    ///
    /// Both Go fixtures above put exactly one `//` at the start of each comment
    /// line and none anywhere else, so this is an exact expectation, not a proxy.
    fn go_comment_start_bytes(source: &str) -> Vec<usize> {
        source.match_indices("//").map(|(i, _)| i).collect()
    }

    /// The comment nodes' `(start, end)` byte ranges, derived from the text: each
    /// synthetic comment runs from its `//` to the end of its line.
    fn go_comment_ranges(source: &str) -> Vec<(usize, usize)> {
        go_comment_start_bytes(source)
            .into_iter()
            .map(|start| {
                let len = source[start..]
                    .find('\n')
                    .expect("every synthetic comment line ends in a newline");
                (start, start + len)
            })
            .collect()
    }

    /// A leading comment run terminated by `package main` yields no doc comments
    /// and is stripped in full by minimal mode.
    ///
    /// WHAT THIS PINS: the artifact — an empty doc-comment table, one removal range
    /// per comment, a single visit per AST node, and the stripped output.
    ///
    /// WHAT THIS DOES NOT PIN: complexity. It does not discriminate O(N) from
    /// Θ(M³) — the defect was output-preserving. See the section note above.
    #[test]
    fn test_go_leading_comment_run_is_stripped_entirely() {
        let n = 256usize;
        let source = go_leading_run_source(n);
        let tree = parse_go(&source);

        // The whole run precedes `package main`, which is NOT an is_go_declaration
        // kind, so NO comment in it is a doc comment.
        let starts = compute_go_doc_comment_starts(tree.root_node(), &source, Language::Go);
        assert!(
            starts.is_empty(),
            "a run terminated by package_clause contains no doc comments; got {starts:?}"
        );

        let out = transform_go(&source, true);
        assert!(
            !out.contains("// leading run line"),
            "a leading comment run terminated by package_clause must be stripped entirely"
        );

        let (ranges, node_count) = collect_minimal_ranges(&source, &tree, Language::Go);
        assert_eq!(
            ranges.len(),
            n,
            "every comment in the run must be collected for removal"
        );
        assert_eq!(
            node_count,
            count_all_nodes(tree.root_node()),
            "collect_removable_comments must visit each AST node exactly once"
        );
    }

    /// The removal ranges for a leading comment run are byte-exact, and the walk is
    /// single-visit, at two sizes.
    ///
    /// WHAT THIS PINS: `collect_removable_comments` emits exactly one range per
    /// comment node, at the byte offsets derived from the source text, and descends
    /// once per AST node.
    ///
    /// WHAT THIS DOES NOT PIN: complexity — the defect was output-preserving. See
    /// the section note above and the contract test in `transform/mod.rs`.
    #[test]
    fn test_go_leading_comment_run_ranges_are_exact_and_single_visit() {
        for n in [128usize, 256] {
            let source = go_leading_run_source(n);
            let tree = parse_go(&source);
            let root = tree.root_node();

            let starts = compute_go_doc_comment_starts(root, &source, Language::Go);
            assert!(
                starts.is_empty(),
                "n={n}: a run terminated by package_clause contains no doc comments; \
                 got {starts:?}"
            );

            let (mut ranges, node_count) = collect_minimal_ranges(&source, &tree, Language::Go);
            ranges.sort_unstable();
            assert_eq!(
                ranges,
                go_comment_ranges(&source),
                "n={n}: one removal range per comment, at the exact comment byte offsets"
            );
            assert_eq!(
                node_count,
                count_all_nodes(root),
                "n={n}: collect_removable_comments must visit each AST node exactly once"
            );

            // The production entry point still composes the precompute and the walk.
            let out = transform_go(&source, true);
            assert!(
                !out.contains("// leading run line"),
                "n={n}: the whole run must be stripped; got:\n{out}"
            );
            assert!(
                out.contains("package main"),
                "n={n}: the terminator must survive; got:\n{out}"
            );
        }
    }

    /// The Go doc-comment precompute is byte-exact, strictly ascending, and reached
    /// in a single visit per AST node.
    ///
    /// WHAT THIS PINS:
    /// 1. `compute_go_doc_comment_starts` equals the `//` offsets derived from the
    ///    text, `3 * n` of them, strictly ascending (`binary_search` depends on it).
    /// 2. `collect_removable_comments` collects nothing — every comment is a doc
    ///    comment — and visits each node exactly once.
    /// 3. `transform_minimal` still composes the two.
    ///
    /// WHAT THIS DOES NOT PIN: complexity — the defect was output-preserving.
    #[test]
    fn test_go_doc_comment_precompute_is_exact_and_single_visit() {
        for n in [128usize, 256] {
            let source = go_doc_blocks_source(n);
            let tree = parse_go(&source);
            let root = tree.root_node();

            let starts = compute_go_doc_comment_starts(root, &source, Language::Go);
            assert_eq!(
                starts,
                go_comment_start_bytes(&source),
                "n={n}: every `//` in this fixture begins a doc comment"
            );
            assert_eq!(
                starts.len(),
                3 * n,
                "n={n}: three doc-comment lines per block"
            );
            assert!(
                starts.windows(2).all(|w| w[0] < w[1]),
                "n={n}: binary_search requires strictly ascending start bytes; got {starts:?}"
            );

            let (ranges, node_count) = collect_minimal_ranges(&source, &tree, Language::Go);
            assert!(
                ranges.is_empty(),
                "n={n}: every comment is a doc comment (KEEP); got {ranges:?}"
            );
            assert_eq!(
                node_count,
                count_all_nodes(root),
                "n={n}: collect_removable_comments must visit each AST node exactly once"
            );

            let out = transform_go(&source, true);
            assert!(
                out.contains("// Fn0 does a thing."),
                "n={n}: first doc comment must survive; got:\n{out}"
            );
            assert!(
                out.contains(&format!("// Even more detail about Fn{}.", n - 1)),
                "n={n}: last doc comment must survive; got:\n{out}"
            );
            assert_eq!(
                out.lines().count(),
                source.lines().count(),
                "n={n}: nothing is removed, so the line count must be preserved"
            );
        }
    }

    /// The same leading-run classification through the PSEUDO path.
    ///
    /// pseudo is the production path: the cat/head/tail rewrite selects
    /// `--mode=pseudo` for regular code files (ADR-008), so this is the mode an
    /// agent actually reads Go through. Go's pseudo rules strip no node kinds, no
    /// keywords and no semicolons, so the comment classification is the only thing
    /// that can change the output.
    ///
    /// WHAT THIS PINS: the shared precompute is empty for this shape, and the
    /// pseudo entry point strips the whole run while leaving the code intact.
    ///
    /// The pseudo walker's own single-visit counter is asserted in `pseudo.rs`,
    /// where `collect_noise_ranges` and `NoiseWalkContext` are in scope.
    ///
    /// WHAT THIS DOES NOT PIN: complexity — the defect was output-preserving.
    #[test]
    fn test_go_pseudo_leading_comment_run_is_stripped_entirely() {
        for n in [128usize, 256] {
            let source = go_leading_run_source(n);
            let tree = parse_go(&source);

            let starts = compute_go_doc_comment_starts(tree.root_node(), &source, Language::Go);
            assert!(
                starts.is_empty(),
                "n={n}: a run terminated by package_clause contains no doc comments; \
                 got {starts:?}"
            );

            let out = transform_go(&source, false);
            assert!(
                !out.contains("// leading run line"),
                "n={n}: pseudo must strip the whole run; got:\n{out}"
            );
            assert!(
                out.contains("package main"),
                "n={n}: pseudo must preserve the package clause; got:\n{out}"
            );
            assert!(
                out.contains("func f()"),
                "n={n}: pseudo must preserve the function; got:\n{out}"
            );
        }
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
}
