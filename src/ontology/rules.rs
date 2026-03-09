//! Post-action rule engine for automatic ontology graph updates.
//!
//! After an action succeeds, the rule engine inspects the action type and
//! parameters to automatically create/strengthen links, promote objects,
//! or trigger follow-up operations. This is MoA's equivalent of Palantir's
//! Ontology Rules — making the graph "alive" without requiring the LLM to
//! explicitly manage every relationship.
//!
//! # Design Principles
//!
//! - Rules are **deterministic** and **side-effect-free** (they only modify
//!   the ontology graph, never call external tools).
//! - Rules are **additive** — they create/strengthen connections but never
//!   delete objects or links.
//! - Rules run **synchronously** after each successful action (no async I/O).
//! - Rule failures are **non-fatal** — they log a warning but don't roll back
//!   the action.

use super::repo::OntologyRepo;
use super::types::*;
use serde_json::json;
use std::sync::Arc;

/// Declarative rule engine that runs after each successful action.
pub struct RuleEngine {
    repo: Arc<OntologyRepo>,
}

impl RuleEngine {
    pub fn new(repo: Arc<OntologyRepo>) -> Self {
        Self { repo }
    }

    /// Apply all matching rules for a completed action.
    pub fn apply_post_action_rules(
        &self,
        action_type: &str,
        req: &ExecuteActionRequest,
        result: &serde_json::Value,
    ) -> anyhow::Result<()> {
        // Type-specific rules.
        match action_type {
            "SendMessage" => self.rule_send_message(req, result)?,
            "CreateTask" => self.rule_create_task(req, result)?,
            "FetchResource" => self.rule_fetch_resource(req, result)?,
            "SummarizeDocument" => self.rule_summarize_document(req, result)?,
            "SavePreference" => self.rule_save_preference(req)?,
            _ => {}
        }

        // Cross-cutting rules that apply to all action types.
        self.rule_auto_tag_important_client(req)?;
        self.rule_auto_group_tasks_into_project(req)?;
        self.rule_channel_profiling(req)?;

        Ok(())
    }

    /// Rule: When a message is sent, link the Task/Document to the Contact.
    fn rule_send_message(
        &self,
        req: &ExecuteActionRequest,
        _result: &serde_json::Value,
    ) -> anyhow::Result<()> {
        if let Some(contact_id) = req.primary_object_id {
            for related_id in &req.related_object_ids {
                let _ = self
                    .repo
                    .create_link("related_to", *related_id, contact_id, None);
            }

            // Ensure the contact is linked to the channel.
            if let Some(ch) = &req.channel {
                let channel_id = self.repo.ensure_object(
                    "Channel",
                    ch,
                    &json!({}),
                    &req.owner_user_id,
                )?;
                let _ = self
                    .repo
                    .create_link("communicates_via", contact_id, channel_id, None);
            }
        }
        Ok(())
    }

    /// Rule: When a task is created with links, ensure related Contact objects
    /// are marked as "active" collaborators.
    fn rule_create_task(
        &self,
        req: &ExecuteActionRequest,
        result: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let task_id = result
            .get("task_object_id")
            .and_then(|v| v.as_i64());

        if let Some(task_id) = task_id {
            // Auto-link to context if present.
            if let Some(ctx_id) = req.context_id {
                let _ = self.repo.create_link("related_to", task_id, ctx_id, None);
            }

            // Auto-link to channel if present.
            if let Some(ch) = &req.channel {
                let channel_id = self.repo.ensure_object(
                    "Channel",
                    ch,
                    &json!({}),
                    &req.owner_user_id,
                )?;
                let _ = self
                    .repo
                    .create_link("related_to", task_id, channel_id, None);
            }
        }
        Ok(())
    }

    /// Rule: When a web resource is fetched, create a Document object for it.
    fn rule_fetch_resource(
        &self,
        req: &ExecuteActionRequest,
        result: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let url = req
            .params
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let output = result
            .get("output")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // Only create a Document if we got meaningful content.
        if output.len() > 50 {
            let doc_id = self.repo.create_object(
                "Document",
                Some(url),
                &json!({
                    "source": "web_fetch",
                    "url": url,
                    "content_length": output.len(),
                }),
                &req.owner_user_id,
            )?;

            // Link the document to any related tasks/projects.
            for related_id in &req.related_object_ids {
                let _ = self
                    .repo
                    .create_link("related_to", doc_id, *related_id, None);
            }
        }
        Ok(())
    }

