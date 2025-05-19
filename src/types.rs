// Node types represent the different kinds of entities in our graph
#[derive(Debug, Clone, PartialEq)]
pub enum NodeType {
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
pub enum EdgeType {
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
    Defines,    // Defines a function
}

// Encapsulate node information
#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub kind: NodeType,
    pub line: Option<usize>,
    pub usr: Option<String>,
    pub type_info: Option<String>,
}

#[derive(Debug)]
pub struct Edge {
    pub kind: EdgeType,
} 