//! Selected context references resolve from their existing owners on use.
//! Native paired Colony is the shared operator surface; distinct principals'
//! conversations are excluded even when an arbitrary source key is submitted.

use anyhow::{Context, Result};
use serde::Serialize;
use zeroclaw_api::{model_provider::ConversationMessage, principal::PrincipalId};
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyContextSource {
    pub key: String,
    pub preview: String,
    pub category: String,
}

fn native_visible(principal: Option<&str>) -> bool {
    principal.is_none_or(|id| id == PrincipalId::SHARED_OPERATOR)
}

fn visible_transcript(messages: &[ConversationMessage]) -> String {
    let mut output = String::new();
    for message in messages {
        match message {
            ConversationMessage::Chat(chat)
                if matches!(chat.role.as_str(), "user" | "assistant") =>
            {
                output.push_str(&format!("{}: {}\n", chat.role, chat.content));
            }
            ConversationMessage::AssistantToolCalls {
                text: Some(text), ..
            } => {
                output.push_str(&format!("assistant: {text}\n"));
            }
            ConversationMessage::ToolResults(results) => {
                for result in results {
                    output.push_str(&format!(
                        "tool result: {}\n",
                        AcpSessionStore::bounded_tool_output(&result.content)
                    ));
                }
            }
            _ => {}
        }
    }
    output
}

pub async fn list_context_sources(
    config: &Config,
    agent: &str,
) -> Result<Vec<ColonyContextSource>> {
    anyhow::ensure!(config.agent(agent).is_some(), "unknown context agent");
    let memory = zeroclaw_memory::create_memory_for_agent(config, agent, None).await?;
    let mut sources = memory
        .list(None, None)
        .await?
        .into_iter()
        .take(200)
        .map(|entry| ColonyContextSource {
            key: format!("memory:{}", entry.key),
            preview: entry.content.chars().take(500).collect(),
            category: entry.category.to_string(),
        })
        .collect::<Vec<_>>();
    let sessions = AcpSessionStore::new(&config.data_dir)?;
    for summary in sessions
        .list_live_sessions_by_agent(agent)?
        .into_iter()
        .filter(|s| native_visible(s.principal_id.as_deref()))
        .take(100)
    {
        let key = format!("session:{}", summary.session_uuid);
        let content = resolve_selected_context(config, agent, &key).await?;
        sources.push(ColonyContextSource {
            key,
            preview: content.chars().take(500).collect(),
            category: "conversation".into(),
        });
    }
    let backend =
        zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)?;
    for summary in backend
        .list_sessions_with_metadata()
        .into_iter()
        .filter(|s| {
            s.agent_alias.as_deref() == Some(agent) && native_visible(s.principal_id.as_deref())
        })
        .take(100)
    {
        let key = format!("channel_session:{}", summary.key);
        let content = resolve_selected_context(config, agent, &key).await?;
        sources.push(ColonyContextSource {
            key,
            preview: content.chars().take(500).collect(),
            category: "conversation".into(),
        });
    }
    Ok(sources)
}

pub async fn resolve_selected_context(config: &Config, agent: &str, key: &str) -> Result<String> {
    anyhow::ensure!(config.agent(agent).is_some(), "unknown context agent");
    let content = if let Some(id) = key.strip_prefix("session:") {
        let sessions = AcpSessionStore::new(&config.data_dir)?;
        let session = sessions
            .load_session_for_agent(id, agent)?
            .context("selected conversation is unavailable")?;
        anyhow::ensure!(
            native_visible(session.principal_id.as_deref()),
            "selected conversation belongs to another principal"
        );
        visible_transcript(&session.messages)
    } else if let Some(id) = key.strip_prefix("channel_session:") {
        let backend = zeroclaw_infra::make_session_backend(
            &config.data_dir,
            &config.channels.session_backend,
        )?;
        let summary = backend
            .list_sessions_with_metadata()
            .into_iter()
            .find(|s| s.key == id)
            .context("selected conversation is unavailable")?;
        anyhow::ensure!(
            summary.agent_alias.as_deref() == Some(agent)
                && native_visible(summary.principal_id.as_deref()),
            "selected conversation belongs to another agent or principal"
        );
        backend
            .try_load(id)?
            .into_iter()
            .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
            .map(|m| format!("{}: {}\n", m.role, m.content))
            .collect()
    } else {
        let memory = zeroclaw_memory::create_memory_for_agent(config, agent, None).await?;
        memory
            .get(key.strip_prefix("memory:").unwrap_or(key))
            .await?
            .context("selected memory is unavailable")?
            .content
    };
    anyhow::ensure!(
        content.len() <= 512_000,
        "selected context is too large; choose a smaller source"
    );
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::model_provider::ChatMessage;

    #[tokio::test]
    async fn selected_conversation_enforces_agent_and_principal_and_excludes_hidden_text() {
        let tmp = tempfile::tempdir().unwrap();
        let mut config = Config {
            data_dir: tmp.path().into(),
            ..Default::default()
        };
        for alias in ["worker", "other"] {
            config.agents.insert(alias.into(), Default::default());
        }
        let store = AcpSessionStore::new(&config.data_dir).unwrap();
        store
            .create_session("selected", "worker", "", Some(PrincipalId::SHARED_OPERATOR))
            .unwrap();
        store
            .append_turn(
                "selected",
                &[
                    ConversationMessage::Chat(ChatMessage::user("Selected prior preference")),
                    ConversationMessage::Chat(ChatMessage::system("Private system instructions")),
                    ConversationMessage::AssistantToolCalls {
                        text: Some("Visible reply".into()),
                        tool_calls: vec![],
                        reasoning_content: Some("Hidden reasoning".into()),
                    },
                ],
            )
            .unwrap();
        let selected = resolve_selected_context(&config, "worker", "session:selected")
            .await
            .unwrap();
        assert!(selected.contains("Selected prior preference"));
        assert!(selected.contains("Visible reply"));
        assert!(!selected.contains("Private system instructions"));
        assert!(!selected.contains("Hidden reasoning"));
        assert!(
            resolve_selected_context(&config, "other", "session:selected")
                .await
                .is_err()
        );
        store
            .create_session("foreign", "worker", "", Some("distinct-principal"))
            .unwrap();
        assert!(
            resolve_selected_context(&config, "worker", "session:foreign")
                .await
                .is_err()
        );
    }
}
