; Role oracle, `import` role (go): the import node.
; Grammar: tree-sitter-go 0.25.0.
;
; Import node: import_declaration. The code visits the identifier leaves
; below it (an import alias is a package_identifier); the import path is a
; string, not an identifier.
;
; Capture: @import, the whole import node.
(import_declaration) @import
