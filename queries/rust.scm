; Rust node-type classifications

; --- Classes (struct/enum/impl treated as class-like) ---
(struct_item) @definition.class
(enum_item) @definition.class
(impl_item) @definition.class

; --- Functions ---
(function_item) @definition.function

; --- Imports ---
(use_declaration) @reference.import

; --- Calls ---
(call_expression) @reference.call
(macro_invocation) @reference.call
