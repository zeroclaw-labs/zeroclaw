//! Saved colony definitions and their live communication boundary.
//!
//! `Config.colonies` creates the membership, graph, setup and layout facts. It
//! references agents and channels rather than copying their profiles or policy.
//! Goal execution/history remains owned by the runtime control plane.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use zeroclaw_macros::Configurable;

use crate::schema::Config;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, zeroclaw_macros::ConfigEnum,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColonyAutonomy {
    PlanOnly,
    #[default]
    Supervised,
    Autonomous,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, zeroclaw_macros::ConfigEnum,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColonyStartMode {
    #[default]
    Review,
    Automatic,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, zeroclaw_macros::ConfigEnum,
)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ColonyResponders {
    #[default]
    Addressed,
    QueenSelected,
    Open,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyConnection {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyPrompt {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyRoom {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub readers: Vec<String>,
    #[serde(default)]
    pub publishers: Vec<String>,
    #[serde(default)]
    pub responders: ColonyResponders,
    #[serde(default = "default_room_turns")]
    pub max_turns: u32,
}

fn default_room_turns() -> u32 {
    8
}

impl Default for ColonyRoom {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            readers: Vec::new(),
            publishers: Vec::new(),
            responders: ColonyResponders::default(),
            max_turns: default_room_turns(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyChannel {
    pub id: String,
    /// Dotted reference into the existing configured channel catalogue.
    pub channel: String,
    /// Exact recipient/conversation identifier; an empty value grants nothing.
    pub conversation: String,
    #[serde(default)]
    pub inbound_agents: Vec<String>,
    #[serde(default)]
    pub outbound_agents: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyContext {
    pub agent: String,
    pub key: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ColonyPosition {
    pub x: f64,
    pub y: f64,
}

macro_rules! object_array_kind {
    ($($ty:ty),+ $(,)?) => { $(
        impl crate::traits::HasPropKind for Vec<$ty> {
            const PROP_KIND: crate::traits::PropKind = crate::traits::PropKind::ObjectArray;
        }
    )+ };
}
object_array_kind!(
    ColonyConnection,
    ColonyPrompt,
    ColonyRoom,
    ColonyChannel,
    ColonyContext
);

#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "colony"]
pub struct ColonyConfig {
    /// User-visible team name.
    #[serde(default)]
    pub name: String,
    /// Existing aliased agent coordinating this colony.
    #[serde(default)]
    pub queen: String,
    /// Exclusive member aliases; the Queen is included implicitly.
    #[serde(default)]
    pub members: Vec<String>,
    #[serde(default)]
    pub autonomy: ColonyAutonomy,
    #[serde(default)]
    pub start_mode: ColonyStartMode,
    /// Ordered directed grants. Reverse communication needs its own grant.
    #[serde(default)]
    pub connections: Vec<ColonyConnection>,
    /// Apply-now epoch for recurring instruction edits. Each admitted turn
    /// captures the prompt list and refreshes it only when this epoch changes.
    #[serde(default)]
    pub instruction_revision: u64,
    /// Ordered recurring instruction nodes, injected once per admitted run.
    #[serde(default)]
    pub prompts: Vec<ColonyPrompt>,
    #[serde(default)]
    pub rooms: Vec<ColonyRoom>,
    #[serde(default)]
    pub channels: Vec<ColonyChannel>,
    /// Explicit prior memory selections; no automatic member-memory sharing.
    #[serde(default)]
    pub baseline_context: Vec<ColonyContext>,
    /// Presentation coordinates only, never an authorization surface.
    #[serde(default)]
    pub positions: HashMap<String, ColonyPosition>,
}

impl ColonyConfig {
    #[must_use]
    pub fn contains(&self, alias: &str) -> bool {
        self.queen == alias || self.members.iter().any(|member| member == alias)
    }

    #[must_use]
    pub fn agent_aliases(&self) -> Vec<&str> {
        std::iter::once(self.queen.as_str())
            .chain(self.members.iter().map(String::as_str))
            .collect()
    }

    #[must_use]
    pub fn allows(&self, from: &str, to: &str) -> bool {
        self.contains(from)
            && self.contains(to)
            && self
                .connections
                .iter()
                .any(|edge| edge.from == from && edge.to == to)
    }
}

impl Config {
    #[must_use]
    pub fn colony_for_agent(&self, alias: &str) -> Option<(&str, &ColonyConfig)> {
        self.colonies
            .iter()
            .find(|(_, colony)| colony.contains(alias))
            .map(|(id, colony)| (id.as_str(), colony))
    }

    /// Resolve the current graph; no long-lived cached policy is authoritative.
    /// Unassigned agents retain their existing communication policy.
    #[must_use]
    pub fn colony_allows_communication(&self, from: &str, to: &str) -> bool {
        if self
            .colonies
            .values()
            .filter(|colony| colony.contains(from))
            .count()
            > 1
            || self
                .colonies
                .values()
                .filter(|colony| colony.contains(to))
                .count()
                > 1
        {
            return false;
        }
        match (self.colony_for_agent(from), self.colony_for_agent(to)) {
            (None, None) => true,
            (Some((left, colony)), Some((right, _))) if left == right => colony.allows(from, to),
            _ => false,
        }
    }

    #[must_use]
    pub fn colony_allows_external(
        &self,
        agent: &str,
        channel: &str,
        conversation: &str,
        outbound: bool,
    ) -> bool {
        if self
            .colonies
            .values()
            .filter(|colony| colony.contains(agent))
            .count()
            > 1
        {
            return false;
        }
        let Some((_, colony)) = self.colony_for_agent(agent) else {
            return true;
        };
        colony.channels.iter().any(|binding| {
            binding.channel == channel
                && !binding.conversation.trim().is_empty()
                && binding.conversation == conversation
                && if outbound {
                    &binding.outbound_agents
                } else {
                    &binding.inbound_agents
                }
                .iter()
                .any(|member| member == agent)
        })
    }

    /// Public connection descriptors deliberately exclude identity documents,
    /// transcripts, credentials, private memory and tool/risk profile contents.
    #[must_use]
    pub fn colony_connection_context(&self, alias: &str) -> String {
        let mut context = String::new();
        if let Some(agent) = self.agents.get(alias)
            && !agent.core_command.trim().is_empty()
        {
            context.push_str("\n\n[Core command]\n");
            context.push_str(&agent.core_command);
        }
        let Some((id, colony)) = self.colony_for_agent(alias) else {
            return context;
        };
        context.push_str("\n\n[Colony — read-only connection information]\n");
        context.push_str(&format!(
            "Colony: {id}; Queen: {}. Connections grant only the listed direction.\n",
            colony.queen
        ));
        context.push_str("To publish one-way to a permitted agent during an active goal, use send_message_to_peer with channel colony and target the agent alias. It queues text for their next admitted turn and grants no reply or automatic run.\n");
        for connected in colony.agent_aliases() {
            let outbound = colony.allows(alias, connected);
            let inbound = colony.allows(connected, alias);
            if connected == alias || (!outbound && !inbound) {
                continue;
            }
            if let Some(agent) = self.agents.get(connected) {
                context.push_str(&format!("Agent: {connected}; core command: {}; may send: {outbound}; may receive: {inbound}.\n", agent.core_command));
            }
        }
        for room in &colony.rooms {
            let read = room.readers.iter().any(|member| member == alias);
            let post = room.publishers.iter().any(|member| member == alias);
            if read || post {
                context.push_str(&format!("Room: {}; name: {}; may read: {read}; may post: {post}. Use colony_room with room_id {}.\n", room.id, room.name, room.id));
            }
        }
        for channel in &colony.channels {
            let inbound = channel.inbound_agents.iter().any(|member| member == alias);
            let outbound = channel.outbound_agents.iter().any(|member| member == alias);
            if inbound || outbound {
                context.push_str(&format!("External channel node: {}; channel: {}; conversation: {}; may receive: {inbound}; may send: {outbound}. Explicit send_via target: colony:{}.\n", channel.id, channel.channel, channel.conversation, channel.id));
            }
        }
        context
    }

    #[must_use]
    pub fn colony_instruction_context(
        &self,
        alias: &str,
        admitted_prompts: &[ColonyPrompt],
    ) -> String {
        let mut context = self.colony_connection_context(alias);
        if self.colony_for_agent(alias).is_some() {
            for prompt in admitted_prompts {
                if prompt.agents.iter().any(|member| member == alias) {
                    context.push_str(&format!(
                        "\n[Colony prompt {} revision {}]\n{}\n",
                        prompt.id, prompt.revision, prompt.text
                    ));
                }
            }
        }
        context
    }

    pub fn validate_colonies(&self) -> Result<()> {
        let mut assigned = HashSet::new();
        for (id, colony) in &self.colonies {
            crate::helpers::validate_alias_key(id).map_err(anyhow::Error::msg)?;
            if colony.name.trim().is_empty() || colony.queen.trim().is_empty() {
                bail!("colonies.{id} requires a name and Queen");
            }
            let mut roster = HashSet::new();
            for alias in colony.agent_aliases() {
                if alias == "user" {
                    bail!("colonies.{id} cannot use the reserved human sender alias user");
                }
                if !self.agents.contains_key(alias) {
                    bail!("colonies.{id} references unknown agent {alias:?}");
                }
                if !roster.insert(alias) || !assigned.insert(alias) {
                    bail!("agent {alias:?} must belong to exactly one colony roster");
                }
            }
            let check_members = |aliases: &[String]| -> Result<()> {
                let mut seen = HashSet::new();
                for alias in aliases {
                    if !roster.contains(alias.as_str()) || !seen.insert(alias) {
                        bail!("colonies.{id} has an invalid member reference {alias:?}");
                    }
                }
                Ok(())
            };
            let mut edges = HashSet::new();
            for edge in &colony.connections {
                if edge.from == edge.to
                    || !roster.contains(edge.from.as_str())
                    || !roster.contains(edge.to.as_str())
                    || !edges.insert((&edge.from, &edge.to))
                {
                    bail!("colonies.{id} has an invalid directed connection");
                }
            }
            // Agent aliases and non-agent nodes share the canvas identity
            // space. Reusing an agent alias for a room/prompt is ambiguous.
            let mut nodes: HashSet<&str> = self.agents.keys().map(String::as_str).collect();
            for prompt in &colony.prompts {
                if prompt.id.trim().is_empty() || !nodes.insert(prompt.id.as_str()) {
                    bail!("colonies.{id} prompt IDs must be nonempty and unique");
                }
                check_members(&prompt.agents)?;
            }
            for room in &colony.rooms {
                if room.id.trim().is_empty()
                    || !nodes.insert(room.id.as_str())
                    || room.max_turns == 0
                    || room.max_turns > 100
                {
                    bail!("colonies.{id} has an invalid room ID or turn bound");
                }
                check_members(&room.readers)?;
                check_members(&room.publishers)?;
            }
            let configured_channels: HashSet<String> = self
                .channels_by_alias()
                .into_iter()
                .map(|info| format!("{}.{}", info.channel_type, info.alias))
                .collect();
            for channel in &colony.channels {
                if channel.id.trim().is_empty()
                    || !nodes.insert(channel.id.as_str())
                    || !configured_channels.contains(&channel.channel)
                    || channel.conversation.trim().is_empty()
                {
                    bail!(
                        "colonies.{id} external channels require a unique ID, configured channel and conversation scope"
                    );
                }
                check_members(&channel.inbound_agents)?;
                check_members(&channel.outbound_agents)?;
            }
            for selection in &colony.baseline_context {
                if !roster.contains(selection.agent.as_str()) || selection.key.trim().is_empty() {
                    bail!("colonies.{id} has an invalid baseline context selection");
                }
            }
            if colony
                .positions
                .values()
                .any(|position| !position.x.is_finite() || !position.y.is_finite())
            {
                bail!("colonies.{id} canvas coordinates must be finite");
            }
        }
        Ok(())
    }

    /// Create a new member from an existing member's resource references.
    /// Private identity, workspace, prior memory and channel assignments are
    /// deliberately fresh facts rather than copies of the template's history.
    pub fn add_colony_agent(
        &mut self,
        colony_id: &str,
        template: &str,
        alias: &str,
        core_command: &str,
    ) -> Result<()> {
        crate::helpers::validate_alias_key(alias).map_err(anyhow::Error::msg)?;
        if alias == "user" {
            bail!("a colony agent cannot use the reserved human sender alias user");
        }
        if self.agents.contains_key(alias) || core_command.trim().is_empty() {
            bail!("a new colony agent requires a unique alias and public core command");
        }
        let colony = self
            .colonies
            .get(colony_id)
            .ok_or_else(|| anyhow::Error::msg("colony_not_found"))?;
        if !colony.contains(template) {
            bail!("agent template must belong to this colony");
        }
        let template = self
            .agents
            .get(template)
            .ok_or_else(|| anyhow::Error::msg("agent_template_not_found"))?;
        let new_agent = crate::schema::AliasedAgentConfig {
            core_command: core_command.trim().to_string(),
            model_provider: template.model_provider.clone(),
            risk_profile: template.risk_profile.clone(),
            runtime_profile: template.runtime_profile.clone(),
            skill_bundles: template.skill_bundles.clone(),
            knowledge_bundles: template.knowledge_bundles.clone(),
            mcp_bundles: template.mcp_bundles.clone(),
            summary_provider: template.summary_provider.clone(),
            classifier_provider: template.classifier_provider.clone(),
            delegate_same_risk_profile: false,
            ..crate::schema::AliasedAgentConfig::default()
        };
        self.agents.insert(alias.to_string(), new_agent);
        if let Some(colony) = self.colonies.get_mut(colony_id) {
            colony.members.push(alias.to_string());
        }
        if let Err(error) = self.validate_colonies() {
            self.agents.remove(alias);
            if let Some(colony) = self.colonies.get_mut(colony_id) {
                colony.members.retain(|member| member != alias);
            }
            return Err(error);
        }
        self.mark_dirty("agents");
        self.mark_dirty("colonies");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::AliasedAgentConfig;

    fn config() -> Config {
        let mut config = Config::default();
        for alias in ["queen", "worker", "outside"] {
            config
                .agents
                .insert(alias.into(), AliasedAgentConfig::default());
        }
        config.colonies.insert(
            "jobs".into(),
            ColonyConfig {
                name: "Job search".into(),
                queen: "queen".into(),
                members: vec!["worker".into()],
                connections: vec![ColonyConnection {
                    from: "queen".into(),
                    to: "worker".into(),
                }],
                ..ColonyConfig::default()
            },
        );
        config
    }

    #[test]
    fn directed_grants_and_exclusive_box() {
        let config = config();
        assert!(config.colony_allows_communication("queen", "worker"));
        assert!(!config.colony_allows_communication("worker", "queen"));
        assert!(!config.colony_allows_communication("worker", "outside"));
        assert!(!config.colony_allows_communication("outside", "worker"));
        assert!(config.colony_allows_communication("outside", "another-unassigned"));
    }

    #[test]
    fn membership_and_graph_are_validated() {
        let mut config = config();
        assert!(config.validate_colonies().is_ok());
        config.colonies.insert(
            "other".into(),
            ColonyConfig {
                name: "Other".into(),
                queen: "outside".into(),
                members: vec!["worker".into()],
                ..ColonyConfig::default()
            },
        );
        assert!(config.validate_colonies().is_err());
    }

    #[test]
    fn human_sender_alias_is_reserved_only_for_colony_rosters() {
        let mut config = config();
        config.agents.insert("user".into(), Default::default());
        assert!(config.validate_colonies().is_ok());
        config.colonies.get_mut("jobs").unwrap().queen = "user".into();
        assert!(config.validate_colonies().is_err());
        config.colonies.get_mut("jobs").unwrap().queen = "queen".into();
        config
            .colonies
            .get_mut("jobs")
            .unwrap()
            .members
            .push("user".into());
        assert!(config.validate_colonies().is_err());
        config.colonies.get_mut("jobs").unwrap().members.pop();
        config.agents.remove("user");
        assert!(
            config
                .add_colony_agent("jobs", "queen", "user", "specialist")
                .is_err()
        );
        assert!(!config.agents.contains_key("user"));
        assert!(config.validate_colonies().is_ok());
    }

    #[test]
    fn prompts_are_ordered_and_injected_once() {
        let mut config = config();
        config
            .agents
            .get_mut("worker")
            .expect("fixture member")
            .core_command = "Evaluate job suitability".into();
        config
            .colonies
            .get_mut("jobs")
            .expect("fixture colony")
            .prompts
            .push(ColonyPrompt {
                id: "preferences".into(),
                text: "Remote roles only".into(),
                agents: vec!["worker".into()],
                revision: 1,
            });
        let prompt = config.colony_instruction_context("worker", &config.colonies["jobs"].prompts);
        assert!(prompt.contains("Evaluate job suitability"));
        assert_eq!(prompt.matches("Remote roles only").count(), 1);
        assert!(!prompt.contains("Local roles only"));
    }
}
