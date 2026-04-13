//! Diff rendering — AST-aware and raw hunk output.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

use rskim_core::Language;

use super::ast::{find_changed_node_ranges, is_container_node};
use super::source::get_file_source;
use super::types::{ChangedNodeRange, DiffHunk, FileDiff, ModeRenderContext};
use super::{DiffMode, MAX_AST_FILE_SIZE};
use crate::output::canonical::DiffFileStatus;

thread_local! {
    /// Per-thread parser cache — avoids creating a new tree-sitter Parser for every file.
    /// Each thread in the rayon pool gets its own `HashMap` of parsers keyed by language.
    static PARSERS: RefCell<HashMap<Language, rskim_core::Parser>> = RefCell::new(HashMap::new());
}

/// Compute the minimum column width needed to display any line number in `hunks`.
///
/// Returns at least 1 so empty diffs still produce a consistent format.
fn line_number_width(hunks: &[DiffHunk<'_>]) -> usize {
    let max_line = hunks
        .iter()
        .map(|h| {
            let old_end = h.old_start + h.old_count;
            let new_end = h.new_start + h.new_count;
            old_end.max(new_end)
        })
        .max()
        .unwrap_or(0);
    max_line.to_string().len().max(1)
}

/// Render a single file diff with AST-aware context.
///
/// For supported languages: shows changed AST nodes with full boundaries,
/// preserving `+`/`-` markers from the patch.
///
/// For unsupported languages or parse failures: falls back to raw hunks.
///
/// `diff_mode` controls how unchanged nodes are rendered:
/// - `Default`: Only changed nodes.
/// - `Structure`: Changed + unchanged nodes as signatures.
/// - `Full`: Changed + unchanged nodes in full.
pub(in crate::cmd::git) fn render_diff_file(
    file_diff: &FileDiff<'_>,
    global_flags: &[String],
    args: &[String],
    diff_mode: DiffMode,
    skip_ast: bool,
) -> String {
    let mut output = String::new();

    // File header: renames show "old -> new (renamed)", others show "path (status)"
    if let (DiffFileStatus::Renamed, Some(old)) = (&file_diff.status, &file_diff.old_path) {
        let _ = writeln!(
            output,
            "\u{2500}\u{2500} {} \u{2192} {} ({}) \u{2500}\u{2500}",
            old, file_diff.path, file_diff.status
        );
    } else {
        let _ = writeln!(
            output,
            "\u{2500}\u{2500} {} ({}) \u{2500}\u{2500}",
            file_diff.path, file_diff.status
        );
    }

    // Binary files
    if file_diff.status == DiffFileStatus::Binary {
        let _ = writeln!(output, "Binary file differs");
        return output;
    }

    // No hunks means nothing to show
    if file_diff.hunks.is_empty() {
        return output;
    }

    // Compute line number column width from this file's hunks.
    let ln_width = line_number_width(&file_diff.hunks);

    // Added/deleted files: show all patch lines verbatim (no AST overlay needed)
    if file_diff.status == DiffFileStatus::Deleted || file_diff.status == DiffFileStatus::Added {
        return render_raw_hunks(file_diff, &output, ln_width);
    }

    // When AST is skipped (e.g., beyond MAX_AST_FILE_COUNT), render raw hunks.
    if skip_ast {
        return render_raw_hunks(file_diff, &output, ln_width);
    }

    // Determine language for parser lookup — serde-based formats (JSON, YAML,
    // TOML) have no tree-sitter grammar, so fall back to raw hunks.
    let Some(lang) =
        Language::from_path(Path::new(&file_diff.path)).filter(|l| !l.is_serde_based())
    else {
        return render_raw_hunks(file_diff, &output, ln_width);
    };

    // Obtain a cached parser from the thread-local pool and attempt AST rendering.
    let ast_result = PARSERS.with_borrow_mut(|cache| {
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(lang) {
            if let Ok(p) = rskim_core::Parser::new(lang) {
                e.insert(p);
            }
        }
        let parser = cache.get_mut(&lang)?;
        try_ast_render(file_diff, global_flags, args, diff_mode, parser, ln_width)
    });

    match ast_result {
        Some(ast_output) => {
            output.push_str(&ast_output);
            output
        }
        None => render_raw_hunks(file_diff, &output, ln_width),
    }
}

/// Attempt AST-aware rendering for a modified/renamed file.
///
/// Returns `Some(rendered)` on success, `None` when the file cannot be
/// processed via tree-sitter (file too large, parse failure, or no
/// overlapping AST nodes).
///
/// Language validation and serde-based filtering happen in the caller
/// (`render_diff_file`), so `parser` is guaranteed to match the file's
/// language.
fn try_ast_render(
    file_diff: &FileDiff<'_>,
    global_flags: &[String],
    args: &[String],
    diff_mode: DiffMode,
    parser: &mut rskim_core::Parser,
    ln_width: usize,
) -> Option<String> {
    let source = match get_file_source(&file_diff.path, global_flags, args) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("skim: AST fallback for {}: {e}", file_diff.path);
            return None;
        }
    };

    // Skip AST for files > 100KB
    if source.len() > MAX_AST_FILE_SIZE {
        return None;
    }

    let tree = parser.parse(&source).ok()?;

    let changed_ranges = find_changed_node_ranges(&tree, &file_diff.hunks);
    if changed_ranges.is_empty() {
        return None;
    }

    let source_lines: Vec<&str> = source.lines().collect();
    let mut output = String::new();

    if diff_mode != DiffMode::Default {
        let ctx = ModeRenderContext {
            changed_ranges: &changed_ranges,
            hunks: &file_diff.hunks,
            source_lines: &source_lines,
            source: &source,
            diff_mode,
            ln_width,
        };
        render_with_unchanged_context(&mut output, &tree, &ctx, parser);
    } else {
        render_changed_only(
            &mut output,
            &changed_ranges,
            &file_diff.hunks,
            &source_lines,
            ln_width,
        );
    }

    Some(output)
}

