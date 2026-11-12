; Role oracle, `def` role (tsx): the name node of each declaration.
; Grammar: tree-sitter-typescript 0.23.2 (LANGUAGE_TSX).
;
; Declaration kinds: function_declaration, generator_function_declaration,
; function_signature, class_declaration, abstract_class_declaration,
; interface_declaration, type_alias_declaration, enum_declaration,
; method_definition, and a variable_declarator whose name is an identifier.
; A variable_declarator is a declaration only outside every statement_block,
; which a query cannot say; its capture has its own name and the code checks
; the ancestors.
;
; Captures: @name, the declared identifier; @name.outside_block, the name of
; a variable_declarator.
(function_declaration name: (identifier) @name)
(generator_function_declaration name: (identifier) @name)
(function_signature name: (identifier) @name)
(class_declaration name: (type_identifier) @name)
(abstract_class_declaration name: (type_identifier) @name)
(interface_declaration name: (type_identifier) @name)
(type_alias_declaration name: (type_identifier) @name)
(enum_declaration name: (identifier) @name)
(method_definition name: (property_identifier) @name)
(variable_declarator name: (identifier) @name.outside_block)
