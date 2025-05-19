use std::collections::{HashMap, HashSet};
use petgraph::graph::{DiGraph, NodeIndex};
use clang::{Entity, EntityKind};
use crate::processors::process_statement;
use crate::types::{Node, Edge, NodeType, EdgeType};
use crate::utils::*;

pub fn process_assignment_value(
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

pub fn process_unary_operator(
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
                    child.clone(), 
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
                    child.clone(), 
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
                child.clone(), 
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

pub fn process_member_access(
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
                child.clone(), 
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

pub fn process_array_access(
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

pub fn find_variable_refs(
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

pub fn process_call_expression(
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

pub fn extract_function_name_from_call(entity: &Entity) -> Option<String> {
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

pub fn process_call_argument(
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

pub fn process_function_pointer_references(
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

pub fn process_if_statement(
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
                child.clone(), 
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
                    child.clone(), 
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

pub fn process_loop(
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
                child.clone(), 
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