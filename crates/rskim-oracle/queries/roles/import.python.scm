; Role oracle, `import` role (python): the import node.
; Grammar: tree-sitter-python 0.25.0.
;
; Import nodes: import_statement and import_from_statement. The code visits
; the identifier leaves below the captured node (dotted names, aliases).
;
; Capture: @import, the whole import node.
(import_statement) @import
(import_from_statement) @import
