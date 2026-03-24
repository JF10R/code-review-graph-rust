; C# node-type classifications

; --- Classes ---
(class_declaration) @definition.class
(interface_declaration) @definition.class
(enum_declaration) @definition.class
(struct_declaration) @definition.class

; --- Functions ---
(method_declaration) @definition.function
(constructor_declaration) @definition.function

; --- Imports ---
(using_directive) @reference.import

; --- Calls ---
(invocation_expression) @reference.call
(object_creation_expression) @reference.call
