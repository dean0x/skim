; Role oracle, `def` role (go): the name node of each declaration.
; Grammar: tree-sitter-go 0.25.0.
;
; Declaration kinds: function_declaration, method_declaration, type_spec,
; const_spec, var_spec. A const_spec or var_spec can hold several `name`
; children (`const A, B = 1, 2`). A query step with the `name:` field binds
; only the first of them (checked against tree-sitter 0.25: with the field,
; `Min` of `const Max, Min = 1, 2` is missed), so those two use the field-less
; step: their only direct identifier children are the names, because the type
; is a type_identifier and the value sits in an expression_list.
;
; Capture: @name, the declared identifier.
(function_declaration name: (identifier) @name)
(method_declaration name: (field_identifier) @name)
(type_spec name: (type_identifier) @name)
(const_spec (identifier) @name)
(var_spec (identifier) @name)
