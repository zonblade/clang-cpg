use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clang::{Entity, EntityKind, Index, TranslationUnit};
use petgraph::dot::{Config, Dot};
use petgraph::graph::{DiGraph, NodeIndex};
use structopt::StructOpt;
use serde_json::{json, Value};

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
}

#[derive(Debug, Clone, PartialEq)]
enum EdgeType {
    Contains,
    Calls,
    Controls,
    Uses,
    Defines,
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
}

fn main() -> Result<()> {
    let opt = Opt::from_args();

    // Read the content of the C file
    let content = fs::read_to_string(&opt.input)
        .with_context(|| format!("Failed to read file: {:?}", opt.input))?;

    // Initialize Clang
    let clang = clang::Clang::new().unwrap();
    let index = clang::Index::new(&clang, true, true);
    let tu = index.parser(opt.input.to_str().unwrap())
        .parse()
        .with_context(|| "Failed to parse C file with Clang")?;

    // Build our graph
    let mut graph = DiGraph::<Node, Edge>::new();
    let mut node_map: HashMap<String, NodeIndex> = HashMap::new();
    let mut processed_entities = HashSet::new();
    
    // Get the root cursor from the translation unit
    let entity = tu.get_entity();
    
    // Process the AST
    analyze_program(entity, &mut graph, &mut node_map, &mut processed_entities, &content);
    
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

fn analyze_program(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
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
    
    match entity.get_kind() {
        EntityKind::FunctionDecl => {
            process_function(entity, graph, node_map, processed, content);
        },
        EntityKind::VarDecl => {
            process_variable(entity, graph, node_map);
        },
        EntityKind::IfStmt => {
            process_if_statement(entity, graph, node_map, processed, content);
        },
        EntityKind::ForStmt => {
            process_loop(entity, graph, node_map, processed, content, NodeType::ForLoop);
        },
        EntityKind::WhileStmt => {
            process_loop(entity, graph, node_map, processed, content, NodeType::WhileLoop);
        },
        _ => {
            // Recursively process children
            for child in entity.get_children() {
                analyze_program(child, graph, node_map, processed, content);
            }
        }
    }
}

fn process_function(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
) {
    if let Some(name) = entity.get_name() {
        let is_main = name == "main";
        let line = get_line_number(&entity);
        
        // Create node for the function
        let node_type = if is_main { NodeType::Main } else { NodeType::Function };
        let node_idx = graph.add_node(Node {
            name: name.clone(),
            kind: node_type,
            line,
        });
        
        node_map.insert(name.clone(), node_idx);
        
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
            });
            
            // Connect function to basic block
            graph.add_edge(
                node_idx,
                bb_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            // Process body contents
            for child in body.get_children() {
                process_statement(child, bb_idx, graph, node_map, processed, content);
            }
        }
    }
}

fn process_statement(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
) {
    match entity.get_kind() {
        EntityKind::CallExpr => {
            process_call_expression(entity, parent_idx, graph, node_map);
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
            let if_idx = process_if_statement(entity, graph, node_map, processed, content);
            
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
            let loop_idx = process_loop(entity, graph, node_map, processed, content, NodeType::ForLoop);
            
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
            let loop_idx = process_loop(entity, graph, node_map, processed, content, NodeType::WhileLoop);
            
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
                process_statement(child, parent_idx, graph, node_map, processed, content);
            }
        },
        _ => {
            // Process other statement types or recurse into children
            for child in entity.get_children() {
                process_statement(child, parent_idx, graph, node_map, processed, content);
            }
        }
    }
}

fn process_call_expression(
    entity: Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
) {
    if let Some(called_entity) = entity.get_reference() {
        if let Some(function_name) = called_entity.get_name() {
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
            
            let call_idx = graph.add_node(Node {
                name: call_label,
                kind: node_type,
                line: get_line_number(&entity),
            });
            
            // Connect parent to call
            graph.add_edge(
                parent_idx,
                call_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            // Connect call to the actual function if it exists in our graph
            if let Some(&func_idx) = node_map.get(&function_name) {
                graph.add_edge(
                    call_idx,
                    func_idx,
                    Edge { kind: EdgeType::Calls },
                );
            }
            
            // For unsafe calls, create another node that controls this one
            if is_unsafe {
                let unsafe_idx = graph.add_node(Node {
                    name: format!("Unsafe: {}", function_name),
                    kind: NodeType::UnsafeCall,
                    line: None,
                });
                
                graph.add_edge(
                    unsafe_idx,
                    call_idx,
                    Edge { kind: EdgeType::Controls },
                );
            }
            
            // Process call arguments to track data flow
            for (i, arg) in entity.get_arguments().unwrap_or_default().iter().enumerate() {
                process_call_argument(arg, call_idx, graph, node_map);
            }
        }
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
        });
        
        node_map.insert(name, var_idx);
    }
}

fn process_if_statement(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
) -> Option<NodeIndex> {
    let if_idx = graph.add_node(Node {
        name: "If statement".to_string(),
        kind: NodeType::IfStatement,
        line: get_line_number(&entity),
    });
    
    // Process the condition (to track variable uses)
    if let Some(cond) = entity.get_children().iter().find(|c| c.get_kind() == EntityKind::BinaryOperator) {
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
        });
        
        graph.add_edge(
            if_idx,
            then_bb_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        for child in then_branch.get_children() {
            process_statement(child, then_bb_idx, graph, node_map, processed, content);
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
            });
            
            graph.add_edge(
                if_idx,
                else_bb_idx,
                Edge { kind: EdgeType::Contains },
            );
            
            for child in else_branch.get_children() {
                process_statement(child, else_bb_idx, graph, node_map, processed, content);
            }
        }
    }
    
    Some(if_idx)
}

fn process_loop(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    processed: &mut HashSet<String>,
    content: &str,
    loop_type: NodeType,
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
    });
    
    // Process loop condition variables
    for child in entity.get_children() {
        if child.get_kind() == EntityKind::BinaryOperator || 
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
        });
        
        graph.add_edge(
            loop_idx,
            body_idx,
            Edge { kind: EdgeType::Contains },
        );
        
        for child in body.get_children() {
            process_statement(child, body_idx, graph, node_map, processed, content);
        }
    }
    
    Some(loop_idx)
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
            NodeType::Call => ("ellipse", "orange", "filled"),
            NodeType::Main => ("ellipse", "green", "filled"),
            NodeType::Function => ("ellipse", "lightblue", "filled"),
            NodeType::BasicBlock => ("box", "yellow", "filled,rounded"),
            NodeType::Parameter => ("ellipse", "purple", "filled"),
            NodeType::BufferParameter => ("ellipse", "blue", "filled"),
            NodeType::Variable => ("ellipse", "orange", "filled"),
            NodeType::IfStatement => ("diamond", "lightgreen", "filled"),
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
        };
        
        // Edge color based on type
        let color = match edge.kind {
            EdgeType::Calls => "blue",
            EdgeType::Contains => "gray",
            EdgeType::Uses => "green",
            EdgeType::Defines => "purple",
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