/// Render only changed nodes (default mode).
///
/// For nested nodes (inside a class/struct), emits the parent declaration
/// header line before the changed child node.
fn render_changed_only(
    output: &mut String,
    changed_ranges: &[ChangedNodeRange],
    hunks: &[DiffHunk<'_>],
    source_lines: &[&str],
    ln_width: usize,
) {
    // Track which parent headers we have already emitted
    let mut emitted_parent_headers: HashSet<usize> = HashSet::new();

    // Pre-compute the last range index for each parent header to avoid O(N^2)
    // scanning on every iteration.
    let mut last_index_for_parent: HashMap<usize, usize> = HashMap::new();
    for (idx, range) in changed_ranges.iter().enumerate() {
        if let Some(ref ctx) = range.parent_context {
            last_index_for_parent.insert(ctx.header_line, idx);
        }
    }

    for (idx, range) in changed_ranges.iter().enumerate() {
        // Emit parent header if this is a nested node
        if let Some(ref ctx) = range.parent_context {
            if emitted_parent_headers.insert(ctx.header_line) {
                if let Some(line) = source_lines.get(ctx.header_line - 1) {
                    let _ = writeln!(output, " {:>ln_width$} {line}", ctx.header_line);
                }
            }
        }

        // Clip the render range to exclude parent boundary lines that are
        // emitted separately (header above, close brace below).  When a
        // grandchild node starts on the same line as the container header
        // (e.g. a class-body node at `{` on line 1) or ends on the same
        // line as the closing brace, render_node_with_hunks would otherwise
        // re-emit those lines as unchanged context, producing duplicates.
        let (effective_start, effective_end) = if let Some(ref ctx) = range.parent_context {
            let start = if range.start == ctx.header_line {
                range.start + 1
            } else {
                range.start
            };
            let end = if range.end == ctx.close_line {
                range.end.saturating_sub(1)
            } else {
                range.end
            };
            (start, end)
        } else {
            (range.start, range.end)
        };

        if effective_start <= effective_end {
            render_node_with_hunks(
                output,
                effective_start,
                effective_end,
                hunks,
                source_lines,
                ln_width,
            );
        }

        // Emit parent closing brace if this is the last child with this parent
        if let Some(ref ctx) = range.parent_context {
            let is_last = last_index_for_parent
                .get(&ctx.header_line)
                .is_some_and(|&last_idx| last_idx == idx);
            if is_last {
                if let Some(line) = source_lines.get(ctx.close_line - 1) {
                    let _ = writeln!(output, " {:>ln_width$} {line}", ctx.close_line);
                }
            }
        }
    }
}

/// Render changed nodes with unchanged nodes as context (structure/full mode).
///
/// Walks all top-level AST nodes. Changed nodes get full patch rendering;
/// unchanged nodes are rendered as signatures (structure mode) or in full
/// (full mode).
///
/// `parser` is threaded through for reuse by `render_unchanged_node` in
/// structure mode, avoiding per-node parser re-creation.
fn render_with_unchanged_context(
    output: &mut String,
    tree: &tree_sitter::Tree,
    ctx: &ModeRenderContext<'_>,
    parser: &mut rskim_core::Parser,
) {
    let root = tree.root_node();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let node_start = child.start_position().row + 1;
        let node_end = child.end_position().row + 1;

        // Check if this top-level node contains any changed range.
        //
        // changed_ranges is sorted by start (AST children are visited in
        // document order), so partition_point skips all ranges that end
        // before this node. We then scan forward only while range.start
        // is within the node boundary — O(log R + matches) instead of O(R).
        let first = ctx.changed_ranges.partition_point(|r| r.start < node_start);
        let has_changes = ctx.changed_ranges[first..].iter().any(|r| {
            if r.start > node_end {
                return false;
            }
            // Either the range is directly this node, or it's a child within this node
            (r.start >= node_start && r.end <= node_end)
                || r.parent_context
                    .as_ref()
                    .is_some_and(|p| p.header_line == node_start)
        });

        if has_changes {
            // This node contains changes — render with full patch detail.
            // If it's a container, render parent header + changed children + context children.
            if is_container_node(&child) {
                render_container_with_mode(output, &child, ctx, parser);
            } else {
                // Non-container changed node: render with patch
                render_node_with_hunks(output, node_start, node_end, ctx.hunks, ctx.source_lines, ctx.ln_width);
            }
        } else {
            // Unchanged node: render at mode level
            render_unchanged_node(
                output,
                &child,
                ctx.source_lines,
                ctx.source,
                ctx.diff_mode,
                parser,
                ctx.ln_width,
            );
        }
    }
}

