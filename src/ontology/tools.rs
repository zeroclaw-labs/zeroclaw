//! Ontology-aware tools exposed to the LLM agent.
//!
//! These tools wrap the ontology layer and present a clean, high-level
//! interface to the LLM. Instead of exposing 70+ ZeroClaw tools directly,
//! the LLM interacts with a small set of ontology actions that route
//! internally to the appropriate tool.
//!
//! # Tools
//!
//! - `ontology_get_context` — snapshot of user's current world state
//! - `ontology_search_objects` — search objects by type and query
//! - `ontology_execute_action` — execute a named action (routes to tools)

use super::context::ContextBuilder;
use super::dispatcher::ActionDispatcher;
use super::repo::OntologyRepo;
use super::types::{ActorKind, ContextSnapshotRequest, ExecuteActionRequest};
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Tool: ontology_get_context
// ---------------------------------------------------------------------------

/// Provides the LLM with a structured snapshot of the user's ontology state.
pub struct OntologyGetContextTool {
    context_builder: Arc<ContextBuilder>,
    default_owner: String,
}

impl OntologyGetContextTool {
    pub fn new(context_builder: Arc<ContextBuilder>, default_owner: String) -> Self {
        Self {
            context_builder,
            default_owner,
        }
    }
}

#[async_trait]
impl Tool for OntologyGetContextTool {
    fn name(&self) -> &str {
        "ontology_get_context"
    }

