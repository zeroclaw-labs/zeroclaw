//! Agent room access resolves the current canonical graph at every invocation.
//! The tool binds a trusted agent identity; prompts cannot supply the sender.

use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use serde_json::{Value, json};
use zeroclaw_api::tool::{Tool, ToolOutput, ToolResult};
use zeroclaw_config::schema::Config;

use crate::{ColonyMessage, store::ColonyStore};

pub(super) fn new(
    config: Arc<RwLock<Config>>,
    store: Arc<ColonyStore>,
    alias: String,
) -> Box<dyn Tool> {
    Box::new(ColonyRoomTool {
        config,
        store,
        alias,
    })
}

struct ColonyRoomTool {
    config: Arc<RwLock<Config>>,
    store: Arc<ColonyStore>,
    alias: String,
}

zeroclaw_api::tool_attribution!(
    ColonyRoomTool,
    zeroclaw_api::attribution::ToolKind::ColonyRoom
);

fn rejected(key: &str) -> ToolResult {
    ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some(zeroclaw_runtime::i18n::get_required_cli_string(key)),
    }
}

#[async_trait::async_trait]
impl Tool for ColonyRoomTool {
    fn name(&self) -> &str {
        "colony_room"
    }
    fn description(&self) -> &str {
        static DESCRIPTION: OnceLock<String> = OnceLock::new();
        DESCRIPTION.get_or_init(|| {
            zeroclaw_runtime::i18n::get_required_cli_string("colony-room-tool-description")
        })
    }
    fn parameters_schema(&self) -> Value {
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["read","post"]},
            "room_id":{"type":"string"},"text":{"type":"string","maxLength":64000},
            "limit":{"type":"integer","minimum":1,"maximum":100,"default":20}
        },"required":["action","room_id"],"additionalProperties":false})
    }
    fn output_schema(&self) -> Option<Value> {
        Some(
            json!({"type":"object","properties":{"messages":{"type":"array"},"message":{"type":"object"}}}),
        )
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let Some(action) = args
            .get("action")
            .and_then(Value::as_str)
            .filter(|action| matches!(*action, "read" | "post"))
        else {
            return Ok(rejected("colony-room-tool-error-arguments"));
        };
        let Some(room_id) = args
            .get("room_id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            return Ok(rejected("colony-room-tool-error-arguments"));
        };
        let goal_id = zeroclaw_runtime::execution_scope::current_goal_id();
        let config = self.config.read();
        let Some((colony_id, colony)) = config.colony_for_agent(&self.alias) else {
            return Ok(rejected("colony-room-tool-error-permission"));
        };
        let Some(room) = colony.rooms.iter().find(|room| room.id == room_id) else {
            return Ok(rejected("colony-room-tool-error-permission"));
        };
        let permitted = if action == "read" {
            room.readers.contains(&self.alias)
        } else {
            room.publishers.contains(&self.alias)
        };
        if !permitted
            || !config
                .agents
                .get(&self.alias)
                .is_some_and(|agent| agent.enabled)
        {
            return Ok(rejected("colony-room-tool-error-permission"));
        }
        // The run's policy filter and approval manager remain authoritative.
        // Resolve the risk-profile fact for the extra read-only safeguard too.
        if action == "post"
            && config
                .risk_profile_for_agent(&self.alias)
                .is_none_or(|risk| risk.level == zeroclaw_config::policy::AutonomyLevel::ReadOnly)
        {
            return Ok(rejected("colony-room-tool-error-permission"));
        }
        if let Some(goal) = &goal_id
            && self.store.colony_task_status(goal)?.is_terminal()
        {
            return Ok(rejected("colony-room-tool-error-permission"));
        }
        let recipient = format!("room:{room_id}");
        // Keep this short synchronous transaction under the live config guard:
        // a concurrent grant revocation cannot interleave permission and write.
        if action == "post" {
            let Some(text) = args
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty() && text.len() <= 64_000)
            else {
                return Ok(rejected("colony-room-tool-error-arguments"));
            };
            let message = ColonyMessage {
                id: uuid::Uuid::new_v4().to_string(),
                colony_id: colony_id.to_string(),
                task_id: goal_id,
                sender: self.alias.clone(),
                recipient,
                content: text.to_string(),
                created_at: chrono::Utc::now().to_rfc3339(),
                in_reply_to: None,
            };
            self.store.append_live_colony_message(&message)?;
            Ok(ToolResult {
                success: true,
                output: ToolOutput::json(json!({"message":message})),
                error: None,
            })
        } else {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 100) as u32;
            let messages = self.store.colony_room_messages(
                colony_id,
                goal_id.as_deref(),
                &recipient,
                limit,
            )?;
            Ok(ToolResult {
                success: true,
                output: ToolOutput::json(json!({"messages":messages})),
                error: None,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::colony::{ColonyConfig, ColonyRoom};

    #[tokio::test]
    async fn room_tool_has_live_independent_read_post_grants_and_no_private_messages() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ColonyStore::open(dir.path()).unwrap());
        let mut config = Config::default();
        config.agents.insert("queen".into(), Default::default());
        config.agents.insert("worker".into(), Default::default());
        config.agents.get_mut("worker").unwrap().risk_profile = "default".into();
        config
            .risk_profiles
            .insert("default".into(), Default::default());
        config.colonies.insert(
            "team".into(),
            ColonyConfig {
                name: "Team".into(),
                queen: "queen".into(),
                members: vec!["worker".into()],
                rooms: vec![ColonyRoom {
                    id: "review".into(),
                    name: "Review".into(),
                    readers: vec!["worker".into()],
                    publishers: vec!["worker".into()],
                    ..Default::default()
                }],
                ..Default::default()
            },
        );
        let config = Arc::new(RwLock::new(config));
        let tool = new(Arc::clone(&config), Arc::clone(&store), "worker".into());
        store
            .append_colony_message(&ColonyMessage {
                id: uuid::Uuid::new_v4().to_string(),
                colony_id: "team".into(),
                task_id: None,
                sender: "user".into(),
                recipient: "queen".into(),
                content: "Private direct conversation".into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                in_reply_to: None,
            })
            .unwrap();
        let posted = tool
            .execute(json!({"action":"post","room_id":"review","text":"Room finding"}))
            .await
            .unwrap();
        assert!(posted.success, "{:?}", posted.error);
        let old_goal = uuid::Uuid::new_v4().to_string();
        let old_task = serde_json::from_value(json!({"id":old_goal,"kind":"goal","agent":"queen","status":"completed","started_at":chrono::Utc::now().to_rfc3339()})).unwrap();
        zeroclaw_runtime::control_plane::TaskRegistry::create(&**store, old_task)
            .await
            .unwrap();
        store
            .append_colony_message(&ColonyMessage {
                id: uuid::Uuid::new_v4().to_string(),
                colony_id: "team".into(),
                task_id: Some(old_goal),
                sender: "worker".into(),
                recipient: "room:review".into(),
                content: "Prior goal context".into(),
                created_at: chrono::Utc::now().to_rfc3339(),
                in_reply_to: None,
            })
            .unwrap();
        let read = tool
            .execute(json!({"action":"read","room_id":"review"}))
            .await
            .unwrap();
        assert!(read.success);
        assert!(read.output.contains("Room finding"));
        assert!(!read.output.contains("Private direct conversation"));
        assert!(!read.output.contains("Prior goal context"));
        config.write().colonies.get_mut("team").unwrap().rooms[0]
            .readers
            .clear();
        assert!(
            !tool
                .execute(json!({"action":"read","room_id":"review"}))
                .await
                .unwrap()
                .success
        );
        assert!(
            tool.execute(
                json!({"action":"post","room_id":"review","text":"Publish-only permission"})
            )
            .await
            .unwrap()
            .success
        );
        config.write().colonies.get_mut("team").unwrap().rooms[0]
            .publishers
            .clear();
        assert!(
            !tool
                .execute(json!({"action":"post","room_id":"review","text":"Revoked"}))
                .await
                .unwrap()
                .success
        );
    }
}
