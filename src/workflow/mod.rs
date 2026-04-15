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

pub mod audit;
pub mod exec;
pub mod hooks;
pub mod intent;
pub mod learning;
pub mod parser;
pub mod preset;
pub mod registry;
pub mod scaffold;
pub mod skill_registry;

// Re-export key types
pub use exec::{execute, CostTracker, ExecContext, WorkflowRunResult};
pub use hooks::{ConsentLevel, HookContext, SecurityHooks};
pub use intent::{classify_heuristic, classify_intent, IntentConfig, WorkflowIntent};
pub use learning::{analyze_workflow, run_learning_loop, LearningConfig, SuggestionType, WorkflowSuggestion};
pub use parser::{parse_spec, InputDef, Limits, Step, WorkflowSpec};
pub use preset::{import_presets, parse_preset, LAWYER_PRESETS};
pub use registry::ToolRegistry;
pub use scaffold::{scaffold, ScaffoldRequest, ScaffoldResponse};
pub use skill_registry::{bundled_skills, expand_skill_step, is_skill_call, resolve_skill};
