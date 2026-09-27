; Structural oracle for skim's `function-with-body` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A function item with a body (Rust). Exact: function_item always contains a
;   block.
;
; Encoded definition:
;   a function_item with a block child (a bodyless trait or extern declaration
;   is a function_signature_item, not a function_item).
;
; Match line: the first line of the @match node, the function_item.
(function_item
  (block)) @match
