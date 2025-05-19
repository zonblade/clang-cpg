use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Context, Result};
use clang::{Entity, EntityKind, Index, TranslationUnit};
use petgraph::dot::{Config, Dot};
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef as VisitEdgeRef;
use regex::Regex;
use structopt::StructOpt;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
struct Node {
    name: String,
    kind: NodeType,
    source_location: Option<String>,
}

#[derive(Debug, PartialEq, Clone)]
enum NodeType {
    Function,
    Variable,
    Parameter,
    Call,
    UnsafeCall,
    BufferParam,
    BasicBlock,
    Main,
}

#[derive(Debug)]
struct Edge {
    kind: EdgeType,
}

#[derive(Debug, PartialEq, Clone)]
enum EdgeType {
    Calls,
    Contains,
    Uses,
    Defines,
    Controls,
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
    #[structopt(short, long, default_value = "json")]
    format: String,
    
    /// Output simplified graph (only show functions)
    #[structopt(short, long)]
    simplified: bool,
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
    
    // Get the root cursor from the translation unit
    let entity = tu.get_entity();
    
    // Process the AST
    process_entity(entity, &mut graph, &mut node_map, &content);
    
    // Check for unsafe functions
    mark_unsafe_functions(&mut graph, &node_map);
    
    // Generate the output based on selected format
    let output = if opt.format == "json" {
        format_graph_as_json(&graph)
    } else {
        format_graph_as_dot(&graph, opt.simplified)
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

fn process_entity(
    entity: Entity,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    content: &str,
) {
    match entity.get_kind() {
        EntityKind::FunctionDecl => {
            // Process function
            let name = entity.get_name().unwrap_or_else(|| "unknown_function".to_string());
            let is_main = name == "main";
            
            let location = get_entity_location(&entity);
            
            let node_type = if is_main { NodeType::Main } else { NodeType::Function };
            
            let node_idx = graph.add_node(Node {
                name: name.clone(),
                kind: node_type,
                source_location: location.clone(),
            });
            
            node_map.insert(name.clone(), node_idx);
            
            // Process function parameters
            for param in entity.get_arguments().unwrap_or_default() {
                let param_name = param.get_name().unwrap_or_else(|| "unnamed_param".to_string());
                let param_type = param.get_type().unwrap();
                let type_str = param_type.get_display_name();
                
                // Check if this is a buffer parameter (char* or similar)
                let is_buffer = type_str.contains("char *") || type_str.contains("char*");
                
                let param_node_idx = graph.add_node(Node {
                    name: param_name.clone(),
                    kind: if is_buffer { NodeType::BufferParam } else { NodeType::Parameter },
                    source_location: get_entity_location(&param),
                });
                
                node_map.insert(format!("{}_{}", name, param_name), param_node_idx);
                
                // Add edge from function to parameter
                graph.add_edge(
                    node_idx,
                    param_node_idx,
                    Edge { kind: EdgeType::Contains },
                );
            }
            
            // Process function body (for getting call graph)
            if let Some(body) = entity.get_children().iter().find(|c| c.get_kind() == EntityKind::CompoundStmt) {
                process_function_body(body, node_idx, graph, node_map, content);
            }
        }
        EntityKind::VarDecl => {
            // Process variable declaration
            if let Some(name) = entity.get_name() {
                let node_idx = graph.add_node(Node {
                    name: name.clone(),
                    kind: NodeType::Variable,
                    source_location: get_entity_location(&entity),
                });
                
                node_map.insert(name, node_idx);
            }
        }
        _ => {
            // Recursively process children
            for child in entity.get_children() {
                process_entity(child, graph, node_map, content);
            }
        }
    }
}

fn process_function_body(
    body: &Entity,
    function_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    content: &str,
) {
    // Create a basic block for the function body
    let bb_idx = graph.add_node(Node {
        name: "entry".to_string(),
        kind: NodeType::BasicBlock,
        source_location: get_entity_location(body),
    });
    
    graph.add_edge(
        function_idx,
        bb_idx,
        Edge { kind: EdgeType::Contains },
    );
    
    // Find all function calls within the body
    find_function_calls(body, bb_idx, graph, node_map, content);
}

fn find_function_calls(
    entity: &Entity,
    parent_idx: NodeIndex,
    graph: &mut DiGraph<Node, Edge>,
    node_map: &mut HashMap<String, NodeIndex>,
    content: &str,
) {
    if entity.get_kind() == EntityKind::CallExpr {
        // This is a function call
        if let Some(called_entity) = entity.get_reference() {
            if let Some(called_name) = called_entity.get_name() {
                // Create a node for this function call
                let call_idx = graph.add_node(Node {
                    name: called_name.clone(),
                    kind: NodeType::Call,
                    source_location: get_entity_location(entity),
                });
                
                // Add edge from parent to call
                graph.add_edge(
                    parent_idx,
                    call_idx,
                    Edge { kind: EdgeType::Contains },
                );
                
                // If the called function is in our node map, add an edge
                if let Some(&called_idx) = node_map.get(&called_name) {
                    graph.add_edge(
                        call_idx,
                        called_idx,
                        Edge { kind: EdgeType::Calls },
                    );
                }
            }
        }
    }
    
    // Recursively process children
    for child in entity.get_children() {
        find_function_calls(&child, parent_idx, graph, node_map, content);
    }
}

fn mark_unsafe_functions(
    graph: &mut DiGraph<Node, Edge>,
    node_map: &HashMap<String, NodeIndex>,
) {
    // List of known unsafe functions
    let unsafe_functions = [
        "strcpy", "strcat", "sprintf", "gets", "scanf", 
        "vsprintf", "memcpy", "memmove", "strncpy", "strncat",
    ];
    
    // Clone the nodes to avoid borrow checker issues
    let nodes: Vec<_> = graph.node_indices().collect();
    
    for node_idx in nodes {
        // Clone the relevant data instead of keeping a reference
        let is_call = graph[node_idx].kind == NodeType::Call;
        let node_name = graph[node_idx].name.clone();
        
        // Check if this is a call to an unsafe function
        if is_call {
            for unsafe_func in &unsafe_functions {
                if node_name == *unsafe_func {
                    // Mark this call as unsafe
                    graph[node_idx].kind = NodeType::UnsafeCall;
                }
            }
        }
    }
}

fn get_entity_location(entity: &Entity) -> Option<String> {
    if let Some(location) = entity.get_location() {
        let file = location.get_file_location();
        let line = file.line;
        let column = file.column;
        Some(format!("{}:{}", line, column))
    } else {
        None
    }
}

// Format the graph as JSON that matches the sample format
fn format_graph_as_json(graph: &DiGraph<Node, Edge>) -> String {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    let mut node_id_map: HashMap<NodeIndex, String> = HashMap::new();
    
    // Process nodes
    for node_idx in graph.node_indices() {
        let node = &graph[node_idx];
        let node_id = format!("{}_{}", node_type_to_prefix(&node.kind), node_idx.index());
        node_id_map.insert(node_idx, node_id.clone());
        
        // Create node label based on type
        let label = match node.kind {
            NodeType::Call => format!("Call: {}", node.name),
            NodeType::UnsafeCall => format!("⚠️ Unsafe: {}", node.name),
            NodeType::Parameter => format!("Param: {} (int)", node.name),
            NodeType::BufferParam => format!("BufferParam: {} (char *) [buffer parameter]", node.name),
            NodeType::BasicBlock => format!("BasicBlock: {}", node.name),
            NodeType::Variable => format!("Var: {}", node.name),
            NodeType::Main => node.name.clone(),
            NodeType::Function => node.name.clone(),
        };
        
        // Map node type to group
        let group = match node.kind {
            NodeType::Function => "function",
            NodeType::Main => "main_function",
            NodeType::Variable => "variable",
            NodeType::Parameter => "param",
            NodeType::Call => "call",
            NodeType::UnsafeCall => "unsafe_call",
            NodeType::BufferParam => "buffer_param",
            NodeType::BasicBlock => "basic",
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
        NodeType::Function => "block",
        NodeType::Main => "block",
        NodeType::Variable => "var",
        NodeType::Parameter => "var_param",
        NodeType::Call => "call",
        NodeType::UnsafeCall => "call",
        NodeType::BufferParam => "var_param",
        NodeType::BasicBlock => "block",
    }
}

// Output the graph in DOT format with custom formatting (original function preserved)
fn format_graph_as_dot(graph: &DiGraph<Node, Edge>, simplified: bool) -> String {
    let mut output = String::from("digraph {\n");
    
    // Add global styling
    output.push_str("    // Graph styling\n");
    output.push_str("    graph [fontname=\"Arial\", rankdir=TB, splines=true];\n");
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
            NodeType::BufferParam => ("ellipse", "blue", "filled"),
            NodeType::Variable => ("ellipse", "purple", "filled"),
        };
        
        // Format label based on node type
        let label = match node.kind {
            NodeType::Call => format!("Call: {}", node.name),
            NodeType::UnsafeCall => format!("Unsafe: {}", node.name),
            NodeType::Parameter => format!("Param: {} (int)", node.name),
            NodeType::BufferParam => format!("BufferParam: {} (char *) [buffer parameter]", node.name),
            NodeType::BasicBlock => format!("BasicBlock: {}", node.name),
            NodeType::Variable => format!("Var: {}", node.name),
            _ => node.name.clone(),
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