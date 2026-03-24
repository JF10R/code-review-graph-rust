; Ruby node-type classifications

; --- Classes ---
(class) @definition.class
(module) @definition.class

; --- Functions ---
(method) @definition.function
(singleton_method) @definition.function

; --- Imports (Ruby uses call nodes for require; filtered at use site) ---
(call) @reference.import

; --- Calls ---
(call) @reference.call
(method_call) @reference.call
