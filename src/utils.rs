use clang::Entity;
use regex::Regex;

pub fn get_entity_id(entity: &Entity) -> String {
    if let Some(name) = entity.get_name() {
        if let Some(loc) = entity.get_location() {
            let file = loc.get_file_location();
            format!("{}:{}:{}", name, file.line, file.column)
        } else {
            name
        }
    } else {
        format!("{:?}", entity.get_kind())
    }
}

pub fn is_system_entity(entity: &Entity) -> bool {
    if let Some(loc) = entity.get_location() {
        let file_path = loc
            .get_file_location()
            .file
            .map(|f| f.get_path())
            .unwrap_or_default();

        let path_str = file_path.to_string_lossy();
        path_str.contains("/usr/include/")
            || path_str.contains("/usr/lib/")
            || path_str.contains("/usr/local/include/")
    } else {
        false
    }
}

pub fn is_unsafe_function(name: &str) -> bool {
    let unsafe_functions = [
        "strcpy", "strcat", "sprintf", "gets", "scanf", "vsprintf", "memcpy", "memmove", "strncpy",
        "strncat",
    ];

    unsafe_functions.contains(&name)
}

pub fn is_standard_library_function(name: &str) -> bool {
    let std_functions = [
        "printf",
        "sprintf",
        "fprintf",
        "snprintf",
        "vprintf",
        "vsprintf",
        "vfprintf",
        "vsnprintf",
        "scanf",
        "sscanf",
        "fscanf",
        "vscanf",
        "vsscanf",
        "vfscanf",
        "malloc",
        "calloc",
        "realloc",
        "aligned_alloc",
        "free",
        "exit",
        "abort",
        "atexit",
        "_Exit",
        "system",
        "getenv",
        "setenv",
        "putenv",
        "unsetenv",
        "time",
        "clock",
        "difftime",
        "mktime",
        "asctime",
        "ctime",
        "gmtime",
        "localtime",
        "strftime",
        "rand",
        "srand",
        "rand_r",
        "atoi",
        "atol",
        "atoll",
        "strtol",
        "strtoll",
        "strtoul",
        "strtoull",
        "memcpy",
        "memmove",
        "memset",
        "memcmp",
        "memchr",
        "memccpy",
        "strlen",
        "strnlen",
        "strcpy",
        "strncpy",
        "strcat",
        "strncat",
        "strcmp",
        "strncmp",
        "strchr",
        "strrchr",
        "strstr",
        "strtok",
        "fopen",
        "fclose",
        "fflush",
        "fread",
        "fwrite",
        "fseek",
        "ftell",
        "fgetpos",
        "fsetpos",
    ];

    std_functions.contains(&name)
}

pub fn get_line_number(entity: &Entity) -> Option<usize> {
    entity.get_location().map(|loc| {
        let file_loc = loc.get_file_location();
        file_loc.line as usize
    })
}

// Extract function calls directly from the source code as a fallback mechanism
pub fn extract_function_calls_from_source(source_code: &str) -> Vec<(String, String)> {
    let mut calls = Vec::new();

    // First identify all functions
    let func_regex = Regex::new(r"(?m)^(?:\w+\s+)+(\w+)\s*\([^)]*\)\s*\{").unwrap();
    let func_names: Vec<String> = func_regex
        .captures_iter(source_code)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect();

    // Then for each function, find all function calls within it
    for func_name in &func_names {
        // Find the function body
        let func_pattern = format!(
            r"(?m)^(?:\w+\s+)+{}\s*\([^)]*\)\s*\{{",
            regex::escape(func_name)
        );
        let func_body_regex = Regex::new(&func_pattern).unwrap();

        if let Some(func_match) = func_body_regex.find(source_code) {
            let start_pos = func_match.end();
            let mut depth = 1;
            let mut end_pos = start_pos;

            // Find the matching closing brace
            for (i, c) in source_code[start_pos..].char_indices() {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = start_pos + i;
                        break;
                    }
                }
            }

            // Extract the function body
            let body = &source_code[start_pos..end_pos];

            // Find function calls in the body
            let call_regex = Regex::new(r"(\w+)\s*\(").unwrap();

            for cap in call_regex.captures_iter(body) {
                if let Some(callee) = cap.get(1) {
                    let callee_name = callee.as_str().to_string();

                    // Skip if the call is to a standard C function that we're not interested in
                    if is_standard_library_function(&callee_name) {
                        continue;
                    }

                    // Skip if the callee is actually a keyword
                    if ["if", "for", "while", "switch", "return"].contains(&callee_name.as_str()) {
                        continue;
                    }

                    // Add the call to our list
                    calls.push((func_name.clone(), callee_name));
                }
            }
        }
    }

    calls
}

// Specialized function to extract pthread_create handler assignments
pub fn extract_pthread_assignments(source_code: &str) -> Vec<(String, String)> {
    let mut assignments = Vec::new();

    // First identify all functions
    let func_regex = Regex::new(r"(?m)^(?:\w+\s+)+(\w+)\s*\([^)]*\)\s*\{").unwrap();
    let func_names: Vec<String> = func_regex
        .captures_iter(source_code)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect();

    // Then for each function, look specifically for pthread_create calls
    for func_name in &func_names {
        // Find the function body
        let func_pattern = format!(
            r"(?m)^(?:\w+\s+)+{}\s*\([^)]*\)\s*\{{",
            regex::escape(func_name)
        );
        let func_body_regex = Regex::new(&func_pattern).unwrap();

        if let Some(func_match) = func_body_regex.find(source_code) {
            let start_pos = func_match.end();
            let mut depth = 1;
            let mut end_pos = start_pos;

            // Find the matching closing brace
            for (i, c) in source_code[start_pos..].char_indices() {
                if c == '{' {
                    depth += 1;
                } else if c == '}' {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = start_pos + i;
                        break;
                    }
                }
            }

            // Extract the function body
            let body = &source_code[start_pos..end_pos];

            // Look specifically for pthread_create with function handlers
            // Pattern matches: pthread_create(..., handler_func, ...)
            let pthread_regex =
                Regex::new(r"pthread_create\s*\([^,]+,\s*[^,]*,\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*,")
                    .unwrap();

            for cap in pthread_regex.captures_iter(body) {
                if let Some(handler_func) = cap.get(1) {
                    let handler_name = handler_func.as_str().to_string();
                    // Make sure this is a known function name
                    if func_names.contains(&handler_name) {
                        assignments.push((func_name.clone(), handler_name));
                    }
                }
            }
        }
    }

    assignments
}
