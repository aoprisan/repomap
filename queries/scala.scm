; Scala symbol + edge extraction. Same capture conventions as rust.scm.
; Node names follow tree-sitter-scala; refine here without touching Rust code.

; --- definitions ---
(object_definition   name: (identifier) @name) @def.object
(class_definition    name: (identifier) @name) @def.class
(trait_definition    name: (identifier) @name) @def.trait
(function_definition name: (identifier) @name) @def.def
(function_declaration name: (identifier) @name) @def.def
(val_definition  pattern: (identifier) @name) @def.val
(var_definition  pattern: (identifier) @name) @def.var
(type_definition name: (type_identifier) @name) @def.type

; --- edges: calls ---
(call_expression function: (identifier) @call.name)
(call_expression function: (field_expression field: (identifier) @call.name))

; --- edges: extends / with ---
(extends_clause (type_identifier) @extends.name)

; --- edges: imports ---
(import_declaration) @import.decl
