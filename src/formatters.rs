use crate::types::{Edge, EdgeType, Node, NodeType};
use petgraph::graph::{DiGraph, NodeIndex};
use serde_json::json;
use std::collections::HashMap;

pub fn format_graph_as_dot(graph: &DiGraph<Node, Edge>) -> String {
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

        output.push_str(&format!(
            "    {} [label=\"{}\", shape={}, fillcolor=\"{}\", style=\"{}\"];\n",
            node_id, label, shape, color, style
        ));
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

        output.push_str(&format!(
            "    {} -> {} [label=\"{}\", color=\"{}\"];\n",
            source_id, target_id, label, color
        ));
    }

    output.push_str("}\n");
    output
}

pub fn format_graph_as_json(graph: &DiGraph<Node, Edge>) -> String {
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
