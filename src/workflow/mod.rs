// Workflow Engine (v3.0 Section D)
//
// Declarative YAML-based workflow system. Users define workflows as
// sequences of typed steps (LLM calls, memory operations, tool calls, etc.)
// with input parameters, cost limits, and trigger conditions.
//
// Architecture:
//   parser.rs  — YAML text → WorkflowSpec (IR)
//   exec.rs    — WorkflowSpec + inputs → execution with cost tracking
//   registry.rs — tool permission whitelist

pub mod exec;
pub mod parser;
pub mod registry;

// Re-export key types
pub use exec::{execute, CostTracker, ExecContext, WorkflowRunResult};
pub use parser::{parse_spec, InputDef, Limits, Step, WorkflowSpec};
pub use registry::ToolRegistry;
