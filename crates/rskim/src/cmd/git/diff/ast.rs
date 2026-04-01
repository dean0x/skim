//! AST node detection — mapping changed lines to tree-sitter node ranges.

use std::collections::BTreeSet;

use super::types::{ChangedNodeRange, DiffHunk, ParentContext};

/// Build the set of changed line numbers from diff hunks.
///
/// Returns 1-indexed line numbers using new-file positions.
pub(super) fn build_changed_lines(hunks: &[DiffHunk<'_>]) -> BTreeSet<usize> {
    let mut changed_lines: BTreeSet<usize> = BTreeSet::new();
    for hunk in hunks {
        let mut new_line = hunk.new_start;
        for patch_line in &hunk.patch_lines {
            match patch_line.as_bytes().first() {
                Some(b'+') => {
                    changed_lines.insert(new_line);
                    new_line += 1;
                }
                Some(b'-') => {
                    // Removed lines exist in old file -- mark the current
                    // new-file position as a change boundary so the
                    // surrounding AST node is included. Trailing deletions
                    // at EOF may push `new_line` beyond the actual file
                    // length; this is harmless because the downstream
                    // `changed_lines.range(node_start..=node_end)` check
                    // will never match an out-of-range value against a
                    // real node.
                    changed_lines.insert(new_line);
                }
                Some(b' ') => {
                    new_line += 1;
                }
                _ => {} // Skip lines starting with '\' or other
            }
        }
    }
    changed_lines
}

/// Check whether a node is a container (class, struct, impl, module).
pub(super) fn is_container_node(node: &tree_sitter::Node<'_>) -> bool {
    let kind = node.kind();
    matches!(
        kind,
        "class_declaration"
            | "class_definition"          // Python
            | "class"
            | "struct_item"               // Rust
            | "impl_item"                 // Rust
            | "enum_item"                 // Rust
            | "trait_item"                // Rust
            | "interface_declaration"     // TypeScript
            | "module"
            | "namespace_definition" // C++
    )
}

/// Find which AST nodes overlap with changed line ranges from hunks.
///
/// Performs one level of nesting: if a top-level container node (class/struct/impl)
/// overlaps with hunks, walks its children to find the specific changed child
/// nodes. Returns child-level ranges with parent context instead of the entire
/// parent range.
///
/// Lines are 1-indexed to match diff output.
pub(super) fn find_changed_node_ranges(
    tree: &tree_sitter::Tree,
    hunks: &[DiffHunk<'_>],
) -> Vec<ChangedNodeRange> {
    if hunks.is_empty() {
        return Vec::new();
    }

    let changed_lines = build_changed_lines(hunks);

    if changed_lines.is_empty() {
        return Vec::new();
    }

    let root = tree.root_node();
    let mut ranges: Vec<ChangedNodeRange> = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let node_start = child.start_position().row + 1;
        let node_end = child.end_position().row + 1;

        let overlaps = changed_lines.range(node_start..=node_end).next().is_some();

        if !overlaps {
            continue;
        }

        // If this is a container node, try to narrow down to child methods/fields
        if is_container_node(&child) {
            let mut child_cursor = child.walk();
            let mut found_child = false;

            for grandchild in child.children(&mut child_cursor) {
                let gc_start = grandchild.start_position().row + 1;
                let gc_end = grandchild.end_position().row + 1;

                let gc_overlaps = changed_lines.range(gc_start..=gc_end).next().is_some();

                if gc_overlaps {
                    found_child = true;
                    ranges.push(ChangedNodeRange {
                        start: gc_start,
                        end: gc_end,
                        parent_context: Some(ParentContext {
                            header_line: node_start,
                            close_line: node_end,
                        }),
                    });
                }
            }

            // If no child matched (change is in parent's direct body), use the whole parent
            if !found_child {
                ranges.push(ChangedNodeRange {
                    start: node_start,
                    end: node_end,
                    parent_context: None,
                });
            }
        } else {
            ranges.push(ChangedNodeRange {
                start: node_start,
                end: node_end,
                parent_context: None,
            });
        }
    }

    ranges
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // build_changed_lines unit tests (#103)
    // ========================================================================

    #[test]
    fn test_build_changed_lines_additions() {
        let hunks = vec![DiffHunk {
            old_start: 3,
            old_count: 1,
            new_start: 3,
            new_count: 3,
            patch_lines: vec!["-  old line", "+  new line 1", "+  new line 2"],
        }];
        let lines = build_changed_lines(&hunks);
        // Deletion at new_start=3 inserts 3, additions insert 3 and 4
        assert!(
            lines.contains(&3),
            "expected line 3 in changed set: {lines:?}"
        );
        assert!(
            lines.contains(&4),
            "expected line 4 in changed set: {lines:?}"
        );
    }

    #[test]
    fn test_build_changed_lines_context_only() {
        // Context lines (starting with ' ') should not appear in the changed set
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 3,
            new_start: 1,
            new_count: 3,
            patch_lines: vec![" unchanged 1", " unchanged 2", " unchanged 3"],
        }];
        let lines = build_changed_lines(&hunks);
        assert!(
            lines.is_empty(),
            "pure context hunks should yield empty changed set: {lines:?}"
        );
    }

    #[test]
    fn test_build_changed_lines_empty_hunks() {
        let lines = build_changed_lines(&[]);
        assert!(lines.is_empty());
    }

    #[test]
    fn test_build_changed_lines_deletions_mark_boundary() {
        // Deletions mark the current new-file position as a change boundary
        let hunks = vec![DiffHunk {
            old_start: 5,
            old_count: 2,
            new_start: 5,
            new_count: 0,
            patch_lines: vec!["-  removed line 1", "-  removed line 2"],
        }];
        let lines = build_changed_lines(&hunks);
        assert!(
            lines.contains(&5),
            "deletion boundary should be marked: {lines:?}"
        );
    }

    #[test]
    fn test_build_changed_lines_multiple_hunks() {
        let hunks = vec![
            DiffHunk {
                old_start: 2,
                old_count: 1,
                new_start: 2,
                new_count: 1,
                patch_lines: vec!["-  old", "+  new"],
            },
            DiffHunk {
                old_start: 10,
                old_count: 1,
                new_start: 10,
                new_count: 1,
                patch_lines: vec!["-  old2", "+  new2"],
            },
        ];
        let lines = build_changed_lines(&hunks);
        assert!(lines.contains(&2), "first hunk change at line 2: {lines:?}");
        assert!(
            lines.contains(&10),
            "second hunk change at line 10: {lines:?}"
        );
        // Lines between hunks should not be marked
        assert!(
            !lines.contains(&6),
            "line 6 should not be in changed set: {lines:?}"
        );
    }

    // ========================================================================
    // is_container_node unit tests (#103)
    // ========================================================================

    #[test]
    fn test_is_container_node_class() {
        let source = "class Foo {\n  x: number = 1;\n}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let class_node = root.children(&mut cursor).next().unwrap();
        assert!(
            is_container_node(&class_node),
            "class_declaration should be a container node, got kind: {}",
            class_node.kind()
        );
    }

    #[test]
    fn test_is_container_node_function_is_not() {
        let source = "function foo() { return 1; }\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let fn_node = root.children(&mut cursor).next().unwrap();
        assert!(
            !is_container_node(&fn_node),
            "function_declaration should NOT be a container node, got kind: {}",
            fn_node.kind()
        );
    }

    #[test]
    fn test_is_container_node_rust_struct() {
        let source = "struct Point {\n    x: i32,\n    y: i32,\n}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::Rust).unwrap();
        let tree = parser.parse(source).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let struct_node = root.children(&mut cursor).next().unwrap();
        assert!(
            is_container_node(&struct_node),
            "struct_item should be a container node, got kind: {}",
            struct_node.kind()
        );
    }

    #[test]
    fn test_is_container_node_rust_impl() {
        let source = "impl Foo {\n    fn bar(&self) {}\n}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::Rust).unwrap();
        let tree = parser.parse(source).unwrap();
        let root = tree.root_node();
        let mut cursor = root.walk();
        let impl_node = root.children(&mut cursor).next().unwrap();
        assert!(
            is_container_node(&impl_node),
            "impl_item should be a container node, got kind: {}",
            impl_node.kind()
        );
    }

    // ========================================================================
    // Changed node detection tests (#103)
    // ========================================================================

    #[test]
    fn test_find_changed_nodes_function_overlaps_hunk() {
        let source = "function foo() {\n  return 1;\n}\n\nfunction bar() {\n  return 2;\n}\n";

        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        // Simulate a hunk that changes line 2 (inside foo)
        let hunks = vec![DiffHunk {
            old_start: 2,
            old_count: 1,
            new_start: 2,
            new_count: 2,
            patch_lines: vec!["-  return 1;", "+  return 42;", "+  console.log(42);"],
        }];

        let ranges = find_changed_node_ranges(&tree, &hunks);

        // Should find at least the function containing line 2
        assert!(
            !ranges.is_empty(),
            "expected at least one changed node range"
        );
        // The changed range should cover foo (lines 1-3) but not bar (lines 5-7)
        assert!(
            ranges[0].start <= 2,
            "changed range should start at or before line 2"
        );
        assert!(
            ranges[0].end >= 2,
            "changed range should end at or after line 2"
        );
    }

    #[test]
    fn test_find_changed_nodes_empty_hunks() {
        let source = "function foo() {}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        let ranges = find_changed_node_ranges(&tree, &[]);
        assert!(ranges.is_empty(), "no hunks should yield no changed nodes");
    }

    #[test]
    fn test_find_changed_nodes_import_overlaps() {
        let source = "import { foo } from 'bar';\nimport { baz } from 'qux';\n\nfunction main() {\n  foo();\n}\n";
        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        // Simulate a hunk that changes line 1 (first import)
        let hunks = vec![DiffHunk {
            old_start: 1,
            old_count: 1,
            new_start: 1,
            new_count: 1,
            patch_lines: vec![
                "-import { foo } from 'bar';",
                "+import { foo, extra } from 'bar';",
            ],
        }];

        let ranges = find_changed_node_ranges(&tree, &hunks);
        assert!(!ranges.is_empty(), "import change should be detected");
    }

    #[test]
    fn test_find_changed_nodes_nested_class_method() {
        // Gap 3: verify nested node detection narrows to child method
        let source = "class Greeter {\n  greet(name: string) {\n    return `Hello, ${name}`;\n  }\n  farewell(name: string) {\n    return `Bye, ${name}`;\n  }\n}\n";

        let mut parser = rskim_core::Parser::new(rskim_core::Language::TypeScript).unwrap();
        let tree = parser.parse(source).unwrap();

        // Simulate a hunk that changes line 3 (inside greet method)
        let hunks = vec![DiffHunk {
            old_start: 3,
            old_count: 1,
            new_start: 3,
            new_count: 1,
            patch_lines: vec![
                "-    return `Hello, ${name}`;",
                "+    return `Hi, ${name}`;",
            ],
        }];

        let ranges = find_changed_node_ranges(&tree, &hunks);
        assert!(
            !ranges.is_empty(),
            "expected at least one changed node range"
        );

        // Should have parent context since greet is inside Greeter class
        let first = &ranges[0];
        assert!(
            first.parent_context.is_some(),
            "expected parent context for nested node"
        );
        let parent = first.parent_context.as_ref().unwrap();
        assert_eq!(
            parent.header_line, 1,
            "parent header should be class declaration"
        );
    }
}
