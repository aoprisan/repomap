; Rust symbol + edge extraction.
; Convention: @def.<kind> marks the whole definition node (gives start/end
; lines); @name is the identifier; @call.name / @extends.name / @import.decl
; drive best-effort edges. The extractor derives the signature from the def
; node's first line and the doc from the preceding comment.

; --- definitions ---
(function_item name: (identifier) @name) @def.fn
(struct_item   name: (type_identifier) @name) @def.struct
(enum_item     name: (type_identifier) @name) @def.enum
(trait_item    name: (type_identifier) @name) @def.trait
(mod_item      name: (identifier) @name) @def.mod
(const_item    name: (identifier) @name) @def.const
(static_item   name: (identifier) @name) @def.static
(type_item     name: (type_identifier) @name) @def.type
(impl_item     type: (type_identifier) @name) @def.impl

; --- edges: calls ---
(call_expression function: (identifier) @call.name)
(call_expression function: (scoped_identifier name: (identifier) @call.name))
(call_expression function: (field_expression field: (field_identifier) @call.name))
(macro_invocation macro: (identifier) @call.name)

; --- edges: extends (impl Trait for Type) ---
(impl_item trait: (type_identifier) @extends.name)

; --- edges: imports ---
(use_declaration) @import.decl
