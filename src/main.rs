use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use petgraph::visit::EdgeRef;

use anyhow::{Context, Result};
use clang::{Entity, EntityKind, Index, TranslationUnit, Type, TypeKind};
use petgraph::dot::{Config, Dot};
use petgraph::graph::{DiGraph, NodeIndex};
use regex::Regex;
use structopt::StructOpt;
use serde_json::{json, Value};

// Node types represent the different kinds of entities in our graph
#[derive(Debug, Clone, PartialEq)]
enum NodeType {
    Function,           // Function definition
    Main,               // Main function (special case)
    Parameter,          // Function parameter
    BufferParameter,    // Buffer parameter (security risk)
    Variable,           // Variable declaration
    Pointer,            // Pointer variable
    Array,              // Array variable
    Call,               // Function call
    UnsafeCall,         // Call to unsafe function (security risk)
    BasicBlock,         // Code block
    IfStatement,        // If statement
    ForLoop,            // For loop
    WhileLoop,          // While loop
    Assignment,         // Variable assignment
    MemoryOp,           // Memory operation (malloc/free)
    Dereference,        // Pointer dereference
    AddressOf,          // Address-of operation
    Cast,               // Type cast
    StructAccess,       // Struct field access
    ArrayAccess,        // Array access
}

// Edge types represent the relationships between nodes
#[derive(Debug, Clone, PartialEq)]
enum EdgeType {
    Contains,   // Parent contains child
    Calls,      // Function call relationship
    Controls,   // Control relationship (unsafe -> function call)
    Uses,       // Usage relationship
    References, // References (e.g., function pointer)
    Assigns,    // Assignment relationship
    Points,     // Pointer points to
    Casts,      // Type cast relationship
    Accesses,   // Access relationship (struct/array)
    Allocates,  // Memory allocation
    Frees,      // Memory free
    Defines,            // Defines a function
}

// Encapsulate node information
#[derive(Debug, Clone)]
struct Node {
    name: String,
    kind: NodeType,
    line: Option<usize>,
    usr: Option<String>,
    type_info: Option<String>,
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
    
    /// Advanced memory tracking
    #[structopt(long)]
    memory_tracking: bool,
}

