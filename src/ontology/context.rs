//! Context snapshot builder for LLM prompt injection.
//!
//! Builds a structured snapshot of the user's current ontology state —
//! recent contacts, tasks, projects, and actions — so the LLM can reason
//! over a "map of reality" instead of raw text chunks.

use super::repo::OntologyRepo;
use super::types::*;
use std::sync::Arc;

/// Builds context snapshots from the ontology for LLM consumption.
pub struct ContextBuilder {
    repo: Arc<OntologyRepo>,
}

impl ContextBuilder {
    pub fn new(repo: Arc<OntologyRepo>) -> Self {
        Self { repo }
    }

    /// Build a context snapshot for the given user/channel.
    ///
    /// This is the primary entry point called by the `ontology.get_context_snapshot`
    /// tool. The resulting JSON is injected into the LLM system prompt or tool
    /// response so the agent understands the user's current world state.
    pub fn build(&self, req: &ContextSnapshotRequest) -> anyhow::Result<ContextSnapshot> {
        let user = self
            .repo
            .list_objects_by_type(&req.owner_user_id, "User", 1)?
            .into_iter()
            .next();

        let current_context = self
            .repo
            .list_objects_by_type(&req.owner_user_id, "Context", 1)?
            .into_iter()
            .next();

        let recent_contacts =
            self.repo
                .list_objects_by_type(&req.owner_user_id, "Contact", 5)?;

        let recent_tasks =
            self.repo
                .list_objects_by_type(&req.owner_user_id, "Task", 10)?;

        let recent_projects =
            self.repo
                .list_objects_by_type(&req.owner_user_id, "Project", 5)?;

        // Fetch recent actions and build summaries.
        // Hard-cap at 50 regardless of caller input (schema documents max:50).
        let limit = req.limit_recent_actions.min(50);
        let raw_actions =
            self.repo
                .recent_actions(&req.owner_user_id, req.channel.as_deref(), limit)?;

        // Pre-build a cache of action_type_id → name to avoid N+1 lookups.
        let mut type_name_cache = std::collections::HashMap::new();

        let recent_actions: Vec<ActionSummary> = raw_actions
            .into_iter()
            .map(|a| {
                let action_type = type_name_cache
                    .entry(a.action_type_id)
                    .or_insert_with(|| {
                        self.repo
                            .action_type_name(a.action_type_id)
                            .unwrap_or_else(|_| format!("unknown_{}", a.action_type_id))
                    })
                    .clone();

                // Use owner-scoped lookup to prevent cross-user data leakage.
                let primary_object_title = a.primary_object_id.and_then(|oid| {
                    self.repo
                        .get_object_for_owner(oid, &req.owner_user_id)
                        .ok()
                        .flatten()
                        .and_then(|o| o.title)
                });

                ActionSummary {
                    action_type,
                    primary_object_title,
                    params_summary: summarize_json(&a.params, 3),
                    result_summary: a.result.as_ref().map(|r| summarize_json(r, 2)),
                    channel: a.channel,
                    occurred_at_utc: a.occurred_at_utc,
                    occurred_at_home: a.occurred_at_home,
                    timezone: a.timezone,
                    location: a.location,
                    status: a.status.to_string(),
                    created_at: a.created_at,
                }
            })
            .collect();

        Ok(ContextSnapshot {
            user,
            current_context,
            recent_contacts,
            recent_tasks,
            recent_projects,
            recent_actions,
        })
    }
}

/// Truncate a JSON value to at most `max_keys` top-level keys for compact display.
fn summarize_json(val: &serde_json::Value, max_keys: usize) -> serde_json::Value {
    match val {
        serde_json::Value::Object(map) => {
            let mut summary = serde_json::Map::new();
            for (i, (k, v)) in map.iter().enumerate() {
                if i >= max_keys {
                    summary.insert(
                        "...".to_string(),
                        serde_json::Value::String(format!("{} more keys", map.len() - max_keys)),
                    );
                    break;
                }
                // Truncate long string values.
                let truncated = match v {
                    serde_json::Value::String(s) if s.len() > 100 => {
                        let boundary = s.char_indices()
                            .take_while(|(i, _)| *i < 100)
                            .last()
                            .map(|(i, c)| i + c.len_utf8())
                            .unwrap_or(0);
                        serde_json::Value::String(format!("{}...", &s[..boundary]))
                    }
                    other => other.clone(),
                };
                summary.insert(k.clone(), truncated);
            }
            serde_json::Value::Object(summary)
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_json_truncates_keys() {
        let val = serde_json::json!({
            "a": 1,
            "b": 2,
            "c": 3,
            "d": 4,
        });
        let summary = summarize_json(&val, 2);
        let map = summary.as_object().unwrap();
        assert!(map.len() <= 3); // 2 keys + "..."
    }

    #[test]
    fn summarize_json_truncates_long_strings() {
        let long = "x".repeat(200);
        let val = serde_json::json!({"text": long});
        let summary = summarize_json(&val, 5);
        let text = summary["text"].as_str().unwrap();
        assert!(text.len() < 150);
        assert!(text.ends_with("..."));
    }
}