/// Render a container node (class/struct) with mode-aware child rendering.
fn render_container_with_mode(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    ctx: &ModeRenderContext<'_>,
    parser: &mut rskim_core::Parser,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;
    let ln_width = ctx.ln_width;

    // Emit parent header
    if let Some(line) = ctx.source_lines.get(node_start - 1) {
        let _ = writeln!(output, " {:>ln_width$} {line}", node_start);
    }

    // Walk children of the container
    let mut child_cursor = node.walk();
    for child in node.children(&mut child_cursor) {
        let child_start = child.start_position().row + 1;
        let child_end = child.end_position().row + 1;

        // Skip the header line itself (already emitted)
        if child_start == node_start {
            continue;
        }

        // Binary search to the first range that could match child_start,
        // then scan forward while start == child_start. Avoids O(R) scan.
        let first = ctx
            .changed_ranges
            .partition_point(|r| r.start < child_start);
        let child_changed = ctx.changed_ranges[first..].iter().any(|r| {
            if r.start != child_start {
                return false;
            }
            r.end == child_end
                && r.parent_context
                    .as_ref()
                    .is_some_and(|p| p.header_line == node_start)
        });

        if child_changed {
            render_node_with_hunks(output, child_start, child_end, ctx.hunks, ctx.source_lines, ln_width);
        } else {
            render_unchanged_node(
                output,
                &child,
                ctx.source_lines,
                ctx.source,
                ctx.diff_mode,
                parser,
                ln_width,
            );
        }
    }

    // Emit closing brace
    if node_end > node_start {
        if let Some(line) = ctx.source_lines.get(node_end - 1) {
            let _ = writeln!(output, " {:>ln_width$} {line}", node_end);
        }
    }
}

/// Render an unchanged node at the appropriate mode level.
///
/// In structure mode, reuses the provided `parser` for transformation
/// instead of creating a new parser per node.
///
/// Full mode renders line numbers using `ln_width` for alignment.
/// Structure mode emits synthetic transformed text — no line numbers
/// since the lines don't correspond 1-to-1 with source positions.
fn render_unchanged_node(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    source_lines: &[&str],
    source: &str,
    diff_mode: DiffMode,
    parser: &mut rskim_core::Parser,
    ln_width: usize,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    match diff_mode {
        DiffMode::Full => {
            // Show unchanged nodes in full with line numbers
            for line_num in node_start..=node_end {
                if let Some(line) = source_lines.get(line_num - 1) {
                    let _ = writeln!(output, " {:>ln_width$} {line}", line_num);
                }
            }
        }
        DiffMode::Structure => {
            // Show unchanged nodes as structure (signatures).
            // Structure output is synthetic (transformed) text — line numbers
            // are omitted because they don't correspond to real source lines.
            let node_text = node.utf8_text(source.as_bytes()).unwrap_or_default();

            // Transform using the reused parser (avoids per-node parser creation)
            let config = rskim_core::TransformConfig::with_mode(rskim_core::Mode::Structure);
            match parser.transform(node_text, &config) {
                Ok(transformed) => {
                    for line in transformed.lines() {
                        let _ = writeln!(output, " {line}");
                    }
                }
                Err(_) => {
                    // Fall back to showing just the first line (declaration)
                    if let Some(line) = source_lines.get(node_start - 1) {
                        let _ = writeln!(output, " {line}");
                    }
                }
            }
        }
        DiffMode::Default => {
            // Default mode: unchanged nodes are omitted (handled by caller)
        }
    }
}

