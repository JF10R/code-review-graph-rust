; PHP node-type classifications

; --- Classes ---
(class_declaration) @definition.class
(interface_declaration) @definition.class

; --- Functions ---
(function_definition) @definition.function
(method_declaration) @definition.function

; --- Imports ---
(namespace_use_declaration) @reference.import

; --- Calls ---
(function_call_expression) @reference.call
(member_call_expression) @reference.call
