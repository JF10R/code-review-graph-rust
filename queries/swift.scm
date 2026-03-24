; Swift node-type classifications

; --- Classes ---
(class_declaration) @definition.class
(struct_declaration) @definition.class
(protocol_declaration) @definition.class

; --- Functions ---
(function_declaration) @definition.function

; --- Imports ---
(import_declaration) @reference.import

; --- Calls ---
(call_expression) @reference.call
