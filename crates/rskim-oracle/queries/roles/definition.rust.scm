; Role oracle, `def` role (rust): the name node of each declaration.
; Grammar: tree-sitter-rust 0.24.0.
;
; Declaration kinds: function_item, function_signature_item, struct_item,
; enum_item, union_item, trait_item, type_item, const_item, static_item,
; mod_item, macro_definition, enum_variant, field_declaration. Each `name`
; field is captured with the node type the grammar gives it.
;
; Capture: @name, the declared identifier.
(function_item name: (identifier) @name)
(function_signature_item name: (identifier) @name)
(struct_item name: (type_identifier) @name)
(enum_item name: (type_identifier) @name)
(union_item name: (type_identifier) @name)
(trait_item name: (type_identifier) @name)
(type_item name: (type_identifier) @name)
(const_item name: (identifier) @name)
(static_item name: (identifier) @name)
(mod_item name: (identifier) @name)
(macro_definition name: (identifier) @name)
(enum_variant name: (identifier) @name)
(field_declaration name: (field_identifier) @name)
