; C# node-type classifications

; --- Classes ---
(class_declaration) @definition.class
(enum_declaration) @definition.class
(struct_declaration) @definition.class
(record_declaration) @definition.class

; --- Functions ---
(method_declaration) @definition.function
(constructor_declaration) @definition.function
(property_declaration) @definition.function
(event_declaration) @definition.function

; --- Types ---
(interface_declaration) @definition.type
(delegate_declaration) @definition.type

; --- Imports ---
(using_directive) @reference.import

; --- Calls ---
(invocation_expression) @reference.call
(object_creation_expression) @reference.call
