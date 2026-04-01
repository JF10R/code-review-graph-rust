; JavaScript / TypeScript / TSX node-type classifications
; Shared across javascript, typescript, and tsx languages.

; --- Classes ---
(class_declaration) @definition.class
(class) @definition.class

; --- Functions ---
(function_declaration) @definition.function
(method_definition) @definition.function
(arrow_function) @definition.function
(public_field_definition) @definition.function

; --- Imports ---
(import_statement) @reference.import

; --- Calls ---
(call_expression) @reference.call
(new_expression) @reference.call

; --- Types ---
(interface_declaration) @definition.type
(type_alias_declaration) @definition.type
