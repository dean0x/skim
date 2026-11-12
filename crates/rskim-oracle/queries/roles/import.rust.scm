; Role oracle, `import` role (rust): the import node.
; Grammar: tree-sitter-rust 0.24.0.
;
; Import node: use_declaration. A use tree nests without a depth bound
; (use_list, scoped_use_list, use_as_clause), which a query cannot follow, so
; the code visits the identifier leaves below the captured node.
;
; Capture: @import, the whole import node.
(use_declaration) @import
