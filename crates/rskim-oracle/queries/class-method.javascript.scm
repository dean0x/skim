; Structural oracle for skim's `class-method` --ast pattern (javascript).
; Grammar: tree-sitter-javascript 0.25.0.
;
; Catalog description (crates/rskim-search/src/ast_index/patterns.rs):
;   A method inside a TypeScript class. Exact: class_declaration contains a
;   class_body, which contains method_definition nodes.
;
; Encoded definition:
;   a method_definition that is a direct child of the class_body of a
;   class_declaration. The same node kinds and relation exist in the JavaScript
;   grammar, so JavaScript is covered too. An abstract_class_declaration or a
;   class expression is not a class_declaration.
;
; Match line: the first line of the @match node, the method_definition.
(class_declaration
  (class_body
    (method_definition) @match))