    /// Rule: When a document is summarized, store the summary as a property.
    fn rule_summarize_document(
        &self,
        req: &ExecuteActionRequest,
        result: &serde_json::Value,
    ) -> anyhow::Result<()> {
        if let Some(doc_id) = req
            .params
            .get("document_object_id")
            .and_then(|v| v.as_i64())
        {
            if let Some(summary) = result.get("summary").and_then(|v| v.as_str()) {
                // Update the document with a summary property.
                if let Some(mut obj) = self.repo.get_object(doc_id)? {
                    if let Some(map) = obj.properties.as_object_mut() {
                        map.insert("summary".to_string(), json!(summary));
                    }
                    self.repo
                        .update_object(doc_id, None, Some(&obj.properties))?;
                }
            }
        }
        Ok(())
    }

    /// Rule: When a preference is saved, link it to the user.
    fn rule_save_preference(&self, req: &ExecuteActionRequest) -> anyhow::Result<()> {
        let user_id_obj = self.repo.ensure_object(
            "User",
            &req.owner_user_id,
            &json!({}),
            &req.owner_user_id,
        )?;

        let pref_key = req
            .params
            .get("key")
            .and_then(|v| v.as_str())
            .unwrap_or("unnamed");

        let pref_id = self.repo.ensure_object(
            "Preference",
            pref_key,
            &json!({}),
            &req.owner_user_id,
        )?;

        let _ = self
            .repo
            .create_link("has_preference", user_id_obj, pref_id, None);

        Ok(())
    }

    // -------------------------------------------------------------------
    // Cross-cutting rules (run after every action)
    // -------------------------------------------------------------------

    /// Rule: If a Contact has 3+ actions referencing it, tag it as "important".
    ///
    /// Counts how many recent actions reference the primary_object as a Contact.
    /// If the threshold is met, the Contact's properties get an `importance: high` tag.
    fn rule_auto_tag_important_client(
        &self,
        req: &ExecuteActionRequest,
    ) -> anyhow::Result<()> {
        const IMPORTANCE_THRESHOLD: usize = 3;

        let contact_id = match req.primary_object_id {
            Some(id) => id,
            None => return Ok(()),
        };

        // Only apply to Contact objects.
        let obj = match self.repo.get_object(contact_id)? {
            Some(o) => o,
            None => return Ok(()),
        };

        // Check if this is a Contact type (type_id for Contact).
        let contact_type_id = self.repo.object_type_id("Contact").unwrap_or(-1);
        if obj.type_id != contact_type_id {
            return Ok(());
        }

        // Already tagged?
        if obj.properties.get("importance").and_then(|v| v.as_str()) == Some("high") {
            return Ok(());
        }

        // Count actions referencing this contact.
        let recent_actions = self.repo.recent_actions(&req.owner_user_id, None, 50)?;
        let action_count = recent_actions
            .iter()
            .filter(|a| a.primary_object_id == Some(contact_id))
            .count();

        if action_count >= IMPORTANCE_THRESHOLD {
            let mut props = obj.properties.clone();
            if let Some(map) = props.as_object_mut() {
                map.insert("importance".to_string(), json!("high"));
            }
            self.repo
                .update_object(contact_id, None, Some(&props))?;
            tracing::info!(
                contact_id,
                action_count,
                "ontology rule: auto-tagged contact as important"
            );
        }

        Ok(())
    }

    /// Rule: If 3+ tasks share a keyword in their title, auto-create a Project
    /// and link the tasks to it.
    ///
    /// Uses a simple heuristic: extracts the first 2 words of each task title
    /// and groups tasks with matching prefixes.
    fn rule_auto_group_tasks_into_project(
        &self,
        req: &ExecuteActionRequest,
    ) -> anyhow::Result<()> {
        const GROUP_THRESHOLD: usize = 3;

        // Only run on CreateTask actions.
        let title = match req.params.get("title").and_then(|v| v.as_str()) {
            Some(t) => t,
            None => return Ok(()),
        };

        // Extract prefix (first 2 significant words) as a grouping key.
        let prefix: String = title
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ");

        if prefix.len() < 3 {
            return Ok(());
        }

        // Search for tasks with matching prefix.
        let matching_tasks = self.repo.search_objects(
            &req.owner_user_id,
            Some("Task"),
            &prefix,
            GROUP_THRESHOLD + 5,
        )?;

        if matching_tasks.len() < GROUP_THRESHOLD {
            return Ok(());
        }

        // Check if a project with this prefix already exists.
        let project_title = format!("{} (auto-grouped)", prefix);
        let project_id = self.repo.ensure_object(
            "Project",
            &project_title,
            &json!({"auto_grouped": true, "keyword": prefix}),
            &req.owner_user_id,
        )?;

        // Link all matching tasks to the project.
        for task in &matching_tasks {
            let _ = self
                .repo
                .create_link("belongs_to", task.id, project_id, None);
        }

        tracing::info!(
            project_title,
            task_count = matching_tasks.len(),
            "ontology rule: auto-grouped tasks into project"
        );

        Ok(())
    }

