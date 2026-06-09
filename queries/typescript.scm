; TypeScript symbol + edge extraction. Same capture conventions as rust.scm:
; @def.<kind> marks the whole definition node; @name is the identifier;
; @call.name / @extends.name / @import.* drive best-effort edges.

; --- definitions ---
(class_declaration     name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
(enum_declaration      name: (identifier) @name) @def.enum
(type_alias_declaration name: (type_identifier) @name) @def.type
(function_declaration  name: (identifier) @name) @def.function
(method_definition     name: (property_identifier) @name) @def.method
; `const foo = (..) => ..` / `const foo = function ..`
(variable_declarator name: (identifier) @name value: (arrow_function)) @def.function
(variable_declarator name: (identifier) @name value: (function_expression)) @def.function

; --- edges: calls (bare `foo(x)` and member `obj.foo(x)`) ---
(call_expression function: (identifier) @call.name)
(call_expression function: (member_expression property: (property_identifier) @call.name))

; --- edges: extends / implements ---
(extends_clause value: (identifier) @extends.name)
(implements_clause (type_identifier) @extends.name)

; --- edges: imports (imported symbol names) ---
(import_specifier name: (identifier) @import.name)
(import_clause (identifier) @import.name)
