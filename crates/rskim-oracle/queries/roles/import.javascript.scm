; Role oracle, `import` role (javascript): the import node.
; Grammar: tree-sitter-javascript 0.25.0.
;
; Import nodes: import_statement, and an export_statement that has a source
; (a re-export). An export_statement without a source is not an import. The
; code visits the identifier leaves below the captured node; the source is a
; string, not an identifier.
;
; Capture: @import, the whole import node.
(import_statement) @import
(export_statement source: (string)) @import
