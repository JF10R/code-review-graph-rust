; Kotlin node-type classifications

; --- Classes ---
(class_declaration) @definition.class
(object_declaration) @definition.class

; --- Functions ---
(function_declaration) @definition.function

; --- Imports ---
(import_header) @reference.import

; --- Calls ---
(call_expression) @reference.call
