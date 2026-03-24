; Go node-type classifications

; --- Classes (Go uses type declarations for structs/interfaces) ---
(type_declaration) @definition.class

; --- Functions ---
(function_declaration) @definition.function
(method_declaration) @definition.function

; --- Imports ---
(import_declaration) @reference.import

; --- Calls ---
(call_expression) @reference.call