    fn description(&self) -> &str {
        "Get a structured snapshot of the user's current world state: recent contacts, tasks, \
         projects, and actions. Use this to understand the user's context before taking action."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "channel": {
                    "type": "string",
                    "description": "Filter by channel (e.g. 'kakao', 'desktop', 'mobile')"
                },
                "limit_recent_actions": {
                    "type": "integer",
                    "description": "Number of recent actions to include (default: 20, max: 50)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let req = ContextSnapshotRequest {
            owner_user_id: self.default_owner.clone(),
            channel: args
                .get("channel")
                .and_then(|v| v.as_str())
                .map(String::from),
            device_id: args
                .get("device_id")
                .and_then(|v| v.as_str())
                .map(String::from),
            limit_recent_actions: args
                .get("limit_recent_actions")
                .and_then(|v| v.as_u64())
                .unwrap_or(20)
                .min(50) as usize,
        };

        match self.context_builder.build(&req) {
            Ok(snapshot) => {
                let output =
                    serde_json::to_string_pretty(&snapshot).unwrap_or_else(|_| "{}".to_string());
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool: ontology_search_objects
// ---------------------------------------------------------------------------

/// Search ontology objects by type and query.
pub struct OntologySearchObjectsTool {
    repo: Arc<OntologyRepo>,
    default_owner: String,
}

impl OntologySearchObjectsTool {
    pub fn new(repo: Arc<OntologyRepo>, default_owner: String) -> Self {
        Self {
            repo,
            default_owner,
        }
    }
}

#[async_trait]
impl Tool for OntologySearchObjectsTool {
    fn name(&self) -> &str {
        "ontology_search_objects"
    }

    fn description(&self) -> &str {
        "Search the user's ontology objects (contacts, tasks, projects, documents, etc.) \
         by type and/or text query. Returns matching objects with their properties."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "type": {
                    "type": "string",
                    "description": "Object type to filter by (e.g. 'Task', 'Contact', 'Document', 'Project')",
                    "enum": ["User", "Contact", "Device", "Channel", "Task", "Project", "Document", "Meeting", "Context", "Preference"]
                },
                "query": {
                    "type": "string",
                    "description": "Text query to search in titles and properties (FTS5)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Max results to return (default: 10)"
                }
            }
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let type_name = args.get("type").and_then(|v| v.as_str());
        let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

        match self
            .repo
            .search_objects(&self.default_owner, type_name, query, limit)
        {
            Ok(objects) => {
                let summaries: Vec<serde_json::Value> = objects
                    .iter()
                    .map(|o| {
                        json!({
                            "id": o.id,
                            "type_id": o.type_id,
                            "title": o.title,
                            "properties": o.properties,
                            "updated_at": o.updated_at,
                        })
                    })
                    .collect();
                let output = serde_json::to_string_pretty(&json!({
                    "objects": summaries,
                    "count": summaries.len(),
                }))
                .unwrap_or_else(|_| "{}".to_string());
                Ok(ToolResult {
                    success: true,
                    output,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tool: ontology_execute_action
// ---------------------------------------------------------------------------

/// Execute a named ontology action that routes to ZeroClaw tools or
/// internal ontology operations.
pub struct OntologyExecuteActionTool {
    dispatcher: Arc<ActionDispatcher>,
    default_owner: String,
}

impl OntologyExecuteActionTool {
    pub fn new(dispatcher: Arc<ActionDispatcher>, default_owner: String) -> Self {
        Self {
            dispatcher,
            default_owner,
        }
    }
}

#[async_trait]
impl Tool for OntologyExecuteActionTool {
    fn name(&self) -> &str {
        "ontology_execute_action"
    }

    fn description(&self) -> &str {
        "Execute a named action in the user's ontology. Actions modify objects, \
         create links, trigger tools, and log results. Available action types: \
         SendMessage, CreateTask, UpdateTask, ListTasks, FetchResource, WebSearch, \
         ReadDocument, SummarizeDocument, CreateEvent, UpdateEvent, ListEvents, \
         StartSession, EndSession, SavePreference, RecordDecision, RunCommand, PlanTasks."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action_type_name": {
                    "type": "string",
                    "description": "The action type to execute",
                    "enum": [
                        "SendMessage", "CreateTask", "UpdateTask", "ListTasks",
                        "FetchResource", "WebSearch", "ReadDocument", "SummarizeDocument",
                        "CreateEvent", "UpdateEvent", "ListEvents",
                        "StartSession", "EndSession",
                        "SavePreference", "RecordDecision",
                        "RunCommand", "PlanTasks"
                    ]
                },
                "primary_object_id": {
                    "type": "integer",
                    "description": "ID of the primary target object (Contact, Task, Document, etc.)"
                },
                "related_object_ids": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "IDs of related objects"
                },
                "params": {
                    "type": "object",
                    "description": "Action-specific parameters (e.g. message text, task title, URL, etc.)"
                },
                "channel": {
                    "type": "string",
                    "description": "Channel context (e.g. 'kakao', 'desktop')"
                },
                "context_id": {
                    "type": "integer",
                    "description": "ID of the current Context object"
                },
                "occurred_at": {
                    "type": "string",
                    "description": "WHEN: Real-world time the action occurred (ISO-8601 e.g. '2026-03-18T14:30:00+09:00', or descriptive e.g. '오늘 오후 2시 30분'). Auto-filled with current time if omitted."
                },
                "location": {
                    "type": "string",
                    "description": "WHERE: Real-world location of the action (free-form, e.g. '서울 서초구 법원로 서울중앙지방법원', '레이크사이드CC 골프장', '사무실')"
                }
            },
            "required": ["action_type_name", "params"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action_type_name = args
            .get("action_type_name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("action_type_name is required"))?
            .to_string();

        let req = ExecuteActionRequest {
            action_type_name,
            owner_user_id: self.default_owner.clone(),
            actor_kind: Some(ActorKind::Agent),
            primary_object_id: args.get("primary_object_id").and_then(|v| v.as_i64()),
            related_object_ids: args
                .get("related_object_ids")
                .and_then(|v| v.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_i64()).collect())
                .unwrap_or_default(),
            params: args.get("params").cloned().unwrap_or(json!({})),
            channel: args
                .get("channel")
                .and_then(|v| v.as_str())
                .map(String::from),
            context_id: args.get("context_id").and_then(|v| v.as_i64()),
            occurred_at: args
                .get("occurred_at")
                .and_then(|v| v.as_str())
                .map(String::from),
            location: args
                .get("location")
                .and_then(|v| v.as_str())
                .map(String::from),
        };

        match self.dispatcher.execute(req).await {
            Ok(result) => {
                let output =
                    serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_string());
                Ok(ToolResult {
                    success: result
                        .get("success")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(true),
                    output,
                    error: result
                        .get("error")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            }),
        }
    }
}
