; Zig node-type classifications
;
; Zig is unique: structs/enums/unions are anonymous type expressions assigned
; to `const` variable_declarations (e.g. `const Foo = struct { ... }`).
; The parser handles this via special variable_declaration unwrapping logic.

; --- Classes (struct/enum/union/opaque treated as class-like) ---
(struct_declaration) @definition.class
(enum_declaration) @definition.class
(union_declaration) @definition.class
(opaque_declaration) @definition.class

; --- Functions ---
(function_declaration) @definition.function

; --- Types (error sets) ---
(error_set_declaration) @definition.type

; --- Imports (usingnamespace) ---
(using_namespace_declaration) @reference.import

; --- Calls ---
(call_expression) @reference.call
(builtin_function) @reference.call
