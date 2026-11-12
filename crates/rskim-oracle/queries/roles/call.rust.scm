; Role oracle, `call` role (rust): the identifier in callee position.
; Grammar: tree-sitter-rust 0.24.0.
;
; Callee positions: call_expression.function and macro_invocation.macro. The
; name passes through scoped_identifier.name, field_expression.field and
; generic_function.function (which wraps an identifier, a scoped_identifier
; or a field_expression). A path segment before `::` and the receiver of a
; field expression are not the callee.
;
; Capture: @callee, the identifier that names the callee.
(call_expression function: (identifier) @callee)
(call_expression function: (scoped_identifier name: (identifier) @callee))
(call_expression function: (field_expression field: (field_identifier) @callee))
(call_expression function: (generic_function function: (identifier) @callee))
(call_expression
  function: (generic_function
    function: (scoped_identifier name: (identifier) @callee)))
(call_expression
  function: (generic_function
    function: (field_expression field: (field_identifier) @callee)))
(macro_invocation macro: (identifier) @callee)
(macro_invocation macro: (scoped_identifier name: (identifier) @callee))
