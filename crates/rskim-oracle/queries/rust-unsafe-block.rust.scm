; Structural oracle for skim's `rust-unsafe-block` --ast pattern (rust).
; Grammar: tree-sitter-rust 0.24.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   An unsafe block (Rust). Exact: unsafe_block is the CST node for `unsafe {
;   ... }`. unsafe_block always contains a block node as its body.
;
; Encoded definition:
;   every unsafe_block (an `unsafe fn` is not an unsafe block).
;
; Match line: the first line of the @match node, the unsafe_block.
(unsafe_block) @match
