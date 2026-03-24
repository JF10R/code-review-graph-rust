; Python node-type classifications for code-review-graph
; Each pattern maps to a category tag used to classify AST node kinds.

; --- Classes ---
; @definition.class patterns: node kinds that represent class-like constructs
(class_definition) @definition.class

; --- Functions ---
; @definition.function patterns: node kinds that represent callable definitions
(function_definition) @definition.function

; --- Imports ---
; @reference.import patterns: node kinds that represent import statements
(import_statement) @reference.import
(import_from_statement) @reference.import

; --- Calls ---
; @reference.call patterns: node kinds that represent call sites
(call) @reference.call
