use std::collections::{HashMap, HashSet};
use petgraph::graph::{DiGraph, NodeIndex};
use clang::{Entity, EntityKind};
use crate::processors_ext::{process_array_access, process_assignment_value, process_call_expression, process_function_pointer_references, process_if_statement, process_loop, process_member_access, process_unary_operator};
use crate::types::{Node, Edge, NodeType, EdgeType};
use crate::utils::*;

pub fn process_function(
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

pub fn process_statement(
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

pub fn process_variable_decl(
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

pub fn process_initializer(
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

pub fn process_binary_operator(
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