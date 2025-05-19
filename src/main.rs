use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use cparser::formatters::{format_graph_as_dot, format_graph_as_json};
use cparser::graph_builder::{analyze_program, find_all_functions, fix_disconnected_calls};
use cparser::types::{Edge, Node};
use cparser::utils::{extract_function_calls_from_source, extract_pthread_assignments};
use petgraph::graph::{DiGraph, NodeIndex};
use structopt::StructOpt;


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