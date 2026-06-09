; Ruby symbol + edge extraction. Same capture conventions as rust.scm:
; @def.<kind> marks the whole definition node; @name is the identifier;
; @call.name / @extends.name drive best-effort edges.

; --- definitions ---
(class            name: (constant) @name) @def.class
(module           name: (constant) @name) @def.module
(method           name: (identifier) @name) @def.method
(singleton_method name: (identifier) @name) @def.method

; --- edges: calls (bare `foo(x)` and receiver `obj.foo`) ---
(call method: (identifier) @call.name)

; --- edges: superclass (`class Foo < Bar`) ---
(superclass (constant) @extends.name)
