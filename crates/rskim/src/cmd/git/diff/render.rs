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
pub(super) fn render_diff_file(
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

    // Added/deleted files: show all patch lines verbatim (no AST overlay needed)
    if file_diff.status == DiffFileStatus::Deleted || file_diff.status == DiffFileStatus::Added {
        return render_raw_hunks(file_diff, &output);
    }

    // When AST is skipped (e.g., beyond MAX_AST_FILE_COUNT), render raw hunks.
    if skip_ast {
        return render_raw_hunks(file_diff, &output);
    }

    // Determine language for parser lookup
    let lang = Language::from_path(Path::new(&file_diff.path));
    let can_ast = lang.is_some_and(|l| !l.is_serde_based());

    if !can_ast {
        return render_raw_hunks(file_diff, &output);
    }

    let lang = lang.expect("checked above");

    // Obtain a cached parser from the thread-local pool and attempt AST rendering.
    let ast_result = PARSERS.with_borrow_mut(|cache| {
        if let std::collections::hash_map::Entry::Vacant(e) = cache.entry(lang) {
            if let Ok(p) = rskim_core::Parser::new(lang) {
                e.insert(p);
            }
        }
        if let Some(parser) = cache.get_mut(&lang) {
            try_ast_render(file_diff, global_flags, args, diff_mode, parser)
        } else {
            None
        }
    });

    if let Some(ast_output) = ast_result {
        output.push_str(&ast_output);
    } else {
        return render_raw_hunks(file_diff, &output);
    }

    output
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
        };
        render_with_unchanged_context(&mut output, &tree, &ctx, parser);
    } else {
        render_changed_only(
            &mut output,
            &changed_ranges,
            &file_diff.hunks,
            &source_lines,
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
                    let _ = writeln!(output, " {line}");
                }
            }
        }

        render_node_with_hunks(output, range.start, range.end, hunks, source_lines);

        // Emit parent closing brace if this is the last child with this parent
        if let Some(ref ctx) = range.parent_context {
            let is_last = last_index_for_parent
                .get(&ctx.header_line)
                .is_some_and(|&last_idx| last_idx == idx);
            if is_last {
                if let Some(line) = source_lines.get(ctx.close_line - 1) {
                    let _ = writeln!(output, " {line}");
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

        // Check if this top-level node contains any changed range
        let has_changes = ctx.changed_ranges.iter().any(|r| {
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
                render_node_with_hunks(output, node_start, node_end, ctx.hunks, ctx.source_lines);
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

    // Emit parent header
    if let Some(line) = ctx.source_lines.get(node_start - 1) {
        let _ = writeln!(output, " {line}");
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

        let child_changed = ctx.changed_ranges.iter().any(|r| {
            r.start == child_start
                && r.end == child_end
                && r.parent_context
                    .as_ref()
                    .is_some_and(|p| p.header_line == node_start)
        });

        if child_changed {
            render_node_with_hunks(output, child_start, child_end, ctx.hunks, ctx.source_lines);
        } else {
            render_unchanged_node(
                output,
                &child,
                ctx.source_lines,
                ctx.source,
                ctx.diff_mode,
                parser,
            );
        }
    }

    // Emit closing brace
    if node_end > node_start {
        if let Some(line) = ctx.source_lines.get(node_end - 1) {
            let _ = writeln!(output, " {line}");
        }
    }
}

/// Render an unchanged node at the appropriate mode level.
///
/// In structure mode, reuses the provided `parser` for transformation
/// instead of creating a new parser per node.
fn render_unchanged_node(
    output: &mut String,
    node: &tree_sitter::Node<'_>,
    source_lines: &[&str],
    source: &str,
    diff_mode: DiffMode,
    parser: &mut rskim_core::Parser,
) {
    let node_start = node.start_position().row + 1;
    let node_end = node.end_position().row + 1;

    match diff_mode {
        DiffMode::Full => {
            // Show unchanged nodes in full
            for line_num in node_start..=node_end {
                if let Some(line) = source_lines.get(line_num - 1) {
                    let _ = writeln!(output, " {line}");
                }
            }
        }
        DiffMode::Structure => {
            // Show unchanged nodes as structure (signatures)
            let node_text = node.utf8_text(source.as_bytes()).unwrap_or("");

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

/// Render a node region with hunk patch lines overlaid.
fn render_node_with_hunks(
    output: &mut String,
    node_start: usize,
    node_end: usize,
    hunks: &[DiffHunk<'_>],
    source_lines: &[&str],
) {
    let relevant_hunks: Vec<&DiffHunk<'_>> = hunks
        .iter()
        .filter(|h| {
            let hunk_start = h.new_start;
            let hunk_end = h.new_start + h.new_count.saturating_sub(1);
            hunk_start <= node_end && hunk_end >= node_start
        })
        .collect();

    if relevant_hunks.is_empty() {
        // No hunks overlap — show as unchanged context
        for line_num in node_start..=node_end {
            if let Some(line) = source_lines.get(line_num - 1) {
                let _ = writeln!(output, " {line}");
            }
        }
        return;
    }

    let mut current_new_line = node_start;

    for hunk in &relevant_hunks {
        // Output unchanged source lines before this hunk's position
        while current_new_line < hunk.new_start && current_new_line <= node_end {
            if let Some(line) = source_lines.get(current_new_line - 1) {
                let _ = writeln!(output, " {line}");
            }
            current_new_line += 1;
        }

        // Output the hunk's patch lines
        for patch_line in &hunk.patch_lines {
            match patch_line.as_bytes().first() {
                Some(b'+') => {
                    let _ = writeln!(output, "{patch_line}");
                    current_new_line += 1;
                }
                Some(b' ') => {
                    let _ = writeln!(output, "{patch_line}");
                    current_new_line += 1;
                }
                Some(b'-' | b'\\') => {
                    let _ = writeln!(output, "{patch_line}");
                }
                _ => {}
            }
        }
    }

    // Output remaining unchanged source lines to end of node
    while current_new_line <= node_end {
        if let Some(line) = source_lines.get(current_new_line - 1) {
            let _ = writeln!(output, " {line}");
        }
        current_new_line += 1;
    }
}

/// Render raw diff hunks as fallback (no AST awareness).
fn render_raw_hunks(file_diff: &FileDiff<'_>, header: &str) -> String {
    let mut output = header.to_string();
    for hunk in &file_diff.hunks {
        for line in &hunk.patch_lines {
            let _ = writeln!(output, "{line}");
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
    use super::super::types::DiffHunk;
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
            rendered.contains("+const x = 1;"),
            "should contain added lines"
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
            rendered.contains("-const x = 1;"),
            "should contain deleted lines"
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
                patch_lines: vec![
                    " fn main() {",
                    "+    println!(\"hello\");",
                    " }",
                ],
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
        // Should contain raw patch lines (not AST-processed)
        assert!(
            output.contains("+    println!(\"hello\");"),
            "expected raw patch line, got: {output}"
        );
    }
}
