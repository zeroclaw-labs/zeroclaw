//! Action dispatcher — routes ontology actions to ZeroClaw tool execution.
//!
//! The dispatcher is the bridge between the high-level ontology action types
//! (e.g. `SendMessage`, `CreateTask`) and the low-level ZeroClaw tool
//! implementations. It:
//!
//! 1. Logs the action as "pending" in the ontology.
//! 2. Routes to the appropriate ZeroClaw tool (or internal ontology operation).
//! 3. Updates the action log with the result.
//! 4. Triggers post-action rules for automatic graph updates.

use super::repo::OntologyRepo;
use super::rules::RuleEngine;
use super::types::*;
use crate::tools::traits::{Tool, ToolResult};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Central action dispatcher that maps ontology action types to tool execution.
pub struct ActionDispatcher {
    repo: Arc<OntologyRepo>,
    rule_engine: Arc<RuleEngine>,
    /// Map of ZeroClaw tool name → tool instance (populated from `all_tools()`).
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ActionDispatcher {
    pub fn new(
        repo: Arc<OntologyRepo>,
        rule_engine: Arc<RuleEngine>,
        tool_list: Vec<Arc<dyn Tool>>,
    ) -> Self {
        let mut tools = HashMap::new();
        for tool in tool_list {
            tools.insert(tool.name().to_string(), tool);
        }
        Self {
            repo,
            rule_engine,
            tools,
        }
    }

    /// Execute an ontology action: log → route → update → rules.
    ///
    /// Returns the result value from the underlying tool, or an internal
    /// ontology operation result.
    pub async fn execute(&self, req: ExecuteActionRequest) -> anyhow::Result<serde_json::Value> {
        let actor_kind = req.actor_kind.clone().unwrap_or(ActorKind::Agent);

        // 1. Log the action as pending.
        let action_id = self.repo.insert_action_pending(
            &req.action_type_name,
            &req.owner_user_id,
            &actor_kind,
            req.primary_object_id,
            &req.related_object_ids,
            &req.params,
            req.channel.as_deref(),
            req.context_id,
        )?;

        // 2. Route to the appropriate handler.
        let result = self.route_action(&req).await;

        // 3. Update action log based on result.
        match &result {
            Ok(value) => {
                // Check if the tool explicitly reported failure via success field.
                let tool_failed = value
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .map_or(false, |s| !s);
                if tool_failed {
                    let err_msg = value
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("tool reported failure");
                    self.repo.fail_action(action_id, err_msg)?;
                } else {
                    self.repo.complete_action(action_id, value)?;
                }
            }
            Err(e) => {
                self.repo.fail_action(action_id, &e.to_string())?;
            }
        }

        // 4. Trigger post-action rules (only on success).
        if let Ok(ref value) = result {
            if let Err(rule_err) = self.rule_engine.apply_post_action_rules(
                &req.action_type_name,
                &req,
                value,
            ) {
                tracing::warn!(
                    action_type = %req.action_type_name,
                    error = %rule_err,
                    "post-action rule failed (non-fatal)"
                );
            }
        }

        result
    }

    /// Internal routing logic: maps action type names to tool calls or
    /// internal ontology operations.
    async fn route_action(
        &self,
        req: &ExecuteActionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        match req.action_type_name.as_str() {
            // -- Internal ontology operations (no ZeroClaw tool needed) --

            "CreateTask" => self.handle_create_task(req),
            "UpdateTask" => self.handle_update_task(req),
            "ListTasks" => self.handle_list_tasks(req),
            "SavePreference" => self.handle_save_preference(req),
            "RecordDecision" => self.handle_record_decision(req),

            // -- ZeroClaw tool-backed actions --

            "SendMessage" => {
                // Route to the appropriate channel tool based on params.channel.
                let channel = req
                    .params
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .or(req.channel.as_deref())
                    .unwrap_or("default");

                let tool_name = match channel {
                    "kakao" | "telegram" | "discord" | "slack" | "email" => {
                        // ZeroClaw typically names channel tools by the channel.
                        // Fallback to http_request if no specific tool exists.
                        format!("{channel}_send")
                    }
                    _ => "http_request".to_string(),
                };

                self.call_tool_or_fallback(&tool_name, &req.params).await
            }

            "FetchResource" => self.call_tool_or_fallback("web_fetch", &req.params).await,

            "WebSearch" => self.call_tool_or_fallback("web_search", &req.params).await,

            "SummarizeDocument" => {
                // Load document content from ontology, then use the LLM.
                // For now, pass through to a summarization prompt or tool.
                self.handle_summarize_document(req).await
            }

            "ReadDocument" => {
                let tool_name = self.select_document_tool(&req.params);
                self.call_tool_or_fallback(&tool_name, &req.params).await
            }

            "RunCommand" => self.call_tool_or_fallback("shell", &req.params).await,

            "PlanTasks" => self.call_tool_or_fallback("task_plan", &req.params).await,

            "CreateEvent" | "UpdateEvent" | "ListEvents" => {
                self.call_tool_or_fallback("schedule", &req.params).await
            }

            "StartSession" | "EndSession" => {
                // Sessions are internal ontology operations + optional tool hooks.
                self.handle_session(req)
            }

            other => {
                anyhow::bail!(
                    "unknown action type '{}' — register it in ontology_action_types and add routing in ActionDispatcher",
                    other
                )
            }
        }
    }

    // -----------------------------------------------------------------------
    // Internal ontology operation handlers
    // -----------------------------------------------------------------------

    fn handle_create_task(&self, req: &ExecuteActionRequest) -> anyhow::Result<serde_json::Value> {
        let title = req
            .params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled task");

        let properties = req
            .params
            .get("properties")
            .cloned()
            .unwrap_or(json!({"status": "open"}));

        let task_id =
            self.repo
                .create_object("Task", Some(title), &properties, &req.owner_user_id)?;

        // Create links specified in the request.
        if let Some(links) = req.params.get("links").and_then(|v| v.as_array()) {
            for link_val in links {
                if let (Some(link_type), Some(to_id)) = (
                    link_val.get("link_type").and_then(|v| v.as_str()),
                    link_val.get("to_object_id").and_then(|v| v.as_i64()),
                ) {
                    let _ = self.repo.create_link(link_type, task_id, to_id, None);
                }
            }
        }

        Ok(json!({"success": true, "task_object_id": task_id}))
    }

    fn handle_update_task(&self, req: &ExecuteActionRequest) -> anyhow::Result<serde_json::Value> {
        let object_id = req
            .primary_object_id
            .or_else(|| req.params.get("object_id").and_then(|v| v.as_i64()))
            .ok_or_else(|| anyhow::anyhow!("UpdateTask requires primary_object_id"))?;

        let title = req.params.get("title").and_then(|v| v.as_str());
        let properties = req.params.get("properties");

        self.repo.update_object(object_id, title, properties)?;
        Ok(json!({"success": true, "updated_object_id": object_id}))
    }

    fn handle_list_tasks(&self, req: &ExecuteActionRequest) -> anyhow::Result<serde_json::Value> {
        let limit = req
            .params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20) as usize;
        let query = req
            .params
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let tasks = self
            .repo
            .search_objects(&req.owner_user_id, Some("Task"), query, limit)?;

        let task_summaries: Vec<serde_json::Value> = tasks
            .iter()
            .map(|t| {
                json!({
                    "id": t.id,
                    "title": t.title,
                    "properties": t.properties,
                    "updated_at": t.updated_at,
                })
            })
            .collect();

        Ok(json!({"tasks": task_summaries, "count": task_summaries.len()}))
    }

