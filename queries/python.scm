; Python symbol + edge extraction. Same capture conventions as rust.scm:
; @def.<kind> marks the whole definition node; @name is the identifier;
; @call.name / @extends.name / @import.* drive best-effort edges.

; --- definitions ---
(class_definition    name: (identifier) @name) @def.class
(function_definition name: (identifier) @name) @def.def

; --- edges: calls (bare `foo(x)` and attribute `obj.foo(x)`) ---
(call function: (identifier) @call.name)
(call function: (attribute attribute: (identifier) @call.name))

; --- edges: base classes (`class Foo(Bar):`) ---
(class_definition superclasses: (argument_list (identifier) @extends.name))

; --- edges: imports ---
(import_statement) @import.decl
(import_from_statement) @import.decl