fn main() -> Result<()> {
    let opt = Opt::from_args();

    // Read the content of the C file
    let content = fs::read_to_string(&opt.input)
        .with_context(|| format!("Failed to read file: {:?}", opt.input))?;

    // Initialize Clang with more options for complete semantic analysis
    let clang = clang::Clang::new().unwrap();
    let index = clang::Index::new(&clang, true, true);
    
    // Use more clang options for better analysis
    let clang_args = vec![
        "-Wall".to_string(),
        "-I/usr/include".to_string(),
        "-I/usr/local/include".to_string(),
        "-std=c11".to_string(),         // Specify language standard
        "-x".to_string(), "c".to_string(), // Force C language
    ];
    
    // Parse with detailed options for deeper analysis
    let tu = index.parser(opt.input.to_str().unwrap())
        .arguments(&clang_args)
        .detailed_preprocessing_record(true)
        .skip_function_bodies(false)
        // .include_all_declarations(true)
        // .visit_implicit_code(true)
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
    
    // Extract pthread function assignments
    let pthread_assignments = extract_pthread_assignments(&content);
    if opt.debug {
        println!("Extracted pthread assignments:");
        for (caller, handler_func) in &pthread_assignments {
            println!("  {} assigns {} to pthread", caller, handler_func);
        }
    }

    // Build our graph
    let mut graph = DiGraph::<Node, Edge>::new();
    let mut node_map: HashMap<String, NodeIndex> = HashMap::new();
    let mut usr_map: HashMap<String, NodeIndex> = HashMap::new();
    
    // Track pointer-target relationships for memory operations
    let mut pointer_targets: HashMap<NodeIndex, NodeIndex> = HashMap::new();
    
    let mut processed_entities = HashSet::new();
    
    // First pass: identify all functions to ensure they're in the graph
    find_all_functions(tu.get_entity(), &mut graph, &mut node_map, &mut usr_map);
    
    // Second pass: process the entire AST and build relationships
    analyze_program(
        tu.get_entity(), 
        &mut graph, 
        &mut node_map, 
        &mut usr_map,
        &mut pointer_targets,
        &mut processed_entities, 
        &content, 
        opt.debug,
        opt.memory_tracking
    );
    
    // Post-process: ensure connections are properly established
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
                
                // Get function return type
                let return_type = entity.get_type()
                    .map(|t| t.get_result_type())
                    .flatten()
                    .map(|t| t.get_display_name())
                    .unwrap_or_else(|| "void".to_string());
                
                // Create function node if not already in the map
                if !node_map.contains_key(&name) {
                    let node_type = if is_main { NodeType::Main } else { NodeType::Function };
                    let line = get_line_number(&entity);
                    
                    let node_idx = graph.add_node(Node {
                        name: name.clone(),
                        kind: node_type,
                        line,
                        usr: Some(usr.clone()),
                        type_info: Some(return_type),
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
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
    memory_tracking: bool,
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
            process_function(entity, graph, node_map, usr_map, pointer_targets, processed, content, debug, memory_tracking);
        },
        EntityKind::VarDecl => {
            process_variable_decl(entity, graph, node_map, pointer_targets, debug);
        },
        EntityKind::IfStmt => {
            process_if_statement(entity, graph, node_map, usr_map, pointer_targets, processed, content, debug, memory_tracking);
        },
        EntityKind::ForStmt => {
            process_loop(entity, graph, node_map, usr_map, pointer_targets, processed, content, NodeType::ForLoop, debug, memory_tracking);
        },
        EntityKind::WhileStmt => {
            process_loop(entity, graph, node_map, usr_map, pointer_targets, processed, content, NodeType::WhileLoop, debug, memory_tracking);
        },
        _ => {
            // Recursively process children
            for child in entity.get_children() {
                analyze_program(child, graph, node_map, usr_map, pointer_targets, processed, content, debug, memory_tracking);
            }
        }
    }
}

fn process_function(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
    memory_tracking: bool,
) {
    if let Some(name) = entity.get_name() {
        let is_main = name == "main";
        let line = get_line_number(&entity);
        
        // Get function return type
        let return_type = entity.get_type()
            .map(|t| t.get_result_type())
            .flatten()
            .map(|t| t.get_display_name())
            .unwrap_or_else(|| "void".to_string());
        
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
                type_info: Some(return_type),
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
                let is_pointer = param_type.contains('*');
                
                let node_type = if is_buffer { 
                    NodeType::BufferParameter 
                } else if is_pointer {
                    NodeType::Pointer
                } else { 
                    NodeType::Parameter 
                };
                
                let param_label = if is_buffer {
                    format!("BufferParam: {} ({})", param_name, param_type)
                } else if is_pointer {
                    format!("Pointer: {} ({})", param_name, param_type)
                } else {
                    format!("Param: {} ({})", param_name, param_type)
                };
                
                let param_idx = graph.add_node(Node {
                    name: param_label,
                    kind: node_type,
                    line: get_line_number(&param),
                    usr: None,
                    type_info: Some(param_type),
                });
                
                // Add edge from function to parameter
                graph.add_edge(
                    node_idx,
                    param_idx,
                    Edge { kind: EdgeType::Contains },
                );
                
                // Store parameter in node map for later reference
                node_map.insert(format!("{}_{}", name, param_name), param_idx);
                node_map.insert(param_name, param_idx); // Also store just the name for local lookups
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
                type_info: None,
            });
            
            // Connect function to basic block
            graph.add_edge(
                node_idx,
                bb_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            // Process body contents
            for child in body.get_children() {
                process_statement(
                    child, 
                    bb_idx, 
                    graph, 
                    node_map, 
                    usr_map, 
                    pointer_targets,
                    processed, 
                    content, 
                    debug,
                    memory_tracking
                );
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
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
    memory_tracking: bool,
) {
    match entity.get_kind() {
        EntityKind::CallExpr => {
            process_call_expression(entity, parent_idx, graph, node_map, usr_map, pointer_targets, debug, memory_tracking);
        },
        EntityKind::DeclStmt => {
            // Handle local variable declarations
            for child in entity.get_children() {
                if child.get_kind() == EntityKind::VarDecl {
                    let var_idx = process_variable_decl(child, graph, node_map, pointer_targets, debug);
                    
                    if let Some(var_idx) = var_idx {
                        // Connect parent to variable
                        graph.add_edge(
                            parent_idx,
                            var_idx,
                            Edge { kind: EdgeType::Contains },
                        );
                    }
                }
            }
        },
        EntityKind::BinaryOperator => {
            process_binary_operator(entity, parent_idx, graph, node_map, pointer_targets, debug);
        },
        EntityKind::UnaryOperator => {
            process_unary_operator(entity, parent_idx, graph, node_map, pointer_targets, debug);
        },
        EntityKind::CompoundAssignOperator | EntityKind::CStyleCastExpr => {
            process_binary_operator(entity, parent_idx, graph, node_map, pointer_targets, debug);
        },
        EntityKind::IfStmt => {
            let if_idx = process_if_statement(entity, graph, node_map, usr_map, pointer_targets, processed, content, debug, memory_tracking);
            
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
            let loop_idx = process_loop(entity, graph, node_map, usr_map, pointer_targets, processed, content, NodeType::ForLoop, debug, memory_tracking);
            
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
            let loop_idx = process_loop(entity, graph, node_map, usr_map, pointer_targets, processed, content, NodeType::WhileLoop, debug, memory_tracking);
            
            // Connect parent to while loop
            if let Some(idx) = loop_idx {
                graph.add_edge(
                    parent_idx,
                    idx,
                    Edge { kind: EdgeType::Contains },
                );
            }
        },
        EntityKind::MemberRefExpr => {
            process_member_access(entity, parent_idx, graph, node_map, pointer_targets, debug);
        },
        EntityKind::ArraySubscriptExpr => {
            process_array_access(entity, parent_idx, graph, node_map, pointer_targets, debug);
        },
        EntityKind::CompoundStmt => {
            // Process nested blocks
            for child in entity.get_children() {
                process_statement(
                    child, 
                    parent_idx, 
                    graph, 
                    node_map, 
                    usr_map, 
                    pointer_targets,
                    processed, 
                    content, 
                    debug,
                    memory_tracking
                );
            }
        },
        EntityKind::DeclRefExpr => {
            // Handle variable references
            if let Some(var_name) = entity.get_name() {
                if let Some(&var_idx) = node_map.get(&var_name) {
                    // Add an edge showing that this statement uses the variable
                    graph.add_edge(
                        parent_idx,
                        var_idx,
                        Edge { kind: EdgeType::Uses },
                    );
                }
            }
        },
        _ => {
            // Process other statement types or recurse into children
            for child in entity.get_children() {
                process_statement(
                    child, 
                    parent_idx, 
                    graph, 
                    node_map, 
                    usr_map, 
                    pointer_targets,
                    processed, 
                    content, 
                    debug,
                    memory_tracking
                );
            }
        }
    }
}

fn process_variable_decl(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) -> Option<NodeIndex> {
    if let Some(name) = entity.get_name() {
        let var_type = entity.get_type().unwrap().get_display_name();
        let is_buffer = var_type.contains("char *") || var_type.contains("char*");
        let is_pointer = var_type.contains('*');
        let is_array = var_type.contains('[') && var_type.contains(']');
        
        let node_type = if is_buffer { 
            NodeType::BufferParameter 
        } else if is_pointer {
            NodeType::Pointer
        } else if is_array {
            NodeType::Array
        } else { 
            NodeType::Variable 
        };
        
        let var_label = if is_buffer {
            format!("BufferParam: {} ({})", name, var_type)
        } else if is_pointer {
            format!("Pointer: {} ({})", name, var_type)
        } else if is_array {
            format!("Array: {} ({})", name, var_type)
        } else {
            format!("Var: {}", name)
        };
        
        let var_idx = graph.add_node(Node {
            name: var_label,
            kind: node_type,
            line: get_line_number(&entity),
            usr: None,
            type_info: Some(var_type),
        });
        
        node_map.insert(name, var_idx);
        
        // Check for initializer
        if let Some(init) = entity.get_children().iter().find(|c| 
            c.get_kind() == EntityKind::BinaryOperator || 
            c.get_kind() == EntityKind::CallExpr ||
            c.get_kind() == EntityKind::UnaryOperator ||
            c.get_kind() == EntityKind::IntegerLiteral ||
            c.get_kind() == EntityKind::StringLiteral ||
            c.get_kind() == EntityKind::DeclRefExpr) 
        {
            // Process initializer
            process_initializer(*init, var_idx, graph, node_map, pointer_targets, debug);
        }
        
        return Some(var_idx);
    }
    None
}

fn process_initializer(
    entity: Entity,
    var_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) {
    match entity.get_kind() {
        EntityKind::CallExpr => {
            // Handle initialization with function call
            if let Some(called_entity) = entity.get_reference() {
                if let Some(function_name) = called_entity.get_name() {
                    // Check if this is a memory allocation function
                    if function_name == "malloc" || function_name == "calloc" || function_name == "realloc" {
                        if debug {
                            println!("Memory allocation detected in variable initialization");
                        }
                        
                        // Create a memory operation node
                        let mem_op_idx = graph.add_node(Node {
                            name: format!("MemoryOp: {}", function_name),
                            kind: NodeType::MemoryOp,
                            line: get_line_number(&entity),
                            usr: None,
                            type_info: None,
                        });
                        
                        // Connect variable to memory operation
                        graph.add_edge(
                            var_idx,
                            mem_op_idx,
                            Edge { kind: EdgeType::Allocates },
                        );
                    }
                }
            }
            
            // Recursively process call arguments to track data flow
            for arg in entity.get_arguments().unwrap_or_default() {
                process_function_pointer_references(arg, var_idx, graph, node_map, debug);
            }
        },
        EntityKind::DeclRefExpr => {
            // Handle initialization with another variable
            if let Some(ref_name) = entity.get_name() {
                if let Some(&ref_idx) = node_map.get(&ref_name) {
                    // Add edge showing the variable is initialized from another
                    graph.add_edge(
                        var_idx,
                        ref_idx,
                        Edge { kind: EdgeType::Uses },
                    );
                    
                    // If the target is a pointer, record this relationship
                    if graph[ref_idx].kind == NodeType::Pointer || 
                       graph[ref_idx].kind == NodeType::BufferParameter {
                        pointer_targets.insert(var_idx, ref_idx);
                    }
                }
            }
        },
        EntityKind::UnaryOperator => {
            // Check for address-of operator
            let token = entity.get_display_name();
            if token == Some("&".to_string()) {
                if debug {
                    println!("Address-of operator detected in initialization");
                }
                
                // Find the variable being referenced
                for child in entity.get_children() {
                    if child.get_kind() == EntityKind::DeclRefExpr {
                        if let Some(ref_name) = child.get_name() {
                            if let Some(&ref_idx) = node_map.get(&ref_name) {
                                // Add edge showing the pointer points to the variable
                                graph.add_edge(
                                    var_idx,
                                    ref_idx,
                                    Edge { kind: EdgeType::Points },
                                );
                                
                                // Record this relationship
                                pointer_targets.insert(var_idx, ref_idx);
                            }
                        }
                    }
                }
            }
        },
        _ => {
            // Process children for other initializer types
            for child in entity.get_children() {
                process_initializer(child, var_idx, graph, node_map, pointer_targets, debug);
            }
        }
    }
}

fn process_binary_operator(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) {
    // Check if this is an assignment
    let token = entity.get_display_name();
    if token == Some("=".to_string()) {
        // Get left and right hand sides
        let children = entity.get_children();
        if children.len() >= 2 {
            let lhs = &children[0];
            let rhs = &children[1];
            
            // Handle left-hand side (target)
            let target_idx = if lhs.get_kind() == EntityKind::DeclRefExpr {
                if let Some(var_name) = lhs.get_name() {
                    node_map.get(&var_name).cloned()
                } else {
                    None
                }
            } else {
                None
            };
            
            if let Some(target_idx) = target_idx {
                // Create an assignment node
                let assign_idx = graph.add_node(Node {
                    name: format!("Assignment"),
                    kind: NodeType::Assignment,
                    line: get_line_number(&entity),
                    usr: None,
                    type_info: None,
                });
                
                // Connect parent to assignment
                graph.add_edge(
                    parent_idx,
                    assign_idx,
                    Edge { kind: EdgeType::Contains },
                );
                
                // Connect assignment to target
                graph.add_edge(
                    assign_idx,
                    target_idx,
                    Edge { kind: EdgeType::Assigns },
                );
                
                // Handle right-hand side (value)
                process_assignment_value(*rhs, assign_idx, target_idx, graph, node_map, pointer_targets, debug);
            }
        }
    } else {
        // For non-assignment binary operators, process operands
        for child in entity.get_children() {
            process_statement(
                child, 
                parent_idx, 
                graph, 
                node_map, 
                &mut HashMap::new(),  // We don't need USR tracking here
                pointer_targets,
                &mut HashSet::new(),  // No need to track processed nodes 
                "",                   // No need for source content
                debug,
                false                 // No need for memory tracking
            );
        }
    }
}

fn process_assignment_value(
    entity: Entity,
    assign_idx: NodeIndex,
    target_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) {
    match entity.get_kind() {
        EntityKind::CallExpr => {
            // Handle assignment from function call
            if let Some(called_entity) = entity.get_reference() {
                if let Some(function_name) = called_entity.get_name() {
                    // Check if this is a memory allocation function
                    if function_name == "malloc" || function_name == "calloc" || function_name == "realloc" {
                        if debug {
                            println!("Memory allocation detected in assignment");
                        }
                        
                        // Create a memory operation node
                        let mem_op_idx = graph.add_node(Node {
                            name: format!("MemoryOp: {}", function_name),
                            kind: NodeType::MemoryOp,
                            line: get_line_number(&entity),
                            usr: None,
                            type_info: None,
                        });
                        
                        // Connect assignment to memory operation
                        graph.add_edge(
                            assign_idx,
                            mem_op_idx,
                            Edge { kind: EdgeType::Uses },
                        );
                        
                        // Connect target to memory operation
                        graph.add_edge(
                            target_idx,
                            mem_op_idx,
                            Edge { kind: EdgeType::Allocates },
                        );
                    }
                }
            }
            
            // Process function call normally
            process_call_expression(
                entity, 
                assign_idx, 
                graph, 
                node_map, 
                &mut HashMap::new(),
                pointer_targets,
                debug,
                false
            );
        },
        EntityKind::DeclRefExpr => {
            // Handle assignment from another variable
            if let Some(ref_name) = entity.get_name() {
                if let Some(&ref_idx) = node_map.get(&ref_name) {
                    // Add edge showing the value comes from another variable
                    graph.add_edge(
                        assign_idx,
                        ref_idx,
                        Edge { kind: EdgeType::Uses },
                    );
                    
                    // If the source is a pointer, record this relationship
                    if graph[ref_idx].kind == NodeType::Pointer || 
                       graph[ref_idx].kind == NodeType::BufferParameter {
                        pointer_targets.insert(target_idx, ref_idx);
                    }
                }
            }
        },
        EntityKind::UnaryOperator => {
            // Check for address-of operator
            let token = entity.get_display_name();
            if token == Some("&".to_string()) {
                if debug {
                    println!("Address-of operator detected in assignment");
                }
                
                // Find the variable being referenced
                for child in entity.get_children() {
                    if child.get_kind() == EntityKind::DeclRefExpr {
                        if let Some(ref_name) = child.get_name() {
                            if let Some(&ref_idx) = node_map.get(&ref_name) {
                                // Add edge showing the pointer points to the variable
                                graph.add_edge(
                                    target_idx,
                                    ref_idx,
                                    Edge { kind: EdgeType::Points },
                                );
                                
                                // Record this relationship
                                pointer_targets.insert(target_idx, ref_idx);
                            }
                        }
                    }
                }
            }
        },
        _ => {
            // Process children for other value types
            for child in entity.get_children() {
                if child.get_kind() == EntityKind::DeclRefExpr {
                    if let Some(ref_name) = child.get_name() {
                        if let Some(&ref_idx) = node_map.get(&ref_name) {
                            // Add edge showing the value uses this variable
                            graph.add_edge(
                                assign_idx,
                                ref_idx,
                                Edge { kind: EdgeType::Uses },
                            );
                        }
                    }
                } else {
                    process_assignment_value(child, assign_idx, target_idx, graph, node_map, pointer_targets, debug);
                }
            }
        }
    }
}

fn process_unary_operator(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) {
    // Check for pointer dereference or address-of
    let token = entity.get_display_name();
    
    if token == Some("*".to_string()) {
        // Pointer dereference
        if debug {
            println!("Pointer dereference detected");
        }
        
        // Create a dereference node
        let deref_idx = graph.add_node(Node {
            name: format!("Dereference"),
            kind: NodeType::Dereference,
            line: get_line_number(&entity),
            usr: None,
            type_info: None,
        });
        
        // Connect parent to dereference
        graph.add_edge(
            parent_idx,
            deref_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        // Find the pointer being dereferenced
        for child in entity.get_children() {
            if child.get_kind() == EntityKind::DeclRefExpr {
                if let Some(ptr_name) = child.get_name() {
                    if let Some(&ptr_idx) = node_map.get(&ptr_name) {
                        // Add edge showing the dereference uses the pointer
                        graph.add_edge(
                            deref_idx,
                            ptr_idx,
                            Edge { kind: EdgeType::Uses },
                        );
                        
                        // If we know what this pointer points to, add that connection
                        if let Some(&target_idx) = pointer_targets.get(&ptr_idx) {
                            graph.add_edge(
                                deref_idx,
                                target_idx,
                                Edge { kind: EdgeType::Accesses },
                            );
                        }
                    }
                }
            } else {
                // Recurse for complex dereferences
                process_statement(
                    child, 
                    deref_idx, 
                    graph, 
                    node_map, 
                    &mut HashMap::new(),
                    pointer_targets,
                    &mut HashSet::new(),
                    "",
                    debug,
                    false
                );
            }
        }
    } else if token == Some("&".to_string()) {
        // Address-of operator
        if debug {
            println!("Address-of operator detected");
        }
        
        // Create an address-of node
        let addr_idx = graph.add_node(Node {
            name: format!("AddressOf"),
            kind: NodeType::AddressOf,
            line: get_line_number(&entity),
            usr: None,
            type_info: None,
        });
        
        // Connect parent to address-of
        graph.add_edge(
            parent_idx,
            addr_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        // Find the variable being referenced
        for child in entity.get_children() {
            if child.get_kind() == EntityKind::DeclRefExpr {
                if let Some(var_name) = child.get_name() {
                    if let Some(&var_idx) = node_map.get(&var_name) {
                        // Add edge showing the address-of uses the variable
                        graph.add_edge(
                            addr_idx,
                            var_idx,
                            Edge { kind: EdgeType::Uses },
                        );
                    }
                }
            } else {
                // Recurse for complex address-of expressions
                process_statement(
                    child, 
                    addr_idx, 
                    graph, 
                    node_map, 
                    &mut HashMap::new(),
                    pointer_targets,
                    &mut HashSet::new(),
                    "",
                    debug,
                    false
                );
            }
        }
    } else {
        // For other unary operators, just process operand
        for child in entity.get_children() {
            process_statement(
                child, 
                parent_idx, 
                graph, 
                node_map, 
                &mut HashMap::new(),
                pointer_targets,
                &mut HashSet::new(),
                "",
                debug,
                false
            );
        }
    }
}

fn process_member_access(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) {
    if debug {
        println!("Processing struct/union member access");
    }
    
    // Extract member name
    let member_name = entity.get_name().unwrap_or_else(|| "unknown_member".to_string());
    
    // Create struct access node
    let access_idx = graph.add_node(Node {
        name: format!("StructAccess: {}", member_name),
        kind: NodeType::StructAccess,
        line: get_line_number(&entity),
        usr: None,
        type_info: None,
    });
    
    // Connect parent to struct access
    graph.add_edge(
        parent_idx,
        access_idx,
        Edge { kind: EdgeType::Contains },
    );
    
    // Find the struct being accessed
    for child in entity.get_children() {
        if child.get_kind() == EntityKind::DeclRefExpr {
            if let Some(struct_name) = child.get_name() {
                if let Some(&struct_idx) = node_map.get(&struct_name) {
                    // Add edge showing the access uses the struct
                    graph.add_edge(
                        access_idx,
                        struct_idx,
                        Edge { kind: EdgeType::Accesses },
                    );
                }
            }
        } else {
            // Recurse for complex member access
            process_statement(
                child, 
                access_idx, 
                graph, 
                node_map, 
                &mut HashMap::new(),
                pointer_targets,
                &mut HashSet::new(),
                "",
                debug,
                false
            );
        }
    }
}

fn process_array_access(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
) {
    if debug {
        println!("Processing array access");
    }
    
    // Create array access node
    let access_idx = graph.add_node(Node {
        name: format!("ArrayAccess"),
        kind: NodeType::ArrayAccess,
        line: get_line_number(&entity),
        usr: None,
        type_info: None,
    });
    
    // Connect parent to array access
    graph.add_edge(
        parent_idx,
        access_idx,
        Edge { kind: EdgeType::Contains },
    );
    
    // Array access has two children: the array and the index
    let children = entity.get_children();
    
    // Find the array being accessed
    if children.len() >= 1 {
        let array_expr = &children[0];
        
        if array_expr.get_kind() == EntityKind::DeclRefExpr {
            if let Some(array_name) = array_expr.get_name() {
                if let Some(&array_idx) = node_map.get(&array_name) {
                    // Add edge showing the access uses the array
                    graph.add_edge(
                        access_idx,
                        array_idx,
                        Edge { kind: EdgeType::Accesses },
                    );
                }
            }
        } else {
            // Recurse for complex array expressions
            process_statement(
                array_expr.clone(), 
                access_idx, 
                graph, 
                node_map, 
                &mut HashMap::new(),
                pointer_targets,
                &mut HashSet::new(),
                "",
                debug,
                false
            );
        }
    }
    
    // Find the index expression
    if children.len() >= 2 {
        let index_expr = &children[1];
        
        // Look for variables in the index expression
        find_variable_refs(*index_expr, access_idx, graph, node_map, EdgeType::Uses);
    }
}

fn find_variable_refs(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    edge_type: EdgeType,
) {
    if entity.get_kind() == EntityKind::DeclRefExpr {
        if let Some(var_name) = entity.get_name() {
            if let Some(&var_idx) = node_map.get(&var_name) {
                // Add edge showing the usage
                graph.add_edge(
                    parent_idx,
                    var_idx,
                    Edge { kind: edge_type.clone() },
                );
            }
        }
    }
    
    // Recurse into children
    for child in entity.get_children() {
        find_variable_refs(child, parent_idx, graph, node_map, edge_type.clone());
    }
}

fn process_call_expression(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    debug: bool,
    memory_tracking: bool,
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
        let is_memory_op = memory_tracking && 
                          (function_name == "malloc" || 
                           function_name == "calloc" || 
                           function_name == "realloc" || 
                           function_name == "free");
        
        // Create node for the function call
        let node_type = if is_unsafe { 
            NodeType::UnsafeCall 
        } else if is_memory_op {
            NodeType::MemoryOp
        } else { 
            NodeType::Call 
        };
        
        let call_label = if is_unsafe {
            format!("Unsafe: {}", function_name)
        } else if is_memory_op {
            format!("MemoryOp: {}", function_name)
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
            type_info: None,
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
                type_info: None,
            });
            
            graph.add_edge(
                unsafe_idx,
                call_idx,
                Edge { kind: EdgeType::Controls },
            );
        }
        
        // Handle memory operations specially
        if is_memory_op {
            if function_name == "free" {
                // For free(), find the pointer being freed
                if let Some(arg) = entity.get_arguments().unwrap_or_default().first() {
                    if arg.get_kind() == EntityKind::DeclRefExpr {
                        if let Some(ptr_name) = arg.get_name() {
                            if let Some(&ptr_idx) = node_map.get(&ptr_name) {
                                // Add edge showing the memory operation frees the pointer
                                graph.add_edge(
                                    call_idx,
                                    ptr_idx,
                                    Edge { kind: EdgeType::Frees },
                                );
                            }
                        }
                    }
                }
            } else {
                // For allocation functions, nothing special to do here
                // The connection will be made by the assignment processing
            }
        }
        
        // Process call arguments to track data flow
        for arg in entity.get_arguments().unwrap_or_default() {
            process_call_argument(&arg, call_idx, graph, node_map, pointer_targets);
        }
        
        // Also check for function pointers in arguments
        process_function_pointer_references(entity, call_idx, graph, node_map, debug);
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
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
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
                        
                        // If the variable is a pointer, we might want to add a relationship
                        // to what it points to as well
                        if let Some(&target_idx) = pointer_targets.get(&var_idx) {
                            graph.add_edge(
                                call_idx,
                                target_idx,
                                Edge { kind: EdgeType::Uses },
                            );
                        }
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

fn process_function_pointer_references(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    debug: bool,
) {
    // This function specifically looks for function pointers in arguments
    match entity.get_kind() {
        EntityKind::CallExpr => {
            // Check arguments for function pointer references
            for arg in entity.get_arguments().unwrap_or_default() {
                // Function pointers often appear as DeclRefExpr in argument position
                if arg.get_kind() == EntityKind::DeclRefExpr || arg.get_kind() == EntityKind::UnexposedExpr {
                    // Try to extract a function name
                    if let Some(func_name) = arg.get_name() {
                        if debug {
                            println!("  Found potential function pointer: {} in argument", func_name);
                        }
                        
                        // Check if this is a known function name
                        if let Some(&func_idx) = node_map.get(&func_name) {
                            if debug {
                                println!("  Connecting function pointer {} to parent", func_name);
                            }
                            
                            // Add an edge showing the function is referenced/used by this entity
                            graph.add_edge(
                                parent_idx,
                                func_idx,
                                Edge { kind: EdgeType::References },
                            );
                        }
                    }
                    
                    // Recursively check inside the argument
                    for child in arg.get_children() {
                        if child.get_kind() == EntityKind::DeclRefExpr {
                            if let Some(name) = child.get_name() {
                                if let Some(&idx) = node_map.get(&name) {
                                    if debug {
                                        println!("  Found nested function pointer: {}", name);
                                    }
                                    graph.add_edge(
                                        parent_idx,
                                        idx,
                                        Edge { kind: EdgeType::References },
                                    );
                                }
                            }
                        }
                    }
                }
            }
        },
        _ => {
            // Recursively process children for other entity types
            for child in entity.get_children() {
                process_function_pointer_references(child, parent_idx, graph, node_map, debug);
            }
        }
    }
}

fn process_if_statement(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    usr_map: &mut HashMap<String, NodeIndex>,
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    debug: bool,
    memory_tracking: bool,
) -> Option<NodeIndex> {
    let if_idx = graph.add_node(Node {
        name: "If statement".to_string(),
        kind: NodeType::IfStatement,
        line: get_line_number(&entity),
        usr: None,
        type_info: None,
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
            type_info: None,
        });
        
        graph.add_edge(
            if_idx,
            then_bb_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        for child in then_branch.get_children() {
            process_statement(
                child, 
                then_bb_idx, 
                graph, 
                node_map, 
                usr_map, 
                pointer_targets,
                processed, 
                content, 
                debug,
                memory_tracking
            );
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
                type_info: None,
            });
            
            graph.add_edge(
                if_idx,
                else_bb_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            for child in else_branch.get_children() {
                process_statement(
                    child, 
                    else_bb_idx, 
                    graph, 
                    node_map, 
                    usr_map, 
                    pointer_targets,
                    processed, 
                    content, 
                    debug,
                    memory_tracking
                );
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
    pointer_targets: &mut HashMap<NodeIndex, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    loop_type: NodeType,
    debug: bool,
    memory_tracking: bool,
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
        type_info: None,
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
            type_info: None,
        });
        
        graph.add_edge(
            loop_idx,
            body_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        for child in body.get_children() {
            process_statement(
                child, 
                body_idx, 
                graph, 
                node_map, 
                usr_map, 
                pointer_targets,
                processed, 
                content, 
                debug,
                memory_tracking
            );
        }
    }
    
    Some(loop_idx)
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
                    type_info: None,
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
    
    // Add pthread function references
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
                                    if graph[bb_edge.id()].kind == EdgeType::Contains {
                                        let call_node = bb_edge.target();
                                        if graph[call_node].name == "Call: pthread_create" {
                                            // Check if this call references the handler
                                            graph.edges(call_node)
                                                .filter_map(|call_edge| {
                                                    if graph[call_edge.id()].kind == EdgeType::References &&
                                                       call_edge.target() == handler_idx {
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
                        type_info: None,
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
    
    // Add the new edges from our AST processing
    for (from, to) in new_edges {
        graph.add_edge(
            from,
            to,
            Edge { kind: EdgeType::Calls },
        );
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
            NodeType::Pointer => ("ellipse", "darkblue", "filled"),
            NodeType::Array => ("ellipse", "lightyellow", "filled"),
            NodeType::IfStatement => ("diamond", "indigo", "filled"),
            NodeType::ForLoop => ("box", "lightblue", "filled,rounded"),
            NodeType::WhileLoop => ("box", "lightblue", "filled,rounded"),
            NodeType::Assignment => ("ellipse", "grey", "filled"),
            NodeType::MemoryOp => ("ellipse", "violet", "filled"),
            NodeType::Dereference => ("ellipse", "darkred", "filled"),
            NodeType::AddressOf => ("ellipse", "lightgreen", "filled"),
            NodeType::Cast => ("ellipse", "cyan", "filled"),
            NodeType::StructAccess => ("ellipse", "pink", "filled"),
            NodeType::ArrayAccess => ("ellipse", "yellow", "filled"),
        };
        
        // Add type information if available
        let label = if let Some(ref type_info) = node.type_info {
            format!("{} [{}]", node.name, type_info)
        } else {
            node.name.clone()
        };
        
        output.push_str(&format!("    {} [label=\"{}\", shape={}, fillcolor=\"{}\", style=\"{}\"];\n", 
                                node_id, label, shape, color, style));
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
            EdgeType::References => "references",
            EdgeType::Assigns => "assigns",
            EdgeType::Points => "points_to",
            EdgeType::Casts => "casts",
            EdgeType::Accesses => "accesses",
            EdgeType::Allocates => "allocates",
            EdgeType::Frees => "frees",
            EdgeType::Controls => "controls",
        };
        
        // Edge color based on type
        let color = match edge.kind {
            EdgeType::Calls => "blue",
            EdgeType::Contains => "gray",
            EdgeType::Uses => "green",
            EdgeType::Defines => "purple",
            EdgeType::References => "darkblue",
            EdgeType::Assigns => "black",
            EdgeType::Points => "darkorange",
            EdgeType::Casts => "cyan",
            EdgeType::Accesses => "pink",
            EdgeType::Allocates => "darkgreen",
            EdgeType::Frees => "red",
            EdgeType::Controls => "red",
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
            NodeType::BufferParameter => "buffer_param",
            NodeType::Pointer => "pointer",
            NodeType::Array => "array",
            NodeType::Call => "call",
            NodeType::UnsafeCall => "unsafe_call",
            NodeType::BasicBlock => "basic",
            NodeType::IfStatement => "if_statement",
            NodeType::ForLoop => "for_loop",
            NodeType::WhileLoop => "while_loop",
            NodeType::Assignment => "assignment",
            NodeType::MemoryOp => "memory_op",
            NodeType::Dereference => "dereference",
            NodeType::AddressOf => "address_of",
            NodeType::Cast => "cast",
            NodeType::StructAccess => "struct_access",
            NodeType::ArrayAccess => "array_access",
        };
        
        // Add type information if available
        let label = if let Some(ref type_info) = node.type_info {
            format!("{} [{}]", node.name, type_info)
        } else {
            node.name.clone()
        };
        
        nodes.push(json!({
            "id": node_id,
            "label": label,
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
            EdgeType::References => ("references", "darkblue", 2.0),
            EdgeType::Assigns => ("assigns", "black", 1.5),
            EdgeType::Points => ("points_to", "darkorange", 2.0),
            EdgeType::Casts => ("casts", "cyan", 1.5),
            EdgeType::Accesses => ("accesses", "pink", 1.5),
            EdgeType::Allocates => ("allocates", "darkgreen", 2.0),
            EdgeType::Frees => ("frees", "red", 2.0),
            EdgeType::Controls => ("controls", "red", 3.0),
            EdgeType::Defines => ("defines", "purple", 2.0),
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
        NodeType::BufferParameter => "buffer",
        NodeType::Pointer => "ptr",
        NodeType::Array => "array",
        NodeType::Call => "call",
        NodeType::UnsafeCall => "unsafe",
        NodeType::BasicBlock => "block",
        NodeType::IfStatement => "if",
        NodeType::ForLoop => "for",
        NodeType::WhileLoop => "while",
        NodeType::Assignment => "assign",
        NodeType::MemoryOp => "memop",
        NodeType::Dereference => "deref",
        NodeType::AddressOf => "addrof",
        NodeType::Cast => "cast",
        NodeType::StructAccess => "struct",
        NodeType::ArrayAccess => "arr_acc",
    }
}