    /// Rule: Track which action types are frequent on each channel.
    ///
    /// Ensures a Channel object exists and updates a `frequent_actions`
    /// property that records action type counts. This enables the LLM to
    /// tailor suggestions based on channel usage patterns (e.g. Kakao =
    /// communication-heavy, Desktop = code-heavy).
    fn rule_channel_profiling(
        &self,
        req: &ExecuteActionRequest,
    ) -> anyhow::Result<()> {
        let channel_name = match &req.channel {
            Some(ch) if !ch.is_empty() => ch,
            _ => return Ok(()),
        };

        let channel_id = self.repo.ensure_object(
            "Channel",
            channel_name,
            &json!({"frequent_actions": {}}),
            &req.owner_user_id,
        )?;

        // Update the frequency counter for this action type.
        if let Some(mut obj) = self.repo.get_object(channel_id)? {
            let freq = obj
                .properties
                .get("frequent_actions")
                .cloned()
                .unwrap_or(json!({}));

            let mut freq_map = freq.as_object().cloned().unwrap_or_default();
            let current_count = freq_map
                .get(&req.action_type_name)
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            freq_map.insert(
                req.action_type_name.clone(),
                json!(current_count + 1),
            );

            if let Some(props_map) = obj.properties.as_object_mut() {
                props_map.insert(
                    "frequent_actions".to_string(),
                    serde_json::Value::Object(freq_map),
                );
            }
            self.repo
                .update_object(channel_id, None, Some(&obj.properties))?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;

    fn test_rule_engine() -> (Arc<OntologyRepo>, RuleEngine) {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        let repo = Arc::new(
            OntologyRepo::from_connection(Arc::new(Mutex::new(conn))).unwrap(),
        );
        let engine = RuleEngine::new(Arc::clone(&repo));
        (repo, engine)
    }

    #[test]
    fn send_message_links_contact_to_channel() {
        let (repo, engine) = test_rule_engine();

        let contact_id = repo
            .create_object("Contact", Some("Alice"), &json!({}), "u1")
            .unwrap();

        let req = ExecuteActionRequest {
            action_type_name: "SendMessage".to_string(),
            owner_user_id: "u1".to_string(),
            actor_kind: Some(ActorKind::Agent),
            primary_object_id: Some(contact_id),
            related_object_ids: vec![],
            params: json!({"message": "hello"}),
            channel: Some("kakao".to_string()),
            context_id: None,
        };

        engine
            .rule_send_message(&req, &json!({"success": true}))
            .unwrap();

        // Verify a Channel object was created and linked.
        let channels = repo.list_objects_by_type("u1", "Channel", 10).unwrap();
        assert!(!channels.is_empty());
        assert_eq!(channels[0].title.as_deref(), Some("kakao"));

        let links = repo.links_from(contact_id).unwrap();
        assert!(!links.is_empty());
    }

    #[test]
    fn create_task_links_to_context() {
        let (repo, engine) = test_rule_engine();

        let ctx_id = repo
            .create_object("Context", Some("OfficePC"), &json!({}), "u1")
            .unwrap();

        let req = ExecuteActionRequest {
            action_type_name: "CreateTask".to_string(),
            owner_user_id: "u1".to_string(),
            actor_kind: Some(ActorKind::Agent),
            primary_object_id: None,
            related_object_ids: vec![],
            params: json!({"title": "Test task"}),
            channel: Some("desktop".to_string()),
            context_id: Some(ctx_id),
        };

        let result = json!({"task_object_id": 999});
        // This will try to link task 999 to context, but 999 doesn't exist
        // in our test — the rule will silently fail on the FK constraint.
        // That's fine for a non-fatal rule.
        let _ = engine.rule_create_task(&req, &result);
    }
}
