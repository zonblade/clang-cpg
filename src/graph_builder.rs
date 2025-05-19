use std::collections::{HashMap, HashSet};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use clang::{Entity, EntityKind};

use crate::types::{Node, Edge, NodeType, EdgeType};
use crate::utils::*;
use crate::processors::*;
use crate::processors_ext::*;

pub fn find_all_functions(
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

pub fn analyze_program(
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

// Fix any disconnected calls by checking call nodes that should be connected to functions
pub fn fix_disconnected_calls(
    graph: &mut DiGraph<Node, Edge>,
    node_map: &HashMap<String, NodeIndex>,
    _usr_map: &HashMap<String, NodeIndex>,
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