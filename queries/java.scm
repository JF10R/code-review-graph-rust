; Java node-type classifications

; --- Classes ---
(class_declaration) @definition.class
(interface_declaration) @definition.class
(enum_declaration) @definition.class

; --- Functions ---
(method_declaration) @definition.function
(constructor_declaration) @definition.function

; --- Imports ---
(import_declaration) @reference.import

; --- Calls ---
(method_invocation) @reference.call
(object_creation_expression) @reference.call
