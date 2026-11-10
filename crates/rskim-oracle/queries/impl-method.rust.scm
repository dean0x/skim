; Structural oracle for skim's `impl-method` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A method inside a Rust impl block. Exact: impl_item contains a
;   declaration_list, which contains function_item nodes.
;
; Encoded definition:
;   a function_item that is a direct child of the declaration_list of an
;   impl_item (an impl with no method is not a match).
;
; Match line: the first line of the @match node, the function_item (the
; method).
(impl_item
  (declaration_list
    (function_item) @match))
