use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::str::FromStr;
use petgraph::visit::EdgeRef;

use anyhow::{Context, Result};
use clang::{Entity, EntityKind, Index, TranslationUnit};
use petgraph::dot::{Config, Dot};
use petgraph::graph::{DiGraph, NodeIndex};
use structopt::StructOpt;
use serde_json::{json, Value};
use regex::Regex;

#[derive(Debug, Clone, PartialEq)]
enum NodeType {
    Function,
    Main,
    Parameter,
    BufferParameter,
    Variable,
    Call,
    UnsafeCall,
    BasicBlock,
    IfStatement,
    ForLoop,
    WhileLoop,
}

#[derive(Debug, Clone)]
struct Node {
    name: String,
    kind: NodeType,
    line: Option<usize>,
    // Store original entity USR for function calls to match with definitions
    usr: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum EdgeType {
    Contains,
    Calls,
    Controls,
    Uses,
    Defines,
    References,
}

#[derive(Debug)]
struct Edge {
    kind: EdgeType,
}

#[derive(Debug, StructOpt)]
#[structopt(name = "c-code-analyzer", about = "Analyze C code and generate visualizations")]
struct Opt {
    /// Input C source file
    #[structopt(parse(from_os_str))]
    input: PathBuf,

    /// Output file
    #[structopt(parse(from_os_str), short, long)]
    output: Option<PathBuf>,
    
    /// Output format (json or dot)
    #[structopt(short, long, default_value = "dot")]
    format: String,
    
