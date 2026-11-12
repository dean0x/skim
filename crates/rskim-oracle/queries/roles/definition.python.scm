; Role oracle, `def` role (python): the name node of each declaration.
; Grammar: tree-sitter-python 0.25.0.
;
; Declaration kinds: function_definition, class_definition.
;
; Capture: @name, the declared identifier.
(function_definition name: (identifier) @name)
(class_definition name: (identifier) @name)
