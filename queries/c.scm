; C / C++ node-type classifications
; C and C++ share a file; C uses struct_specifier/type_definition,
; C++ additionally uses class_specifier.

; --- Classes ---
(struct_specifier) @definition.class
(enum_specifier) @definition.class
(union_specifier) @definition.class
(type_definition) @definition.class
(class_specifier) @definition.class

; --- Functions ---
(function_definition) @definition.function

; --- Imports ---
(preproc_include) @reference.import

; --- Calls ---
(call_expression) @reference.call