    fn handle_save_preference(
        &self,
        req: &ExecuteActionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        let key = req
            .params
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed_preference");
        let value = req
            .params
            .get("value")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let pref_id = self.repo.ensure_object(
            "Preference",
            key,
            &json!({"value": value}),
            &req.owner_user_id,
        )?;

        // Update properties if the preference already existed.
        self.repo
            .update_object(pref_id, None, Some(&json!({"value": value})))?;

        Ok(json!({"success": true, "preference_id": pref_id}))
    }

    fn handle_record_decision(
        &self,
        _req: &ExecuteActionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        // Decisions are stored as action log entries — the action log itself
        // is the record. We just acknowledge it.
        Ok(json!({"success": true, "recorded": true}))
    }

    fn handle_session(&self, req: &ExecuteActionRequest) -> anyhow::Result<serde_json::Value> {
        let session_title = req
            .params
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled session");

        match req.action_type_name.as_str() {
            "StartSession" => {
                let session_id = self.repo.create_object(
                    "Meeting",
                    Some(session_title),
                    &json!({"status": "active"}),
                    &req.owner_user_id,
                )?;
                Ok(json!({"success": true, "session_object_id": session_id}))
            }
            "EndSession" => {
                if let Some(obj_id) = req.primary_object_id {
                    self.repo
                        .update_object(obj_id, None, Some(&json!({"status": "ended"})))?;
                }
                Ok(json!({"success": true, "ended": true}))
            }
            _ => Ok(json!({"success": false, "error": "unexpected session action"})),
        }
    }

