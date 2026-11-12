; Role oracle, `def` role (javascript): the name node of each declaration.
; Grammar: tree-sitter-javascript 0.25.0.
;
; Declaration kinds the grammar has: function_declaration,
; generator_function_declaration, class_declaration, method_definition, and a
; variable_declarator whose name is an identifier. (It has no
; function_signature, abstract_class_declaration, interface_declaration,
; type_alias_declaration or enum_declaration.) A variable_declarator is a
; declaration only outside every statement_block, which a query cannot say;
; its capture has its own name and the code checks the ancestors.
;
; Captures: @name, the declared identifier; @name.outside_block, the name of
; a variable_declarator.
(function_declaration name: (identifier) @name)
(generator_function_declaration name: (identifier) @name)
(class_declaration name: (identifier) @name)
(method_definition name: (property_identifier) @name)
(variable_declarator name: (identifier) @name.outside_block)
