; Structural oracle for skim's `empty-function` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A function item with an empty body (Rust). Exact: EMPTY_BODY is emitted when
;   a block has zero counted children, keyed on the enclosing function_item.
;
; Encoded definition:
;   a function_item whose block body has zero body elements. Post-filter (Rust,
;   structural.rs `PostFilter::Empty`): a body element is a named, non-extra
;   child that is not an attribute, so a comment-only body is empty and a tail
;   expression (`{ self }`) is not.
;
; Match line: the first line of the @match node, the function_item.
(function_item
  (block) @body) @match
