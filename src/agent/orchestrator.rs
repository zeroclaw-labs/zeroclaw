use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use crate::agent::executor::{self, AgentExecutionResult, AgentExecutionSink};
use crate::aria::db::AriaDb;
use crate::config::BrowserConfig;
use crate::memory::Memory;
use crate::prompt::{SkillDescriptor, SystemPromptBuilder};
use crate::providers::Provider;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool};

pub struct LiveTurnConfig<'a> {
    pub provider: &'a dyn Provider,
    pub security: &'a Arc<SecurityPolicy>,
    pub memory: Arc<dyn Memory>,
    pub composio_api_key: Option<&'a str>,
    pub browser_config: &'a BrowserConfig,
    pub registry_db: &'a AriaDb,
    pub workspace_dir: &'a Path,
    pub tenant_id: &'a str,
    pub model: &'a str,
    pub temperature: f64,
    pub mode_hint: &'a str,
    pub max_turns: Option<u32>,
    pub external_tool_context: Option<executor::ExternalToolContext>,
}

async fn build_memory_context(mem: &dyn Memory, user_msg: &str) -> String {
    let mut context = String::new();
    if let Ok(entries) = mem.recall(user_msg, 5).await {
        if !entries.is_empty() {
            context.push_str("[Memory context]\n");
            for entry in &entries {
                context.push_str("- ");
                context.push_str(&entry.key);
                context.push_str(": ");
                context.push_str(&entry.content);
                context.push('\n');
            }
            context.push('\n');
        }
    }
    context
}

fn load_registry_tools_prompt_section(
    db: &AriaDb,
    tenant_id: &str,
    allowed_tool_names: &HashSet<String>,
) -> anyhow::Result<String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT name, description, version, schema
             FROM aria_tools
             WHERE tenant_id=?1 AND status='active'
             ORDER BY updated_at DESC",
        )?;
        let mut rows = stmt.query([tenant_id])?;
        let mut out = String::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            if !allowed_tool_names.contains(&name) {
                continue;
            }
            if out.is_empty() {
                out.push_str("## Available Tools\n\n");
            }
            let description: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let version: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(1);
            let schema: String = row
                .get::<_, Option<String>>(3)?
                .unwrap_or_else(|| "{}".to_string());
            out.push_str(&format!(
                "- **{}** (v{}): {}\n  Schema: {}\n",
                name, version, description, schema
            ));
        }
        Ok(out)
    })
}

fn load_registry_agents_prompt_section(db: &AriaDb, tenant_id: &str) -> anyhow::Result<String> {
    db.with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT name, description, version, model
             FROM aria_agents
             WHERE tenant_id=?1 AND status='active'
             ORDER BY updated_at DESC",
        )?;
        let mut rows = stmt.query([tenant_id])?;
        let mut out = String::new();
        while let Some(row) = rows.next()? {
            if out.is_empty() {
                out.push_str("## Available Agents\n\n");
            }
            let name: String = row.get(0)?;
            let description: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
            let version: i64 = row.get::<_, Option<i64>>(2)?.unwrap_or(1);
            let model: String = row
                .get::<_, Option<String>>(3)?
                .unwrap_or_else(|| "default".to_string());
            out.push_str(&format!(
                "- **{}** (v{}): {}\n  Model: {}\n",
                name, version, description, model
            ));
        }
        Ok(out)
    })
}

fn build_live_system_prompt(
    workspace_dir: &Path,
    model: &str,
    tools: &[Box<dyn Tool>],
    registry_db: &AriaDb,
    tenant_id: &str,
    mode_hint: &str,
) -> String {
    let skill_descriptors: Vec<SkillDescriptor> = crate::skills::load_skills(workspace_dir)
        .into_iter()
        .map(|s| SkillDescriptor {
            name: s.name,
            description: s.description,
        })
        .collect();

    let tool_descs_owned: Vec<(String, String)> = tools
        .iter()
        .map(|t| (t.name().to_string(), t.description().to_string()))
        .collect();
    let tool_descs: Vec<(&str, &str)> = tool_descs_owned
        .iter()
        .map(|(name, desc)| (name.as_str(), desc.as_str()))
        .collect();
    let allowed_names: HashSet<String> = tool_descs_owned.iter().map(|(n, _)| n.clone()).collect();

    let registry_tools_section =
        load_registry_tools_prompt_section(registry_db, tenant_id, &allowed_names).unwrap_or_else(
            |e| {
                tracing::warn!(
                    tenant_id,
                    error = %e,
                    "Failed to build registry tools prompt section"
                );
                String::new()
            },
        );

    let registry_agents_section = load_registry_agents_prompt_section(registry_db, tenant_id)
        .unwrap_or_else(|e| {
            tracing::warn!(
                tenant_id,
                error = %e,
                "Failed to build registry agents prompt section"
            );
            String::new()
        });

    let prompt = SystemPromptBuilder::new(workspace_dir)
        .tools(&tool_descs)
        .skills(&skill_descriptors)
        .model(model)
        .registry_tools_section(registry_tools_section)
        .registry_agents_section(registry_agents_section)
        .build();

    if mode_hint.is_empty() {
        prompt
    } else {
        format!("{prompt}\n\n{mode_hint}")
    }
}

pub async fn run_live_turn(
    config: LiveTurnConfig<'_>,
    user_input: &str,
    sink: Option<&mut dyn AgentExecutionSink>,
) -> anyhow::Result<AgentExecutionResult> {
    let tools = tools::all_tools_for_tenant(
        config.security,
        config.memory.clone(),
        config.composio_api_key,
        config.browser_config,
        config.registry_db,
        config.tenant_id,
    );

    let system_prompt = build_live_system_prompt(
        config.workspace_dir,
        config.model,
        &tools,
        config.registry_db,
        config.tenant_id,
        config.mode_hint,
    );

    let memory_context = build_memory_context(config.memory.as_ref(), user_input).await;
    let enriched = if memory_context.is_empty() {
        user_input.to_string()
    } else {
        format!("{memory_context}{user_input}")
    };

    executor::execute_agent_with_sink(
        config.provider,
        &tools,
        &system_prompt,
        &enriched,
        config.model,
        config.temperature,
        config.max_turns,
        config.external_tool_context,
        sink,
    )
    .await
}