    // -----------------------------------------------------------------------
    // ZeroClaw tool helpers
    // -----------------------------------------------------------------------

    /// Call a ZeroClaw tool by name, falling back to an error if not found.
    async fn call_tool_or_fallback(
        &self,
        tool_name: &str,
        params: &serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        if let Some(tool) = self.tools.get(tool_name) {
            let result: ToolResult = tool.execute(params.clone()).await?;
            Ok(json!({
                "success": result.success,
                "output": result.output,
                "error": result.error,
            }))
        } else {
            // Try a looser match — some tools have different naming.
            let alt_names = [
                format!("{tool_name}_tool"),
                tool_name.replace('-', "_"),
            ];
            for alt in &alt_names {
                if let Some(tool) = self.tools.get(alt.as_str()) {
                    let result: ToolResult = tool.execute(params.clone()).await?;
                    return Ok(json!({
                        "success": result.success,
                        "output": result.output,
                        "error": result.error,
                    }));
                }
            }

            anyhow::bail!(
                "ZeroClaw tool '{}' not found in registry. Available tools: {:?}",
                tool_name,
                self.tools.keys().take(10).collect::<Vec<_>>()
            )
        }
    }

    /// Select the appropriate document-reading tool based on file extension.
    fn select_document_tool(&self, params: &serde_json::Value) -> String {
        let path = params
            .get("path")
            .or_else(|| params.get("url"))
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if path.ends_with(".pdf") {
            "pdf_read".to_string()
        } else if path.ends_with(".docx") {
            "docx_read".to_string()
        } else if path.ends_with(".xlsx") {
            "xlsx_read".to_string()
        } else if path.ends_with(".pptx") {
            "pptx_read".to_string()
        } else {
            "file_read".to_string()
        }
    }

    /// Summarize a document: fetch content from ontology, then call a summary tool.
    async fn handle_summarize_document(
        &self,
        req: &ExecuteActionRequest,
    ) -> anyhow::Result<serde_json::Value> {
        // If a document_object_id is provided, load its content from the ontology.
        let doc_content = if let Some(doc_id) = req
            .params
            .get("document_object_id")
            .and_then(|v| v.as_i64())
        {
            if let Some(obj) = self.repo.get_object_for_owner(doc_id, &req.owner_user_id)? {
                obj.properties
                    .get("content")
                    .or_else(|| obj.properties.get("raw_body"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            } else {
                String::new()
            }
        } else {
            req.params
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        if doc_content.is_empty() {
            return Ok(json!({"success": false, "error": "no document content to summarize"}));
        }

        let style = req
            .params
            .get("summary_style")
            .and_then(|v| v.as_str())
            .unwrap_or("bullet_points");

        // The actual summarization would be done by the LLM in the agent loop.
        // Here we return the content and style so the agent can process it.
        Ok(json!({
            "success": true,
            "content_length": doc_content.len(),
            "summary_style": style,
            "content_preview": &doc_content[..doc_content.len().min(500)],
            "requires_llm_summarization": true,
        }))
    }
}
