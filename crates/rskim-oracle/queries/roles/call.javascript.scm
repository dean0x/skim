; Role oracle, `call` role (javascript): the identifier in callee position.
; Grammar: tree-sitter-javascript 0.25.0.
;
; Callee positions: call_expression.function and
; new_expression.constructor. The name passes through
; member_expression.property; the object of a member expression is not the
; callee. A JSX element is not a call.
;
; Capture: @callee, the identifier that names the callee.
(call_expression function: (identifier) @callee)
(call_expression function: (member_expression property: (property_identifier) @callee))
(new_expression constructor: (identifier) @callee)
(new_expression constructor: (member_expression property: (property_identifier) @callee))
