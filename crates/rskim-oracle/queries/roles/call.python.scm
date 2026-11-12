; Role oracle, `call` role (python): the identifier in callee position.
; Grammar: tree-sitter-python 0.25.0.
;
; Callee position: call.function. The name passes through attribute.attribute;
; the object of an attribute is not the callee.
;
; Capture: @callee, the identifier that names the callee.
(call function: (identifier) @callee)
(call function: (attribute attribute: (identifier) @callee))
