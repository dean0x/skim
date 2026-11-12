; Role oracle, `call` role (go): the identifier in callee position.
; Grammar: tree-sitter-go 0.25.0.
;
; Callee position: call_expression.function. The name passes through
; selector_expression.field; the operand of a selector is not the callee.
;
; Capture: @callee, the identifier that names the callee.
(call_expression function: (identifier) @callee)
(call_expression function: (selector_expression field: (field_identifier) @callee))
