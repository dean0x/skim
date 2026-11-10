; Structural oracle for skim's `excessive-params` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A function with many parameters (Rust, >= 5). Exact: MANY_PARAMS ->
;   bucket_label(0) is emitted when a parameter list has >= 5 counted children.
;   Bucket granularity is fixed at index time.
;
; Encoded definition:
;   a function -- a function_item, or a bodyless function_signature_item in a
;   trait or extern block -- whose parameters list has at least 5 parameters.
;   Post-filter (Rust, structural.rs `PostFilter::AtLeast`, min 5): a parameter
;   is a named, non-extra child that is not an attribute, so `self` counts and a
;   `#[cfg]` on a parameter does not. Closures and `fn(..)` pointer types are
;   not functions.
;
; Match line: the first line of the @match node, the function_item /
; function_signature_item.
[
  (function_item
    (parameters) @params)
  (function_signature_item
    (parameters) @params)
] @match
