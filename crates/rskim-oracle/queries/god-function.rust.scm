; Structural oracle for skim's `god-function` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A function with a very large body (Rust, >= 20 statements). Exact:
;   LARGE_BODY -> bucket_label(1) is emitted when a function body has >= 20
;   counted children. Bucket granularity is fixed at index time; only
;   function/method bodies emit this marker.
;
; Encoded definition:
;   a function_item (free function or method) whose block body has at least 20
;   body elements. Post-filter (Rust, structural.rs `PostFilter::AtLeast`, min
;   20): a body element is a named, non-extra child that is not an attribute --
;   a statement, a nested item, or the tail expression; comments and attributes
;   do not count.
;
; Match line: the first line of the @match node, the function_item.
(function_item
  (block) @body) @match