    /// Debug mode
    #[structopt(short, long)]
    debug: bool,
}

fn main() -> Result<()> {
    let opt = Opt::from_args();

    // Read the content of the C file
    let content = fs::read_to_string(&opt.input)
        .with_context(|| format!("Failed to read file: {:?}", opt.input))?;

    let pthread_assignments = extract_pthread_assignments(&content);
    if opt.debug {
        println!("Extracted pthread assignments:");
        for (caller, handler_func) in &pthread_assignments {
            println!("  {} assigns {} to pthread", caller, handler_func);
        }
    }

    // Initialize Clang
    let clang = clang::Clang::new().unwrap();
    let index = clang::Index::new(&clang, true, true);
    
    // Use more clang options for better analysis
    let clang_args = vec![
        "-Wall".to_string(),
        "-I/usr/include".to_string(),
        "-I/usr/local/include".to_string(),
    ];
    
    let tu = index.parser(opt.input.to_str().unwrap())
        .arguments(&clang_args)
        .detailed_preprocessing_record(true)
        .skip_function_bodies(false)
        .parse()
        .with_context(|| "Failed to parse C file with Clang")?;

    // Extract function calls directly from the source code as a backup
    let function_calls = extract_function_calls_from_source(&content);
    if opt.debug {
        println!("Extracted function calls from source:");
        for (caller, callee) in &function_calls {
            println!("  {} calls {}", caller, callee);
        }
    }

    // Build our graph
    let mut graph = DiGraph::<Node, Edge>::new();
    let mut node_map: HashMap<String, NodeIndex> = HashMap::new();
    let mut usr_map: HashMap<String, NodeIndex> = HashMap::new();
    let mut processed_entities = HashSet::new();
    
    // First pass: identify all functions to ensure they're in the graph
    find_all_functions(tu.get_entity(), &mut graph, &mut node_map, &mut usr_map);
    
    // Second pass: process the entire AST and build relationships
    analyze_program(tu.get_entity(), &mut graph, &mut node_map, &mut usr_map, &mut processed_entities, &content, opt.debug);
    
    // Post-process: ensure call connections are properly established
    fix_disconnected_calls(&mut graph, &node_map, &usr_map, &function_calls, &pthread_assignments);

    // Generate the output based on selected format
    let output = if opt.format == "json" {
        format_graph_as_json(&graph)
    } else {
        format_graph_as_dot(&graph)
    };
    
    // Write to file or stdout
    if let Some(output_path) = opt.output {
        fs::write(&output_path, output)
            .with_context(|| format!("Failed to write to file: {:?}", output_path))?;
        println!("Graph written to {:?}", output_path);
    } else {
        println!("{}", output);
    }

    Ok(())
}

// Extract function calls directly from the source code as a fallback mechanism
fn extract_function_calls_from_source(source_code: &str) -> Vec<(String, String)> {
    let mut calls = Vec::new();
    
    // First identify all functions
    let func_regex = Regex::new(r"(?m)^(?:\w+\s+)+(\w+)\s*\([^)]*\)\s*\{").unwrap();
    let func_names: Vec<String> = func_regex.captures_iter(source_code)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect();
    
    // Then for each function, find all function calls within it
    for func_name in &func_names {
        // Find the function body
        let func_pattern = format!(r"(?m)^(?:\w+\s+)+{}\s*\([^)]*\)\s*\{{", regex::escape(func_name));
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

fn is_standard_library_function(name: &str) -> bool {
    let std_functions = [
        "printf", "sprintf", "fprintf", "snprintf", "vprintf", "vsprintf", "vfprintf", "vsnprintf",
        "scanf", "sscanf", "fscanf", "vscanf", "vsscanf", "vfscanf",
        "malloc", "calloc", "realloc", "aligned_alloc", "free",
        "exit", "abort", "atexit", "_Exit", 
        "system", "getenv", "setenv", "putenv", "unsetenv",
        "time", "clock", "difftime", "mktime", "asctime", "ctime", "gmtime", "localtime", "strftime",
        "rand", "srand", "rand_r",
        "atoi", "atol", "atoll", "strtol", "strtoll", "strtoul", "strtoull",
        "memcpy", "memmove", "memset", "memcmp", "memchr", "memccpy",
        "strlen", "strnlen", "strcpy", "strncpy", "strcat", "strncat", "strcmp", "strncmp", 
        "strchr", "strrchr", "strstr", "strtok",
        "fopen", "fclose", "fflush", "fread", "fwrite", "fseek", "ftell", "fgetpos", "fsetpos",
    ];
    
    std_functions.contains(&name)
}

fn find_all_functions(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
) {
    // Skip system headers
    if is_system_entity(&entity) {
        return;
    }
    
    match entity.get_kind() {
        EntityKind::FunctionDecl => {
            if let Some(name) = entity.get_name() {
                let is_main = name == "main";
                let usr = format!("{:?}", entity.get_usr());
                
                // Create function node if not already in the map
                if !node_map.contains_key(&name) {
                    let node_type = if is_main { NodeType::Main } else { NodeType::Function };
                    let line = get_line_number(&entity);
                    
                    let node_idx = graph.add_node(Node {
                        name: name.clone(),
                        kind: node_type,
                        line,
                        usr: Some(usr.clone()),
                    });
                    
                    node_map.insert(name.clone(), node_idx);
                    
                    // Store USR for precise matching
                    if !usr.is_empty() {
                        usr_map.insert(usr, node_idx);
                    }
                }
            }
        },
        _ => {
            // Recursively process children
            for child in entity.get_children() {
                find_all_functions(child, graph, node_map, usr_map);
            }
        }
    }
}

fn analyze_program(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
) {
    // Skip system headers and already processed entities
    if is_system_entity(&entity) {
        return;
    }
    
    let entity_id = get_entity_id(&entity);
    if processed.contains(&entity_id) {
        return;
    }
    
    processed.insert(entity_id);
    
    // Debug output
    if debug {
        if let Some(name) = entity.get_name() {
            println!("Processing entity: {} ({:?})", name, entity.get_kind());
        } else {
            println!("Processing entity: {:?}", entity.get_kind());
        }
    }
    
    match entity.get_kind() {
        EntityKind::FunctionDecl => {
            process_function(entity, graph, node_map, usr_map, processed, content, debug);
        },
        EntityKind::VarDecl => {
            process_variable(entity, graph, node_map);
        },
        EntityKind::IfStmt => {
            process_if_statement(entity, graph, node_map, usr_map, processed, content, debug);
        },
        EntityKind::ForStmt => {
            process_loop(entity, graph, node_map, usr_map, processed, content, NodeType::ForLoop, debug);
        },
        EntityKind::WhileStmt => {
            process_loop(entity, graph, node_map, usr_map, processed, content, NodeType::WhileLoop, debug);
        },
        _ => {
            // Recursively process children
            for child in entity.get_children() {
                analyze_program(child, graph, node_map, usr_map, processed, content, debug);
            }
        }
    }
}

fn process_function(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
) {
    if let Some(name) = entity.get_name() {
        let is_main = name == "main";
        let line = get_line_number(&entity);
        
        // Get or create a node for this function
        let node_idx = if let Some(&idx) = node_map.get(&name) {
            idx
        } else {
            let node_type = if is_main { NodeType::Main } else { NodeType::Function };
            let usr = format!("{:?}", entity.get_usr());
            
            let node_idx = graph.add_node(Node {
                name: name.clone(),
                kind: node_type,
                line,
                usr: Some(usr.clone()),
            });
            
            node_map.insert(name.clone(), node_idx);
            
            // Store USR for precise matching
            if !usr.is_empty() {
                usr_map.insert(usr, node_idx);
            }
            
            node_idx
        };
        
        // Process function parameters
        for param in entity.get_arguments().unwrap_or_default() {
            if let Some(param_name) = param.get_name() {
                let param_type = param.get_type().unwrap().get_display_name();
                let is_buffer = param_type.contains("char *") || param_type.contains("char*");
                
                let node_type = if is_buffer { 
                    NodeType::BufferParameter 
                } else { 
                    NodeType::Parameter 
                };
                
                let param_label = if is_buffer {
                    format!("BufferParam: {} ({})", param_name, param_type)
                } else {
                    format!("Param: {} ({})", param_name, param_type)
                };
                
                let param_idx = graph.add_node(Node {
                    name: param_label,
                    kind: node_type,
                    line: get_line_number(&param),
                    usr: None,
                });
                
                // Add edge from function to parameter
                graph.add_edge(
                    node_idx,
                    param_idx,
                    Edge { kind: EdgeType::Contains },
                );
                
                // Store parameter in node map for later reference
                node_map.insert(format!("{}_{}", name, param_name), param_idx);
            }
        }
        
        // Process function body
        if let Some(body) = entity.get_children().iter().find(|c| c.get_kind() == EntityKind::CompoundStmt) {
            // Create a basic block for the function body
            let bb_idx = graph.add_node(Node {
                name: "BasicBlock: entry".to_string(),
                kind: NodeType::BasicBlock,
                line: get_line_number(body),
                usr: None,
            });
            
            // Connect function to basic block
            graph.add_edge(
                node_idx,
                bb_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            // Process body contents
            for child in body.get_children() {
                process_statement(child, bb_idx, graph, node_map, usr_map, processed, content, debug);
            }
        }
    }
}

fn process_statement(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
) {
    match entity.get_kind() {
        EntityKind::CallExpr => {
            process_call_expression(entity, parent_idx, graph, node_map, usr_map, debug);
        },
        EntityKind::DeclStmt => {
            // Handle local variable declarations
            for child in entity.get_children() {
                if child.get_kind() == EntityKind::VarDecl {
                    process_variable(child, graph, node_map);
                    
                    if let Some(var_name) = child.get_name() {
                        if let Some(&var_idx) = node_map.get(&var_name) {
                            // Connect parent to variable
                            graph.add_edge(
                                parent_idx,
                                var_idx,
                                Edge { kind: EdgeType::Contains },
                            );
                        }
                    }
                }
            }
        },
        EntityKind::IfStmt => {
            let if_idx = process_if_statement(entity, graph, node_map, usr_map, processed, content, debug);
            
            // Connect parent to if statement
            if let Some(idx) = if_idx {
                graph.add_edge(
                    parent_idx,
                    idx,
                    Edge { kind: EdgeType::Contains },
                );
            }
        },
        EntityKind::ForStmt => {
            let loop_idx = process_loop(entity, graph, node_map, usr_map, processed, content, NodeType::ForLoop, debug);
            
            // Connect parent to for loop
            if let Some(idx) = loop_idx {
                graph.add_edge(
                    parent_idx,
                    idx,
                    Edge { kind: EdgeType::Contains },
                );
            }
        },
        EntityKind::WhileStmt => {
            let loop_idx = process_loop(entity, graph, node_map, usr_map, processed, content, NodeType::WhileLoop, debug);
            
            // Connect parent to while loop
            if let Some(idx) = loop_idx {
                graph.add_edge(
                    parent_idx,
                    idx,
                    Edge { kind: EdgeType::Contains },
                );
            }
        },
        EntityKind::CompoundStmt => {
            // Process nested blocks
            for child in entity.get_children() {
                process_statement(child, parent_idx, graph, node_map, usr_map, processed, content, debug);
            }
        },
        EntityKind::BinaryOperator | EntityKind::UnaryOperator | EntityKind::DeclRefExpr => {
            // Process binary operations, looking for variable references
            for child in entity.get_children() {
                if child.get_kind() == EntityKind::DeclRefExpr {
                    if let Some(var_name) = child.get_name() {
                        if let Some(&var_idx) = node_map.get(&var_name) {
                            // Add an edge showing that this statement uses the variable
                            graph.add_edge(
                                parent_idx,
                                var_idx,
                                Edge { kind: EdgeType::Uses },
                            );
                        }
                    }
                } else {
                    process_statement(child, parent_idx, graph, node_map, usr_map, processed, content, debug);
                }
            }
        },
        _ => {
            // Process other statement types or recurse into children
            for child in entity.get_children() {
                process_statement(child, parent_idx, graph, node_map, usr_map, processed, content, debug);
            }
        }
    }
}

fn process_call_expression(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    debug: bool,
) {
    // First look for a direct reference to the called function
    let called_entity = entity.get_reference();
    
    if debug {
        println!("Processing call expression: {:?}", entity);
        if let Some(ref entity) = called_entity {
            println!("  Called entity: {:?} (name: {:?})", entity.get_kind(), entity.get_name());
        } else {
            println!("  No called entity reference found.");
        }
    }
    
    // Try to extract the function name
    let function_name = if let Some(ref called) = called_entity {
        called.get_name()
    } else {
        // If no direct reference, try to extract from the expression
        extract_function_name_from_call(&entity)
    };
    
    if let Some(function_name) = function_name {
        if debug {
            println!("  Function name: {}", function_name);
        }
        
        let is_unsafe = is_unsafe_function(&function_name);
        
        // Create node for the function call
        let node_type = if is_unsafe { 
            NodeType::UnsafeCall 
        } else { 
            NodeType::Call 
        };
        
        let call_label = if is_unsafe {
            format!("Unsafe: {}", function_name)
        } else {
            format!("Call: {}", function_name)
        };
        
        let usr = if let Some(ref called) = called_entity {
            Some(format!("{:?}", called.get_usr()))
        } else {
            None
        };
        
        let call_idx = graph.add_node(Node {
            name: call_label,
            kind: node_type,
            line: get_line_number(&entity),
            usr: usr.clone(),
        });
        
        // Connect parent to call
        graph.add_edge(
            parent_idx,
            call_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        // Try to find the called function in our maps
        let func_idx = if let Some(ref usr_str) = usr {
            if !usr_str.is_empty() {
                usr_map.get(usr_str).cloned()
            } else {
                None
            }
        } else {
            None
        }.or_else(|| node_map.get(&function_name).cloned());
        
        // Connect call to the actual function if it exists in our graph
        if let Some(func_idx) = func_idx {
            graph.add_edge(
                call_idx,
                func_idx,
                Edge { kind: EdgeType::Calls },
            );
            
            if debug {
                println!("  Added 'calls' edge from {} to {}", call_idx.index(), func_idx.index());
            }
        } else if debug {
            println!("  Could not find function definition for: {}", function_name);
        }
        
        // For unsafe calls, create another node that controls this one
        if is_unsafe {
            let unsafe_idx = graph.add_node(Node {
                name: format!("Unsafe: {}", function_name),
                kind: NodeType::UnsafeCall,
                line: None,
                usr: None,
            });
            
            graph.add_edge(
                unsafe_idx,
                call_idx,
                Edge { kind: EdgeType::Controls },
            );
        }
        
        // Process call arguments to track data flow
        for arg in entity.get_arguments().unwrap_or_default() {
            process_call_argument(&arg, call_idx, graph, node_map);
        }
    }
}

fn extract_function_name_from_call(entity: &Entity) -> Option<String> {
    // Try to extract the function name from the first child
    let children = entity.get_children();
    if !children.is_empty() {
        match children[0].get_kind() {
            EntityKind::DeclRefExpr => children[0].get_name(),
            _ => None,
        }
    } else {
        None
    }
}

fn process_call_argument(
    arg: &Entity,
    call_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
) {
    // Try to find references to variables/parameters in the argument
    let mut current = arg.clone();
    
    // Traverse through the AST looking for variable references
    loop {
        match current.get_kind() {
            EntityKind::DeclRefExpr => {
                if let Some(var_name) = current.get_name() {
                    // Try to find this variable in our node map
                    if let Some(&var_idx) = node_map.get(&var_name) {
                        // Add "uses" edge
                        graph.add_edge(
                            call_idx,
                            var_idx,
                            Edge { kind: EdgeType::Uses },
                        );
                    }
                }
                break;
            },
            _ => {
                // Check if there are any children to traverse
                let children = current.get_children();
                if children.is_empty() {
                    break;
                }
                // Just take the first child for simplicity
                current = children[0].clone();
            }
        }
    }
}

fn process_variable(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
) {
    if let Some(name) = entity.get_name() {
        let var_type = entity.get_type().unwrap().get_display_name();
        let is_buffer = var_type.contains("char *") || var_type.contains("char*");
        
        let node_type = if is_buffer { 
            NodeType::BufferParameter 
        } else { 
            NodeType::Variable 
        };
        
        let var_label = if is_buffer {
            format!("BufferParam: {} ({})", name, var_type)
        } else {
            format!("Var: {}", name)
        };
        
        let var_idx = graph.add_node(Node {
            name: var_label,
            kind: node_type,
            line: get_line_number(&entity),
            usr: None,
        });
        
        node_map.insert(name, var_idx);
    }
}

fn process_if_statement(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
) -> Option<NodeIndex> {
    let if_idx = graph.add_node(Node {
        name: "If statement".to_string(),
        kind: NodeType::IfStatement,
        line: get_line_number(&entity),
        usr: None,
    });
    
    // Process the condition (to track variable uses)
    if let Some(cond) = entity.get_children().iter().find(|c| 
        c.get_kind() == EntityKind::BinaryOperator || 
        c.get_kind() == EntityKind::UnaryOperator ||
        c.get_kind() == EntityKind::DeclRefExpr
    ) {
        for child in cond.get_children() {
            if child.get_kind() == EntityKind::DeclRefExpr {
                if let Some(var_name) = child.get_name() {
                    if let Some(&var_idx) = node_map.get(&var_name) {
                        graph.add_edge(
                            if_idx,
                            var_idx,
                            Edge { kind: EdgeType::Uses },
                        );
                    }
                }
            }
        }
    }
    
    // Process the then branch
    if let Some(then_branch) = entity.get_children().iter().find(|c| c.get_kind() == EntityKind::CompoundStmt) {
        let then_bb_idx = graph.add_node(Node {
            name: "BasicBlock: then".to_string(),
            kind: NodeType::BasicBlock,
            line: get_line_number(then_branch),
            usr: None,
        });
        
        graph.add_edge(
            if_idx,
            then_bb_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        for child in then_branch.get_children() {
            process_statement(child, then_bb_idx, graph, node_map, usr_map, processed, content, debug);
        }
    }
    
    // Process the else branch if it exists
    let children = entity.get_children();
    if children.len() >= 3 {
        let else_branch = &children[2];
        if else_branch.get_kind() == EntityKind::CompoundStmt {
            let else_bb_idx = graph.add_node(Node {
                name: "BasicBlock: else".to_string(),
                kind: NodeType::BasicBlock,
                line: get_line_number(else_branch),
                usr: None,
            });
            
            graph.add_edge(
                if_idx,
                else_bb_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            for child in else_branch.get_children() {
                process_statement(child, else_bb_idx, graph, node_map, usr_map, processed, content, debug);
            }
        }
    }
    
    Some(if_idx)
}

fn process_loop(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    loop_type: NodeType,
    debug: bool,
) -> Option<NodeIndex> {
    let loop_name = match loop_type {
        NodeType::ForLoop => "For loop",
        NodeType::WhileLoop => "While loop",
        _ => "Loop",
    };
    
    let loop_idx = graph.add_node(Node {
        name: loop_name.to_string(),
        kind: loop_type,
        line: get_line_number(&entity),
        usr: None,
    });
    
    // Process loop condition variables
    for child in entity.get_children() {
        if child.get_kind() == EntityKind::BinaryOperator || 
           child.get_kind() == EntityKind::UnaryOperator ||
           child.get_kind() == EntityKind::DeclRefExpr {
            for subchild in child.get_children() {
                if subchild.get_kind() == EntityKind::DeclRefExpr {
                    if let Some(var_name) = subchild.get_name() {
                        if let Some(&var_idx) = node_map.get(&var_name) {
                            graph.add_edge(
                                loop_idx,
                                var_idx,
                                Edge { kind: EdgeType::Uses },
                            );
                        }
                    }
                }
            }
        }
    }
    
    // Process loop body
    if let Some(body) = entity.get_children().iter().find(|c| c.get_kind() == EntityKind::CompoundStmt) {
        let body_idx = graph.add_node(Node {
            name: "BasicBlock: loop body".to_string(),
            kind: NodeType::BasicBlock,
            line: get_line_number(body),
            usr: None,
        });
        
        graph.add_edge(
            loop_idx,
            body_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        for child in body.get_children() {
            process_statement(child, body_idx, graph, node_map, usr_map, processed, content, debug);
        }
    }
    
    Some(loop_idx)
}


// Specialized function to extract pthread_create handler assignments
fn extract_pthread_assignments(source_code: &str) -> Vec<(String, String)> {
    let mut assignments = Vec::new();
    
    // First identify all functions
    let func_regex = Regex::new(r"(?m)^(?:\w+\s+)+(\w+)\s*\([^)]*\)\s*\{").unwrap();
    let func_names: Vec<String> = func_regex.captures_iter(source_code)
        .filter_map(|cap| cap.get(1).map(|m| m.as_str().to_string()))
        .collect();
    
    // Then for each function, look specifically for pthread_create calls
    for func_name in &func_names {
        // Find the function body
        let func_pattern = format!(r"(?m)^(?:\w+\s+)+{}\s*\([^)]*\)\s*\{{", regex::escape(func_name));
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
            let pthread_regex = Regex::new(
                r"pthread_create\s*\([^,]+,\s*[^,]*,\s*([a-zA-Z_][a-zA-Z0-9_]*)\s*,"
            ).unwrap();
            
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

// Fix any disconnected calls by checking call nodes that should be connected to functions
fn fix_disconnected_calls(
    graph: &mut DiGraph<Node, Edge>,
    node_map: &HashMap<String, NodeIndex>,
    usr_map: &HashMap<String, NodeIndex>,
    extracted_calls: &[(String, String)],
    pthread_assignments: &[(String, String)],
) {
    let mut new_edges = Vec::new();
    
    // First, fix edges based on our AST processing
    for node_idx in graph.node_indices() {
        let node = &graph[node_idx];
        
        if node.kind == NodeType::Call || node.kind == NodeType::UnsafeCall {
            let function_name = if node.name.starts_with("Call: ") {
                node.name[6..].to_string()
            } else if node.name.starts_with("Unsafe: ") {
                node.name[8..].to_string()
            } else {
                continue;
            };
            
            // Check if this call is already connected to a function
            let already_connected = graph.edges(node_idx)
                .any(|edge| graph[edge.id()].kind == EdgeType::Calls);
            
            if !already_connected {
                // Try to find the function this call should connect to
                if let Some(&func_idx) = node_map.get(&function_name) {
                    new_edges.push((node_idx, func_idx));
                }
            }
        }
    }
    
    // Then, use the source code extracted calls to create any missing edges
    let mut caller_to_node = HashMap::new();
    
    // Create a mapping of function names to their basic block nodes
    for node_idx in graph.node_indices() {
        let node = &graph[node_idx];
        
        if node.kind == NodeType::Function || node.kind == NodeType::Main {
            // Find all basic blocks that are children of this function
            let basic_blocks: Vec<NodeIndex> = graph.edges(node_idx)
                .filter_map(|edge| {
                    if graph[edge.id()].kind == EdgeType::Contains {
                        let target = edge.target();
                        if graph[target].kind == NodeType::BasicBlock {
                            Some(target)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .collect();
            
            for &bb_idx in &basic_blocks {
                caller_to_node.insert(node.name.clone(), bb_idx);
            }
        }
    }
    
    // For each extracted call, make sure there's a corresponding edge
    for (caller, callee) in extracted_calls {
        // Skip standard library functions
        if is_standard_library_function(callee) {
            continue;
        }
        
        // Get the function and basic block nodes
        if let (Some(&func_idx), Some(&caller_block)) = (node_map.get(callee), caller_to_node.get(caller)) {
            // Check if there's already a call to this function from this caller
            let has_call = graph.edges(caller_block)
                .any(|edge| {
                    if graph[edge.id()].kind == EdgeType::Contains {
                        let target = edge.target();
                        if (graph[target].kind == NodeType::Call || graph[target].kind == NodeType::UnsafeCall) && 
                           (graph[target].name == format!("Call: {}", callee) || 
                            graph[target].name == format!("Unsafe: {}", callee)) {
                            // Check if this call is connected to the function
                            graph.edges(target).any(|call_edge| {
                                graph[call_edge.id()].kind == EdgeType::Calls && 
                                call_edge.target() == func_idx
                            })
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                });
            
            if !has_call {
                // Create a new call node
                let is_unsafe = is_unsafe_function(callee);
                let node_type = if is_unsafe { NodeType::UnsafeCall } else { NodeType::Call };
                let call_label = if is_unsafe { format!("Unsafe: {}", callee) } else { format!("Call: {}", callee) };
                
                let call_idx = graph.add_node(Node {
                    name: call_label,
                    kind: node_type,
                    line: None,
                    usr: None,
                });
                
                // Connect everything
                graph.add_edge(
                    caller_block,
                    call_idx,
                    Edge { kind: EdgeType::Contains },
                );
                
                graph.add_edge(
                    call_idx,
                    func_idx,
                    Edge { kind: EdgeType::Calls },
                );
            }
        }
    }
    
    // Add the new edges from our AST processing
    for (from, to) in new_edges {
        graph.add_edge(
            from,
            to,
            Edge { kind: EdgeType::Calls },
        );
    }

    // Add special handling for pthread function assignments
    for (caller, handler_func) in pthread_assignments {
        if let (Some(&caller_idx), Some(&handler_idx)) = (node_map.get(caller), node_map.get(handler_func)) {
            // Find if there's already a relationship
            let already_connected = graph.edges(caller_idx)
                .flat_map(|edge| {
                    if graph[edge.id()].kind == EdgeType::Contains {
                        let target = edge.target();
                        if graph[target].kind == NodeType::BasicBlock {
                            // Check all children of this basic block
                            graph.edges(target)
                                .filter_map(|bb_edge| {
                                    if graph[bb_edge.id()].kind == EdgeType::Uses &&
                                       bb_edge.target() == handler_idx {
                                        Some(true)
                                    } else {
                                        None
                                    }
                                })
                                .next()
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                })
                .next()
                .is_some();
            
            if !already_connected {
                // Find the basic block for the caller
                let basic_blocks: Vec<NodeIndex> = graph.edges(caller_idx)
                    .filter_map(|edge| {
                        if graph[edge.id()].kind == EdgeType::Contains {
                            let target = edge.target();
                            if graph[target].kind == NodeType::BasicBlock {
                                Some(target)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    })
                    .collect();
                
                if let Some(&bb_idx) = basic_blocks.first() {
                    // Create a new node to represent the pthread_create call
                    let pthread_idx = graph.add_node(Node {
                        name: format!("Call: pthread_create"),
                        kind: NodeType::Call,
                        line: None,
                        usr: None,
                    });
                    
                    // Connect the call to the basic block
                    graph.add_edge(
                        bb_idx,
                        pthread_idx,
                        Edge { kind: EdgeType::Contains },
                    );
                    
                    // Create a References edge from pthread_create to the handler function
                    graph.add_edge(
                        pthread_idx,
                        handler_idx,
                        Edge { kind: EdgeType::References },
                    );
                }
            }
        }
    }
}

// Helper functions

fn get_entity_id(entity: &Entity) -> String {
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

fn is_system_entity(entity: &Entity) -> bool {
    if let Some(loc) = entity.get_location() {
        let file_path = loc.get_file_location().file
            .map(|f| f.get_path())
            .unwrap_or_default();
        
        let path_str = file_path.to_string_lossy();
        path_str.contains("/usr/include/") || 
        path_str.contains("/usr/lib/") ||
        path_str.contains("/usr/local/include/")
    } else {
        false
    }
}

fn is_unsafe_function(name: &str) -> bool {
    let unsafe_functions = [
        "strcpy", "strcat", "sprintf", "gets", "scanf", 
        "vsprintf", "memcpy", "memmove", "strncpy", "strncat",
    ];
    
    unsafe_functions.contains(&name)
}

fn get_line_number(entity: &Entity) -> Option<usize> {
    entity.get_location().map(|loc| {
        let file_loc = loc.get_file_location();
        file_loc.line as usize
    })
}

// Output formatting functions

fn format_graph_as_dot(graph: &DiGraph<Node, Edge>) -> String {
    let mut output = String::from("digraph {\n");
    
    // Add global styling
    output.push_str("    graph [fontname=\"Arial\", rankdir=TB, splines=true];\n");
    output.push_str("    node [fontname=\"Arial\"];\n");
    output.push_str("    edge [fontname=\"Arial\"];\n\n");
    
    // Add nodes with different shapes based on type
    for node_idx in graph.node_indices() {
        let node = &graph[node_idx];
        let node_id = node_idx.index();
        
        // Determine shape and color based on node type
        let (shape, color, style) = match node.kind {
            NodeType::UnsafeCall => ("ellipse", "red", "filled"),
            NodeType::Call => ("ellipse", "purple", "filled"),
            NodeType::Main => ("ellipse", "green", "filled"),
            NodeType::Function => ("ellipse", "lightblue", "filled"),
            NodeType::BasicBlock => ("box", "red", "filled,rounded"),
            NodeType::Parameter => ("ellipse", "orange", "filled"),
            NodeType::BufferParameter => ("ellipse", "blue", "filled"),
            NodeType::Variable => ("ellipse", "green", "filled"),
            NodeType::IfStatement => ("diamond", "indigo", "filled"),
            NodeType::ForLoop => ("box", "lightblue", "filled,rounded"),
            NodeType::WhileLoop => ("box", "lightblue", "filled,rounded"),
        };
        
        output.push_str(&format!("    {} [label=\"{}\", shape={}, fillcolor=\"{}\", style=\"{}\"];\n", 
                                node_id, node.name, shape, color, style));
    }
    
    // Add edges with labels
    for edge_idx in graph.edge_indices() {
        let (source, target) = graph.edge_endpoints(edge_idx).unwrap();
        let source_id = source.index();
        let target_id = target.index();
        let edge = &graph[edge_idx];
        
        // Edge label based on type
        let label = match edge.kind {
            EdgeType::Calls => "calls",
            EdgeType::Contains => "contains",
            EdgeType::Uses => "uses",
            EdgeType::Defines => "defines",
            EdgeType::Controls => "controls", 
            EdgeType::References => "references",
        };
        
        // Edge color based on type
        let color = match edge.kind {
            EdgeType::Calls => "blue",
            EdgeType::Contains => "gray",
            EdgeType::Uses => "green",
            EdgeType::Defines => "purple",
            EdgeType::Controls => "red",
            EdgeType::References => "purple",
        };
        
        output.push_str(&format!("    {} -> {} [label=\"{}\", color=\"{}\"];\n", 
                                source_id, target_id, label, color));
    }
    
    output.push_str("}\n");
    output
}

fn format_graph_as_json(graph: &DiGraph<Node, Edge>) -> String {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_id_map: HashMap<NodeIndex, String> = HashMap::new();
    
    // Process nodes
    for node_idx in graph.node_indices() {
        let node = &graph[node_idx];
        let node_id = format!("{}_{}", node_type_to_prefix(&node.kind), node_idx.index());
        node_id_map.insert(node_idx, node_id.clone());
        
        // Map node type to group
        let group = match node.kind {
            NodeType::Function => "function",
            NodeType::Main => "main_function",
            NodeType::Variable => "variable",
            NodeType::Parameter => "param",
            NodeType::Call => "call",
            NodeType::UnsafeCall => "unsafe_call",
            NodeType::BufferParameter => "buffer_param",
            NodeType::BasicBlock => "basic",
            NodeType::IfStatement => "if_statement",
            NodeType::ForLoop => "for_loop",
            NodeType::WhileLoop => "while_loop",
        };
        
        nodes.push(json!({
            "id": node_id,
            "label": node.name,
            "group": group
        }));
    }
    
    // Process edges
    for edge_idx in graph.edge_indices() {
        let (source, target) = graph.edge_endpoints(edge_idx).unwrap();
        let source_id = node_id_map.get(&source).unwrap();
        let target_id = node_id_map.get(&target).unwrap();
        let edge = &graph[edge_idx];
        
        // Map edge type to label, color, and weight
        let (label, color, weight) = match edge.kind {
            EdgeType::Calls => ("calls", "blue", 2.0),
            EdgeType::Contains => ("contains", "gray", 1.0),
            EdgeType::Uses => ("uses", "green", 2.0),
            EdgeType::Defines => ("defines", "orange", 1.5),
            EdgeType::Controls => ("controls", "red", 3.0),
            EdgeType::References => ("references", "purple", 2.0),
        };
        
        edges.push(json!({
            "from": source_id,
            "to": target_id,
            "label": label,
            "weight": weight,
            "color": color,
            "dashes": false
        }));
    }
    
    // Build final JSON object
    let result = json!({
        "nodes": nodes,
        "edges": edges
    });
    
    serde_json::to_string_pretty(&result).unwrap()
}

// Helper function to map node types to ID prefixes
fn node_type_to_prefix(node_type: &NodeType) -> &'static str {
    match node_type {
        NodeType::Function => "func",
        NodeType::Main => "main",
        NodeType::Variable => "var",
        NodeType::Parameter => "param",
        NodeType::Call => "call",
        NodeType::UnsafeCall => "unsafe",
        NodeType::BufferParameter => "buffer",
        NodeType::BasicBlock => "block",
        NodeType::IfStatement => "if",
        NodeType::ForLoop => "for",
        NodeType::WhileLoop => "while",
    }
}