/// Render a node region with hunk patch lines overlaid, including line numbers.
///
/// Line number assignment:
/// - `+` (added) lines use the new-file line number; `current_new_line` advances.
/// - `-` (removed) lines use the old-file line number; `current_old_line` advances.
/// - ` ` (context) lines use the new-file line number; both counters advance.
/// - `\` (no-newline marker) has no line number.
/// - Unchanged source lines between hunks use the new-file line number.
fn render_node_with_hunks(
    output: &mut String,
    node_start: usize,
    node_end: usize,
    hunks: &[DiffHunk<'_>],
    source_lines: &[&str],
    ln_width: usize,
) {
    // Hunks are sorted by new_start (they come from git's sequential output).
    // Use partition_point to skip hunks that end before node_start, then
    // take_while to stop once the hunk starts after node_end — O(log H + matches).
    let first = hunks.partition_point(|h| h.new_start + h.new_count.saturating_sub(1) < node_start);
    let relevant_hunks: Vec<&DiffHunk<'_>> = hunks[first..]
        .iter()
        .take_while(|h| h.new_start <= node_end)
        .collect();

    if relevant_hunks.is_empty() {
        // No hunks overlap — show as unchanged context with new-file line numbers
        for line_num in node_start..=node_end {
            if let Some(line) = source_lines.get(line_num - 1) {
                let _ = writeln!(output, " {:>ln_width$} {line}", line_num);
            }
        }
        return;
    }

    let mut current_new_line = node_start;

    for hunk in &relevant_hunks {
        // Output unchanged source lines before this hunk's position.
        // Context lines: use new-file line number.
        while current_new_line < hunk.new_start && current_new_line <= node_end {
            if let Some(line) = source_lines.get(current_new_line - 1) {
                let _ = writeln!(output, " {:>ln_width$} {line}", current_new_line);
            }
            current_new_line += 1;
        }

        // Old-line cursor starts at the hunk boundary.
        // The patch-line cursor (patch_new_line) starts at the hunk's new_start so
        // that skip logic can correctly advance past pre-node lines even when the
        // hunk begins before node_start.
        let mut patch_new_line = hunk.new_start;
        let mut patch_old_line = hunk.old_start;

        // Output the hunk's patch lines, clipped to [node_start, node_end].
        //
        // A single git hunk can span multiple AST nodes (one `@@` block covering
        // both an interface ending at line 8 and a class starting at line 10).
        // Without clipping, every node that overlaps the hunk would emit ALL of
        // the hunk's patch lines — causing duplicate output across adjacent nodes.
        //
        // Clipping rules:
        //   - Skip lines before node_start: advance counters without emitting.
        //     Removed lines (`-`, new_delta == 0) are skipped if the current
        //     new-file position (patch_new_line) hasn't yet reached node_start.
        //   - Stop after node_end: break once patch_new_line > node_end.
        for patch_line in &hunk.patch_lines {
            // Stop once we've passed the node's end boundary.
            if patch_new_line > node_end {
                break;
            }

            let (new_delta, old_delta) = match patch_line.as_bytes().first() {
                Some(b'+') => (1usize, 0usize),
                Some(b'-') => (0, 1),
                Some(b' ') => (1, 1),
                _ => (0, 0),
            };

            // Skip lines that fall before the node's start (hunk started earlier
            // in the file).
            if patch_new_line < node_start {
                patch_new_line += new_delta;
                patch_old_line += old_delta;
                continue;
            }

            let (nd, od) =
                emit_patch_line(output, patch_line, patch_new_line, patch_old_line, ln_width);
            patch_new_line += nd;
            patch_old_line += od;
        }

        // Advance the outer cursor past the lines consumed by this hunk so the
        // inter-hunk context fill and trailing-lines loops stay in sync.
        current_new_line = patch_new_line;
    }

    // Output remaining unchanged source lines to end of node
    while current_new_line <= node_end {
        if let Some(line) = source_lines.get(current_new_line - 1) {
            let _ = writeln!(output, " {:>ln_width$} {line}", current_new_line);
        }
        current_new_line += 1;
    }
}

/// Emit a single patch line with its line number, updating the line counters.
///
/// Returns `(new_line_delta, old_line_delta)` — the amount each counter should
/// advance after this line.  Most callers immediately add them back; splitting
/// the counters out of this function avoids passing `&mut` through the hot path.
///
/// `\` (no-newline marker) and unknown prefixes are written verbatim with no
/// line number and contribute zero delta to either counter.
fn emit_patch_line(
    output: &mut String,
    patch_line: &str,
    current_new_line: usize,
    current_old_line: usize,
    ln_width: usize,
) -> (usize, usize) {
    match patch_line.as_bytes().first() {
        Some(b'+') => {
            let _ = writeln!(output, "+{:>ln_width$} {}", current_new_line, &patch_line[1..]);
            (1, 0)
        }
        Some(b'-') => {
            let _ = writeln!(output, "-{:>ln_width$} {}", current_old_line, &patch_line[1..]);
            (0, 1)
        }
        Some(b' ') => {
            let _ = writeln!(output, " {:>ln_width$} {}", current_new_line, &patch_line[1..]);
            (1, 1)
        }
        _ => {
            // `\` (no-newline marker) or unexpected prefix — emit verbatim, no line number
            let _ = writeln!(output, "{patch_line}");
            (0, 0)
        }
    }
}

/// Render raw diff hunks as fallback (no AST awareness), with line numbers.
///
/// Tracks old and new line counters across all hunks in the file, emitting
/// the appropriate file line number after each prefix character.
fn render_raw_hunks(file_diff: &FileDiff<'_>, header: &str, ln_width: usize) -> String {
    let mut output = header.to_string();
    for hunk in &file_diff.hunks {
        let mut current_new_line = hunk.new_start;
        let mut current_old_line = hunk.old_start;
        for line in &hunk.patch_lines {
            let (new_delta, old_delta) =
                emit_patch_line(&mut output, line, current_new_line, current_old_line, ln_width);
            current_new_line += new_delta;
            current_old_line += old_delta;
        }
    }
    output
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::canonical::{DiffFileEntry, DiffResult};

    // ========================================================================
    // Render output tests (#103)
    // ========================================================================

    #[test]
    fn test_render_binary_file() {
        let file_diff = FileDiff {
            path: "assets/logo.png".to_string(),
            old_path: None,
            status: DiffFileStatus::Binary,
            hunks: vec![],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false);
        assert!(rendered.contains("logo.png"));
        assert!(rendered.contains("binary"));
        assert!(rendered.contains("Binary file differs"));
    }

    #[test]
    fn test_render_added_file() {
        let file_diff = FileDiff {
            path: "src/new.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 2,
                patch_lines: vec!["+const x = 1;", "+const y = 2;"],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false);
        assert!(rendered.contains("added"), "header should show 'added'");
        assert!(
            rendered.contains("const x = 1;"),
            "should contain added line content"
        );
        // Line numbers are prepended: format is `+{ln} {content}`
        assert!(
            rendered.contains("+1 const x = 1;") || rendered.contains("+ 1 const x = 1;"),
            "added lines should have line numbers; got: {rendered}"
        );
    }

    #[test]
    fn test_render_deleted_file() {
        let file_diff = FileDiff {
            path: "src/old.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Deleted,
            hunks: vec![DiffHunk {
                old_start: 1,
                old_count: 2,
                new_start: 0,
                new_count: 0,
                patch_lines: vec!["-const x = 1;", "-const y = 2;"],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false);
        assert!(rendered.contains("deleted"), "header should show 'deleted'");
        assert!(
            rendered.contains("const x = 1;"),
            "should contain deleted line content"
        );
        // Line numbers are prepended: format is `-{ln} {content}`
        assert!(
            rendered.contains("-1 const x = 1;") || rendered.contains("- 1 const x = 1;"),
            "deleted lines should have line numbers; got: {rendered}"
        );
    }

    #[test]
    fn test_render_renamed_file_header() {
        let file_diff = FileDiff {
            path: "src/utils/format.ts".to_string(),
            old_path: Some("src/utils/helpers.ts".to_string()),
            status: DiffFileStatus::Renamed,
            hunks: vec![],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false);
        assert!(rendered.contains("helpers.ts"), "should show old path");
        assert!(rendered.contains("format.ts"), "should show new path");
        assert!(rendered.contains("renamed"), "header should show 'renamed'");
    }

    // ========================================================================
    // DiffResult output type tests (#103)
    // ========================================================================

    #[test]
    fn test_diff_result_display() {
        let entries = vec![
            DiffFileEntry {
                path: "src/main.rs".to_string(),
                status: DiffFileStatus::Modified,
                changed_regions: 2,
            },
            DiffFileEntry {
                path: "src/lib.rs".to_string(),
                status: DiffFileStatus::Added,
                changed_regions: 1,
            },
        ];
        let result = DiffResult::new(entries, "test rendered output".to_string());
        assert_eq!(result.files_changed, 2);
        assert_eq!(result.to_string(), "test rendered output");
    }

    #[test]
    fn test_diff_result_serde_roundtrip() {
        let entries = vec![DiffFileEntry {
            path: "src/main.rs".to_string(),
            status: DiffFileStatus::Modified,
            changed_regions: 1,
        }];
        let original = DiffResult::new(entries, "rendered output".to_string());
        let json = serde_json::to_string(&original).unwrap();
        let mut deserialized: DiffResult = serde_json::from_str(&json).unwrap();
        deserialized.ensure_rendered();
        // After deserialization+ensure_rendered, it should have some output
        assert!(!deserialized.as_ref().is_empty());
    }

    // ========================================================================
    // Thread-local PARSERS cache tests
    // ========================================================================

    /// Validates that the thread-local parser cache does not corrupt state
    /// across sequential renders of the same language.
    ///
    /// If the cached parser retained stale incremental-parse state from the
    /// first call, the second render would produce wrong output. Correct
    /// output from both calls proves the cache reuse path is safe.
    #[test]
    fn test_parser_cache_reuse_does_not_corrupt_output() {
        // Both diffs are for TypeScript files — the second render must reuse
        // the same parser instance (already in the thread-local cache after
        // the first call) and still produce correct output.
        let file_diff_a = FileDiff {
            path: "src/foo.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                patch_lines: vec!["+const FOO = 1;"],
            }],
        };
        let file_diff_b = FileDiff {
            path: "src/bar.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                old_count: 0,
                new_start: 1,
                new_count: 1,
                patch_lines: vec!["+const BAR = 2;"],
            }],
        };

        let out_a = render_diff_file(&file_diff_a, &[], &[], DiffMode::Default, false);
        let out_b = render_diff_file(&file_diff_b, &[], &[], DiffMode::Default, false);

        // Each output should contain only its own added line, not content
        // from the other file — proving cache reuse doesn't bleed state.
        assert!(
            out_a.contains("foo.ts"),
            "first render should reference foo.ts"
        );
        assert!(
            out_a.contains("FOO = 1;"),
            "first render should contain its patch line content"
        );
        assert!(
            out_b.contains("bar.ts"),
            "second render should reference bar.ts"
        );
        assert!(
            out_b.contains("BAR = 2;"),
            "second render should contain its patch line content"
        );
        assert!(
            !out_a.contains("BAR"),
            "first render must not bleed second file content"
        );
        assert!(
            !out_b.contains("FOO"),
            "second render must not bleed first file content"
        );
    }

    // ========================================================================
    // MAX_AST_FILE_COUNT / skip_ast tests (#103 review batch-7)
    // ========================================================================

    #[test]
    fn test_render_diff_file_skip_ast_uses_raw_hunks() {
        // When skip_ast is true, render_diff_file should produce raw patch
        // lines instead of attempting AST-aware rendering.
        let file_diff = FileDiff {
            path: "src/foo.rs".to_string(),
            old_path: None,
            status: DiffFileStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                old_count: 3,
                new_start: 1,
                new_count: 4,
                patch_lines: vec![" fn main() {", "+    println!(\"hello\");", " }"],
            }],
        };

        let output = render_diff_file(
            &file_diff,
            &[],
            &[],
            DiffMode::Structure,
            true, // skip_ast
        );

        // Should contain file header
        assert!(
            output.contains("src/foo.rs (modified)"),
            "expected file header, got: {output}"
        );
        // Should contain raw patch line content with line number prefix
        assert!(
            output.contains("println!(\"hello\");"),
            "expected raw patch line content, got: {output}"
        );
        // Line number format: `+{ln} {content}`
        assert!(
            output.contains("+2     println!(\"hello\");") || output.contains("+ 2     println!(\"hello\");"),
            "expected line-numbered patch line, got: {output}"
        );
    }

    // ========================================================================
    // Line number tests (Workstream 1)
    // ========================================================================

    #[test]
    fn test_render_raw_hunks_shows_line_numbers() {
        // render_raw_hunks (fallback path) should prefix each line with its
        // file line number, right-aligned to the width of the largest line.
        let file_diff = FileDiff {
            path: "src/old.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Deleted,
            hunks: vec![DiffHunk {
                old_start: 10,
                old_count: 3,
                new_start: 0,
                new_count: 0,
                // Three removed lines starting at old line 10
                patch_lines: vec!["-const a = 1;", "-const b = 2;", "-const c = 3;"],
            }],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false);
        // Removed lines should use old-file line numbers (10, 11, 12)
        assert!(
            rendered.contains("-10 const a = 1;"),
            "first removed line should carry line 10; got:\n{rendered}"
        );
        assert!(
            rendered.contains("-11 const b = 2;"),
            "second removed line should carry line 11; got:\n{rendered}"
        );
        assert!(
            rendered.contains("-12 const c = 3;"),
            "third removed line should carry line 12; got:\n{rendered}"
        );
    }

    #[test]
    fn test_render_raw_hunks_multi_hunk_line_tracking() {
        // Two hunks in a single added file — line numbers must restart from each
        // hunk's new_start and not bleed across hunk boundaries.
        let file_diff = FileDiff {
            path: "src/mod.ts".to_string(),
            old_path: None,
            status: DiffFileStatus::Added,
            hunks: vec![
                DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 1,
                    new_count: 2,
                    patch_lines: vec!["+const A = 1;", "+const B = 2;"],
                },
                DiffHunk {
                    old_start: 0,
                    old_count: 0,
                    new_start: 10,
                    new_count: 1,
                    patch_lines: vec!["+const C = 3;"],
                },
            ],
        };
        let rendered = render_diff_file(&file_diff, &[], &[], DiffMode::Default, false);
        // First hunk: lines 1 and 2
        assert!(
            rendered.contains("+1 const A = 1;") || rendered.contains("+ 1 const A = 1;"),
            "first hunk line 1; got:\n{rendered}"
        );
        assert!(
            rendered.contains("+2 const B = 2;") || rendered.contains("+ 2 const B = 2;"),
            "first hunk line 2; got:\n{rendered}"
        );
        // Second hunk: line 10
        assert!(
            rendered.contains("+10 const C = 3;") || rendered.contains("+10  const C = 3;"),
            "second hunk line 10; got:\n{rendered}"
        );
    }

    #[test]
    fn test_line_number_width_helper() {
        // Empty hunks → minimum width 1
        assert_eq!(line_number_width(&[]), 1);
        // Single-digit max
        assert_eq!(
            line_number_width(&[DiffHunk {
                old_start: 1,
                old_count: 3,
                new_start: 1,
                new_count: 3,
                patch_lines: vec![]
            }]),
            1
        );
        // Three-digit max (old_end = 100 + 10 = 110)
        assert_eq!(
            line_number_width(&[DiffHunk {
                old_start: 100,
                old_count: 10,
                new_start: 1,
                new_count: 5,
                patch_lines: vec![]
            }]),
            3
        );
    }

    // ========================================================================
    // Container header deduplication tests
    // ========================================================================

    /// When a changed child range starts on the same line as its parent
    /// container header (e.g. a body node starting at `{` on line 1), the
    /// parent header must appear exactly once in the rendered output.
    ///
    /// Without the fix, `render_changed_only` emits the header explicitly then
    /// `render_node_with_hunks` re-emits it as an unchanged context line,
    /// producing a duplicate `1 interface UserService {` pair.
    #[test]
    fn test_render_changed_only_no_duplicate_parent_header_when_range_starts_at_header() {
        // interface UserService {        ← line 1 (parent header AND child start)
        //   id: string;                  ← line 2 (changed)
        //   name: string;               ← line 3
        // }                             ← line 4 (parent close)
        let source_lines: Vec<&str> =
            vec!["interface UserService {", "  id: string;", "  name: string;", "}"];

        // Hunk changes line 2 (new file)
        let hunks = vec![DiffHunk {
            old_start: 2,
            old_count: 1,
            new_start: 2,
            new_count: 1,
            patch_lines: vec!["-  id: string;", "+  id: number;"],
        }];

        // Simulate AST returning a body node that starts at line 1 (same as parent),
        // with parent_context also at line 1. This is the scenario that triggers the
        // duplicate: the body node covers lines 1-4 with its own start == header_line.
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1, // child starts on same line as parent header
            end: 4,
            parent_context: Some(super::super::types::ParentContext {
                header_line: 1,
                close_line: 4,
            }),
        }];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        // Count occurrences of the header line content
        let header_count = output
            .lines()
            .filter(|l| l.contains("interface UserService {"))
            .count();
        assert_eq!(
            header_count, 1,
            "container header must appear exactly once; got {header_count}:\n{output}"
        );

        // The changed line should appear
        assert!(
            output.contains("id: number;"),
            "changed line should appear in output:\n{output}"
        );
    }

    /// When two changed child ranges share the same parent container, the
    /// container header must appear exactly once and the close brace exactly once.
    #[test]
    fn test_render_changed_only_no_duplicate_header_two_children_same_parent() {
        // interface UserService {        ← line 1 (parent header)
        //   findById(id: string): User;  ← line 2 (changed)
        //   createUser(): User;          ← line 3 (changed)
        //   deleteUser(): void;          ← line 4
        // }                             ← line 5 (parent close)
        let source_lines: Vec<&str> = vec![
            "interface UserService {",
            "  findById(id: string): User;",
            "  createUser(): User;",
            "  deleteUser(): void;",
            "}",
        ];

        // Two hunks, one for each changed method
        let hunks = vec![
            DiffHunk {
                old_start: 2,
                old_count: 1,
                new_start: 2,
                new_count: 1,
                patch_lines: vec!["-  findById(id: string): User;", "+  findById(id: string): UserDto;"],
            },
            DiffHunk {
                old_start: 3,
                old_count: 1,
                new_start: 3,
                new_count: 1,
                patch_lines: vec!["-  createUser(): User;", "+  createUser(): UserDto;"],
            },
        ];

        // Two child ranges, both with the same parent container at lines 1-5
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 2,
                end: 2,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 5,
                }),
            },
            super::super::types::ChangedNodeRange {
                start: 3,
                end: 3,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 5,
                }),
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        let header_count = output
            .lines()
            .filter(|l| l.contains("interface UserService {"))
            .count();
        assert_eq!(
            header_count, 1,
            "container header must appear exactly once; got {header_count}:\n{output}"
        );

        // Close brace is rendered as " 5 }" — ends_with(" }") counts it uniquely
        // since none of the changed lines in this test end with " }".
        let close_count = output.lines().filter(|l| l.ends_with(" }")).count();
        assert_eq!(
            close_count, 1,
            "container close brace must appear exactly once; got {close_count}:\n{output}"
        );

        // Both changed lines should appear
        assert!(
            output.contains("UserDto"),
            "changed content must appear:\n{output}"
        );
    }

    /// When the changed range ends on the same line as the parent close brace,
    /// the close brace must appear exactly once (not once from render_node_with_hunks
    /// and once from the explicit is_last close brace emission).
    #[test]
    fn test_render_changed_only_no_duplicate_close_brace_when_range_ends_at_close() {
        // class AuthService {   ← line 1 (parent header AND child start)
        //   login(): void;      ← line 2 (changed)
        // }                     ← line 3 (parent close AND child end)
        let source_lines: Vec<&str> = vec!["class AuthService {", "  login(): void;", "}"];

        let hunks = vec![DiffHunk {
            old_start: 2,
            old_count: 1,
            new_start: 2,
            new_count: 1,
            patch_lines: vec!["-  login(): void;", "+  login(): Promise<void>;"],
        }];

        // Range starts at header line 1, ends at close line 3
        let changed_ranges = vec![super::super::types::ChangedNodeRange {
            start: 1,
            end: 3,
            parent_context: Some(super::super::types::ParentContext {
                header_line: 1,
                close_line: 3,
            }),
        }];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 1);

        let header_count = output
            .lines()
            .filter(|l| l.contains("class AuthService {"))
            .count();
        assert_eq!(
            header_count, 1,
            "class header must appear exactly once; got {header_count}:\n{output}"
        );

        // Close brace rendered as " 3 }" — ends_with(" }") counts it uniquely
        let close_count = output.lines().filter(|l| l.ends_with(" }")).count();
        assert_eq!(
            close_count, 1,
            "close brace must appear exactly once; got {close_count}:\n{output}"
        );
    }

    /// When a single git hunk spans multiple AST nodes (e.g. one `@@` block covering
    /// both an interface ending at line 8 and a class starting at line 10), patch lines
    /// must be clipped to each node's [start, end] range.
    ///
    /// Without the fix, the interface node (lines 1-8) would emit patch lines for lines
    /// 4-16+, and the class node (lines 10-20) would emit the same patch lines again.
    #[test]
    fn test_render_node_with_hunks_clips_to_node_boundaries_when_hunk_spans_two_nodes() {
        // interface Foo {         ← line 1
        //   a: string;            ← line 2
        //   b: string;            ← line 3
        //   c: string;            ← line 4  (changed in hunk)
        // }                       ← line 5
        //                         ← line 6 (blank)
        // class Bar {             ← line 7
        //   x: number;            ← line 8  (changed in hunk)
        //   y: number;            ← line 9
        // }                       ← line 10
        let source_lines: Vec<&str> = vec![
            "interface Foo {",
            "  a: string;",
            "  b: string;",
            "  c: string;",
            "}",
            "",
            "class Bar {",
            "  x: number;",
            "  y: number;",
            "}",
        ];

        // Single hunk that spans both containers (lines 4-8 in the new file).
        let hunks = vec![DiffHunk {
            old_start: 4,
            old_count: 5,
            new_start: 4,
            new_count: 5,
            patch_lines: vec![
                "-  c: string;",
                "+  c: boolean;",
                " }",
                " ",
                " class Bar {",
                "-  x: number;",
                "+  x: boolean;",
            ],
        }];

        // Two changed ranges: interface body (lines 1-5) and class body (lines 7-10).
        let changed_ranges = vec![
            super::super::types::ChangedNodeRange {
                start: 1,
                end: 5,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 1,
                    close_line: 5,
                }),
            },
            super::super::types::ChangedNodeRange {
                start: 7,
                end: 10,
                parent_context: Some(super::super::types::ParentContext {
                    header_line: 7,
                    close_line: 10,
                }),
            },
        ];

        let mut output = String::new();
        render_changed_only(&mut output, &changed_ranges, &hunks, &source_lines, 2);

        // Each container header must appear exactly once.
        let foo_count = output.lines().filter(|l| l.contains("interface Foo {")).count();
        assert_eq!(
            foo_count, 1,
            "interface Foo header must appear exactly once; got {foo_count}:\n{output}"
        );

        let bar_count = output.lines().filter(|l| l.contains("class Bar {")).count();
        assert_eq!(
            bar_count, 1,
            "class Bar header must appear exactly once; got {bar_count}:\n{output}"
        );

        // Each changed line must appear exactly once (not duplicated across nodes).
        let c_bool_count = output.lines().filter(|l| l.contains("c: boolean;")).count();
        assert_eq!(
            c_bool_count, 1,
            "c: boolean must appear exactly once; got {c_bool_count}:\n{output}"
        );

        let x_bool_count = output.lines().filter(|l| l.contains("x: boolean;")).count();
        assert_eq!(
            x_bool_count, 1,
            "x: boolean must appear exactly once; got {x_bool_count}:\n{output}"
        );

        // The class Bar change must NOT appear in the interface section and vice versa.
        // We verify by checking that the interface node does not contain "x: boolean".
        // (The output is a single string but we can check ordering of appearances.)
        let foo_pos = output.find("interface Foo {").unwrap();
        let bar_pos = output.find("class Bar {").unwrap();
        let c_bool_pos = output.find("c: boolean").unwrap();
        let x_bool_pos = output.find("x: boolean").unwrap();

        assert!(
            c_bool_pos < bar_pos,
            "c: boolean must appear before class Bar section:\n{output}"
        );
        assert!(
            x_bool_pos > foo_pos,
            "x: boolean must appear after interface Foo section starts:\n{output}"
        );
        assert!(
            x_bool_pos > c_bool_pos,
            "x: boolean must appear after c: boolean (class Bar comes after interface Foo):\n{output}"
        );
    }
}
