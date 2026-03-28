use super::traits::{Tool, ToolResult};
use crate::channels::ack_reaction::{
    select_ack_reaction_with_trace, AckReactionContext, AckReactionContextChatType,
    AckReactionSelectionSource,
};
use crate::config::{
    AckReactionChannelsConfig, AckReactionConfig, AckReactionRuleConfig, AckReactionStrategy,
    Config,
};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AckChannel {
    Telegram,
    Discord,
    Lark,
    Feishu,
}

impl AckChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Telegram => "telegram",
            Self::Discord => "discord",
            Self::Lark => "lark",
            Self::Feishu => "feishu",
        }
    }

    fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "telegram" => Ok(Self::Telegram),
            "discord" => Ok(Self::Discord),
            "lark" => Ok(Self::Lark),
            "feishu" => Ok(Self::Feishu),
            other => {
                anyhow::bail!("Unsupported channel '{other}'. Use telegram|discord|lark|feishu")
            }
        }
    }
}

pub struct ChannelAckConfigTool {
    config: Arc<Config>,
    security: Arc<SecurityPolicy>,
}

impl ChannelAckConfigTool {
    pub fn new(config: Arc<Config>, security: Arc<SecurityPolicy>) -> Self {
        Self { config, security }
    }

    fn load_config_without_env(&self) -> anyhow::Result<Config> {
        let contents = fs::read_to_string(&self.config.config_path).map_err(|error| {
            anyhow::anyhow!(
                "Failed to read config file {}: {error}",
                self.config.config_path.display()
            )
        })?;

        let mut parsed: Config = toml::from_str(&contents).map_err(|error| {
            anyhow::anyhow!(
                "Failed to parse config file {}: {error}",
                self.config.config_path.display()
            )
        })?;
        parsed.config_path = self.config.config_path.clone();
        parsed.workspace_dir = self.config.workspace_dir.clone();
        Ok(parsed)
    }

    fn require_write_access(&self) -> Option<ToolResult> {
        if !self.security.can_act() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: autonomy is read-only".into()),
            });
        }

        if !self.security.record_action() {
            return Some(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        None
    }

    fn parse_channel(args: &Value) -> anyhow::Result<AckChannel> {
        let raw = args
            .get("channel")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: channel"))?;
        AckChannel::parse(raw)
    }

    fn parse_strategy(raw: &str) -> anyhow::Result<AckReactionStrategy> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "random" => Ok(AckReactionStrategy::Random),
            "first" => Ok(AckReactionStrategy::First),
            other => anyhow::bail!("Invalid strategy '{other}'. Use random|first"),
        }
    }

    fn parse_sample_rate(raw: &Value, field: &str) -> anyhow::Result<f64> {
        let value = raw
            .as_f64()
            .ok_or_else(|| anyhow::anyhow!("'{field}' must be a number in range [0.0, 1.0]"))?;
        if !value.is_finite() {
            anyhow::bail!("'{field}' must be finite");
        }
        if !(0.0..=1.0).contains(&value) {
            anyhow::bail!("'{field}' must be within [0.0, 1.0]");
        }
        Ok(value)
    }

    fn parse_chat_type(args: &Value) -> anyhow::Result<AckReactionContextChatType> {
        match args
            .get("chat_type")
            .and_then(Value::as_str)
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("direct") => Ok(AckReactionContextChatType::Direct),
            Some("group") => Ok(AckReactionContextChatType::Group),
            Some(other) => anyhow::bail!("Invalid chat_type '{other}'. Use direct|group"),
        }
    }

    fn parse_runs(args: &Value) -> anyhow::Result<usize> {
        let Some(raw_runs) = args.get("runs") else {
            return Ok(1);
        };
        let runs_u64 = raw_runs
            .as_u64()
            .ok_or_else(|| anyhow::anyhow!("'runs' must be an integer in range [1, 1000]"))?;
        if !(1..=1000).contains(&runs_u64) {
            anyhow::bail!("'runs' must be within [1, 1000]");
        }
        usize::try_from(runs_u64).map_err(|_| anyhow::anyhow!("'runs' is too large"))
    }

    fn fallback_defaults(channel: AckChannel) -> Vec<String> {
        match channel {
            AckChannel::Telegram => vec!["⚡️", "👌", "👀", "🔥", "👍"],
            AckChannel::Discord => vec!["⚡️", "🦀", "🙌", "💪", "👌", "👀", "👣"],
            AckChannel::Lark | AckChannel::Feishu => {
                vec!["✅", "👍", "👌", "👏", "💯", "🎉", "🫡", "✨", "🚀"]
            }
        }
        .into_iter()
        .map(ToOwned::to_owned)
        .collect()
    }

    fn parse_string_list(raw: &Value, field: &str) -> anyhow::Result<Vec<String>> {
        if raw.is_null() {
            return Ok(Vec::new());
        }

        if let Some(raw_string) = raw.as_str() {
            return Ok(raw_string
                .split(',')
                .map(str::trim)
                .filter(|entry| !entry.is_empty())
                .map(ToOwned::to_owned)
                .collect());
        }

        if let Some(array) = raw.as_array() {
            let mut out = Vec::new();
            for item in array {
                let value = item
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'{field}' array must only contain strings"))?;
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    out.push(trimmed.to_string());
                }
            }
            return Ok(out);
        }

        anyhow::bail!("'{field}' must be a string, string[], or null")
    }

    fn parse_rule(raw: &Value) -> anyhow::Result<AckReactionRuleConfig> {
        if !raw.is_object() {
            anyhow::bail!("'rule' must be an object");
        }
        serde_json::from_value(raw.clone())
            .map_err(|error| anyhow::anyhow!("Invalid rule: {error}"))
    }

    fn parse_rules(raw: &Value) -> anyhow::Result<Vec<AckReactionRuleConfig>> {
        if raw.is_null() {
            return Ok(Vec::new());
        }
        let rules = raw
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("'rules' must be an array"))?;
        let mut parsed = Vec::with_capacity(rules.len());
        for rule in rules {
            parsed.push(Self::parse_rule(rule)?);
        }
        Ok(parsed)
    }

    fn channel_config_ref<'a>(
        channels: &'a AckReactionChannelsConfig,
        channel: AckChannel,
    ) -> Option<&'a AckReactionConfig> {
        match channel {
            AckChannel::Telegram => channels.telegram.as_ref(),
            AckChannel::Discord => channels.discord.as_ref(),
            AckChannel::Lark => channels.lark.as_ref(),
            AckChannel::Feishu => channels.feishu.as_ref(),
        }
    }

    fn channel_config_mut<'a>(
        channels: &'a mut AckReactionChannelsConfig,
        channel: AckChannel,
    ) -> &'a mut Option<AckReactionConfig> {
        match channel {
            AckChannel::Telegram => &mut channels.telegram,
            AckChannel::Discord => &mut channels.discord,
            AckChannel::Lark => &mut channels.lark,
            AckChannel::Feishu => &mut channels.feishu,
        }
    }

    fn snapshot_one(config: Option<&AckReactionConfig>) -> Value {
        config.map_or(Value::Null, |cfg| {
            json!({
                "enabled": cfg.enabled,
                "strategy": match cfg.strategy {
                    AckReactionStrategy::Random => "random",
                    AckReactionStrategy::First => "first",
                },
                "sample_rate": cfg.sample_rate,
                "emojis": cfg.emojis,
                "rules": cfg.rules,
            })
        })
    }

    fn snapshot_all(channels: &AckReactionChannelsConfig) -> Value {
        json!({
            "telegram": Self::snapshot_one(channels.telegram.as_ref()),
            "discord": Self::snapshot_one(channels.discord.as_ref()),
            "lark": Self::snapshot_one(channels.lark.as_ref()),
            "feishu": Self::snapshot_one(channels.feishu.as_ref()),
        })
    }

    fn handle_get(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let cfg = self.load_config_without_env()?;
        let output = if let Some(raw_channel) = args.get("channel").and_then(Value::as_str) {
            let channel = AckChannel::parse(raw_channel)?;
            json!({
                "channel": channel.as_str(),
                "ack_reaction": Self::snapshot_one(Self::channel_config_ref(
                    &cfg.channels_config.ack_reaction,
                    channel
                )),
            })
        } else {
            json!({
                "ack_reaction": Self::snapshot_all(&cfg.channels_config.ack_reaction),
            })
        };

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&output)?,
            error: None,
        })
    }

    async fn handle_set(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let channel = Self::parse_channel(args)?;
        let mut cfg = self.load_config_without_env()?;
        let slot = Self::channel_config_mut(&mut cfg.channels_config.ack_reaction, channel);
        let mut channel_cfg = slot.clone().unwrap_or_default();

        if let Some(raw_enabled) = args.get("enabled") {
            channel_cfg.enabled = raw_enabled
                .as_bool()
                .ok_or_else(|| anyhow::anyhow!("'enabled' must be a boolean"))?;
        }

        if let Some(raw_strategy) = args.get("strategy") {
            if raw_strategy.is_null() {
                channel_cfg.strategy = AckReactionStrategy::Random;
            } else {
                let value = raw_strategy
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("'strategy' must be a string or null"))?;
                channel_cfg.strategy = Self::parse_strategy(value)?;
            }
        }

        if let Some(raw_sample_rate) = args.get("sample_rate") {
            if raw_sample_rate.is_null() {
                channel_cfg.sample_rate = 1.0;
            } else {
                channel_cfg.sample_rate = Self::parse_sample_rate(raw_sample_rate, "sample_rate")?;
            }
        }

        if let Some(raw_emojis) = args.get("emojis") {
            channel_cfg.emojis = Self::parse_string_list(raw_emojis, "emojis")?;
        }

        if let Some(raw_rules) = args.get("rules") {
            channel_cfg.rules = Self::parse_rules(raw_rules)?;
        }

        *slot = Some(channel_cfg);
        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": format!("Updated channels_config.ack_reaction.{}", channel.as_str()),
                "channel": channel.as_str(),
                "ack_reaction": Self::snapshot_one(Self::channel_config_ref(
                    &cfg.channels_config.ack_reaction,
                    channel
                )),
            }))?,
            error: None,
        })
    }

    async fn handle_add_rule(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let channel = Self::parse_channel(args)?;
        let raw_rule = args
            .get("rule")
            .ok_or_else(|| anyhow::anyhow!("Missing required field: rule"))?;
        let rule = Self::parse_rule(raw_rule)?;

        let mut cfg = self.load_config_without_env()?;
        let slot = Self::channel_config_mut(&mut cfg.channels_config.ack_reaction, channel);
        let mut channel_cfg = slot.clone().unwrap_or_default();
        channel_cfg.rules.push(rule);
        *slot = Some(channel_cfg);
        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": format!("Added rule to channels_config.ack_reaction.{}", channel.as_str()),
                "channel": channel.as_str(),
                "ack_reaction": Self::snapshot_one(Self::channel_config_ref(
                    &cfg.channels_config.ack_reaction,
                    channel
                )),
            }))?,
            error: None,
        })
    }

    async fn handle_remove_rule(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let channel = Self::parse_channel(args)?;
        let index = args
            .get("index")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: index"))?;
        let index = usize::try_from(index).map_err(|_| anyhow::anyhow!("'index' is too large"))?;

        let mut cfg = self.load_config_without_env()?;
        let slot = Self::channel_config_mut(&mut cfg.channels_config.ack_reaction, channel);
        let mut channel_cfg = slot.clone().ok_or_else(|| {
            anyhow::anyhow!("No channel policy is configured for {}", channel.as_str())
        })?;
        if index >= channel_cfg.rules.len() {
            anyhow::bail!(
                "Rule index out of range. {} has {} rule(s)",
                channel.as_str(),
                channel_cfg.rules.len()
            );
        }
        channel_cfg.rules.remove(index);
        *slot = Some(channel_cfg);
        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": format!("Removed rule #{index} from channels_config.ack_reaction.{}", channel.as_str()),
                "channel": channel.as_str(),
                "ack_reaction": Self::snapshot_one(Self::channel_config_ref(
                    &cfg.channels_config.ack_reaction,
                    channel
                )),
            }))?,
            error: None,
        })
    }

    async fn handle_clear_rules(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let channel = Self::parse_channel(args)?;
        let mut cfg = self.load_config_without_env()?;
        let slot = Self::channel_config_mut(&mut cfg.channels_config.ack_reaction, channel);
        let mut channel_cfg = slot.clone().unwrap_or_default();
        channel_cfg.rules.clear();
        *slot = Some(channel_cfg);
        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": format!("Cleared rules in channels_config.ack_reaction.{}", channel.as_str()),
                "channel": channel.as_str(),
                "ack_reaction": Self::snapshot_one(Self::channel_config_ref(
                    &cfg.channels_config.ack_reaction,
                    channel
                )),
            }))?,
            error: None,
        })
    }

    async fn handle_unset(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let channel = Self::parse_channel(args)?;
        let mut cfg = self.load_config_without_env()?;
        let slot = Self::channel_config_mut(&mut cfg.channels_config.ack_reaction, channel);
        *slot = None;
        cfg.save().await?;

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "message": format!("Removed channels_config.ack_reaction.{}", channel.as_str()),
                "channel": channel.as_str(),
                "ack_reaction": Value::Null,
            }))?,
            error: None,
        })
    }

    fn handle_simulate(&self, args: &Value) -> anyhow::Result<ToolResult> {
        let channel = Self::parse_channel(args)?;
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: text"))?;
        let chat_type = Self::parse_chat_type(args)?;
        let sender_id = args.get("sender_id").and_then(Value::as_str);
        let chat_id = args.get("chat_id").and_then(Value::as_str);
        let locale_hint = args.get("locale_hint").and_then(Value::as_str);
        let runs = Self::parse_runs(args)?;

        let defaults = if let Some(raw_defaults) = args.get("defaults") {
            Self::parse_string_list(raw_defaults, "defaults")?
        } else {
            Self::fallback_defaults(channel)
        };
        let default_refs = defaults.iter().map(String::as_str).collect::<Vec<_>>();

        let cfg = self.load_config_without_env()?;
        let policy = Self::channel_config_ref(&cfg.channels_config.ack_reaction, channel);
        let mut first_selection = None;
        let mut emoji_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut no_emoji_count = 0usize;
        let mut suppressed_count = 0usize;
        let mut matched_rule_index_counts: BTreeMap<String, usize> = BTreeMap::new();
        let mut source_counts: BTreeMap<String, usize> = BTreeMap::new();

        for _ in 0..runs {
            let selection = select_ack_reaction_with_trace(
                policy,
                &default_refs,
                &AckReactionContext {
                    text,
                    sender_id,
                    chat_id,
                    chat_type,
                    locale_hint,
                },
            );

            if first_selection.is_none() {
                first_selection = Some(selection.clone());
            }

            if let Some(emoji) = selection.emoji.clone() {
                *emoji_counts.entry(emoji).or_insert(0) += 1;
            } else {
                no_emoji_count += 1;
            }

            if selection.suppressed {
                suppressed_count += 1;
            }

            if let Some(index) = selection.matched_rule_index {
                *matched_rule_index_counts
                    .entry(index.to_string())
                    .or_insert(0) += 1;
            }

            let source_key = match selection.source {
                Some(AckReactionSelectionSource::Rule(_)) => "rule",
                Some(AckReactionSelectionSource::ChannelPool) => "channel_pool",
                Some(AckReactionSelectionSource::DefaultPool) => "default_pool",
                None => "none",
            };
            *source_counts.entry(source_key.to_string()).or_insert(0) += 1;
        }

        let selection = first_selection.unwrap_or_else(|| {
            select_ack_reaction_with_trace(
                policy,
                &default_refs,
                &AckReactionContext {
                    text,
                    sender_id,
                    chat_id,
                    chat_type,
                    locale_hint,
                },
            )
        });

        let source = selection.source.as_ref().map(|source| match source {
            AckReactionSelectionSource::Rule(index) => json!({
                "kind": "rule",
                "index": index
            }),
            AckReactionSelectionSource::ChannelPool => json!({
                "kind": "channel_pool"
            }),
            AckReactionSelectionSource::DefaultPool => json!({
                "kind": "default_pool"
            }),
        });

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "channel": channel.as_str(),
                "input": {
                    "text": text,
                    "sender_id": sender_id,
                    "chat_id": chat_id,
                    "chat_type": match chat_type {
                        AckReactionContextChatType::Direct => "direct",
                        AckReactionContextChatType::Group => "group",
                    },
                    "locale_hint": locale_hint,
                    "defaults": defaults,
                    "runs": runs,
                },
                "selection": {
                    "emoji": selection.emoji,
                    "matched_rule_index": selection.matched_rule_index,
                    "suppressed": selection.suppressed,
                    "source": source,
                },
                "aggregate": {
                    "runs": runs,
                    "emoji_counts": emoji_counts,
                    "no_emoji_count": no_emoji_count,
                    "suppressed_count": suppressed_count,
                    "matched_rule_index_counts": matched_rule_index_counts,
                    "source_counts": source_counts,
                },
            }))?,
            error: None,
        })
    }
}

#[async_trait]
impl Tool for ChannelAckConfigTool {
    fn name(&self) -> &str {
        "channel_ack_config"
    }

    fn description(&self) -> &str {
        "Inspect and update configurable ACK emoji reaction policies for Telegram/Discord/Lark/Feishu under [channels_config.ack_reaction]. Supports enabling/disabling reactions, setting emoji pools, and rule-based conditions."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["get", "set", "add_rule", "remove_rule", "clear_rules", "unset", "simulate"],
                    "description": "Operation to perform"
                },
                "channel": {
                    "type": "string",
                    "enum": ["telegram", "discord", "lark", "feishu"]
                },
                "enabled": {"type": "boolean"},
                "strategy": {"type": ["string", "null"], "enum": ["random", "first", null]},
                "sample_rate": {"type": ["number", "null"], "minimum": 0.0, "maximum": 1.0},
                "emojis": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}},
                        {"type": "null"}
                    ]
                },
                "rules": {"type": ["array", "null"]},
                "rule": {"type": "object"},
                "index": {"type": "integer", "minimum": 0},
                "text": {"type": "string"},
                "sender_id": {"type": ["string", "null"]},
                "chat_id": {"type": ["string", "null"]},
                "chat_type": {"type": "string", "enum": ["direct", "group"]},
                "locale_hint": {"type": ["string", "null"]},
                "runs": {"type": "integer", "minimum": 1, "maximum": 1000},
                "defaults": {
                    "anyOf": [
                        {"type": "string"},
                        {"type": "array", "items": {"type": "string"}},
                        {"type": "null"}
                    ]
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("Missing required field: action"))?;

        match action {
            "get" => self.handle_get(&args),
            "set" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_set(&args).await
            }
            "add_rule" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_add_rule(&args).await
            }
            "remove_rule" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_remove_rule(&args).await
            }
            "clear_rules" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_clear_rules(&args).await
            }
            "unset" => {
                if let Some(blocked) = self.require_write_access() {
                    return Ok(blocked);
                }
                self.handle_unset(&args).await
            }
            "simulate" => self.handle_simulate(&args),
            other => anyhow::bail!(
                "Unsupported action '{other}'. Use get|set|add_rule|remove_rule|clear_rules|unset|simulate"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};
    use tempfile::TempDir;

    fn test_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn readonly_security() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    async fn test_config(tmp: &TempDir) -> Arc<Config> {
        let config = Config {
            workspace_dir: tmp.path().join("workspace"),
            config_path: tmp.path().join("config.toml"),
            ..Config::default()
        };
        config.save().await.unwrap();
        Arc::new(config)
    }

    #[tokio::test]
    async fn set_and_get_channel_policy() {
        let tmp = TempDir::new().unwrap();
        let tool = ChannelAckConfigTool::new(test_config(&tmp).await, test_security());

        let set_result = tool
            .execute(json!({
                "action": "set",
                "channel": "telegram",
                "enabled": true,
                "strategy": "first",
                "sample_rate": 0.75,
                "emojis": ["✅", "👍"]
            }))
            .await
            .unwrap();
        assert!(set_result.success, "{:?}", set_result.error);

        let get_result = tool
            .execute(json!({
                "action": "get",
                "channel": "telegram"
            }))
            .await
            .unwrap();
        assert!(get_result.success, "{:?}", get_result.error);
        let output: Value = serde_json::from_str(&get_result.output).unwrap();
        assert_eq!(output["ack_reaction"]["strategy"], json!("first"));
        assert_eq!(output["ack_reaction"]["sample_rate"], json!(0.75));
        assert_eq!(output["ack_reaction"]["emojis"], json!(["✅", "👍"]));
    }

    #[tokio::test]
    async fn add_and_remove_rule_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let tool = ChannelAckConfigTool::new(test_config(&tmp).await, test_security());

        let add_result = tool
            .execute(json!({
                "action": "add_rule",
                "channel": "discord",
                "rule": {
                    "enabled": true,
                    "contains_any": ["deploy"],
                    "chat_types": ["group"],
                    "emojis": ["🚀"],
                    "strategy": "first"
                }
            }))
            .await
            .unwrap();
        assert!(add_result.success, "{:?}", add_result.error);

        let remove_result = tool
            .execute(json!({
                "action": "remove_rule",
                "channel": "discord",
                "index": 0
            }))
            .await
            .unwrap();
        assert!(remove_result.success, "{:?}", remove_result.error);

        let output: Value = serde_json::from_str(&remove_result.output).unwrap();
        assert_eq!(output["ack_reaction"]["rules"], json!([]));
    }

    #[tokio::test]
    async fn readonly_mode_blocks_mutation() {
        let tmp = TempDir::new().unwrap();
        let tool = ChannelAckConfigTool::new(test_config(&tmp).await, readonly_security());

        let result = tool
            .execute(json!({
                "action": "set",
                "channel": "telegram",
                "enabled": false
            }))
            .await
            .unwrap();

        assert!(!result.success);
        assert!(result
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("read-only"));
    }

    #[tokio::test]
    async fn simulate_reports_rule_selection() {
        let tmp = TempDir::new().unwrap();
        let tool = ChannelAckConfigTool::new(test_config(&tmp).await, test_security());

        let set_result = tool
            .execute(json!({
                "action": "set",
                "channel": "telegram",
                "enabled": true,
                "strategy": "first",
                "emojis": ["✅"],
                "rules": [{
                    "enabled": true,
                    "contains_any": ["deploy"],
                    "action": "react",
                    "strategy": "first",
                    "emojis": ["🚀"]
                }]
            }))
            .await
            .unwrap();
        assert!(set_result.success, "{:?}", set_result.error);

        let result = tool
            .execute(json!({
                "action": "simulate",
                "channel": "telegram",
                "text": "deploy finished",
                "chat_type": "group",
                "sender_id": "u1",
                "locale_hint": "en"
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let output: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["selection"]["emoji"], json!("🚀"));
        assert_eq!(output["selection"]["matched_rule_index"], json!(0));
        assert_eq!(output["selection"]["suppressed"], json!(false));
        assert_eq!(output["selection"]["source"]["kind"], json!("rule"));
    }

    #[tokio::test]
    async fn simulate_runs_reports_aggregate_counts() {
        let tmp = TempDir::new().unwrap();
        let tool = ChannelAckConfigTool::new(test_config(&tmp).await, test_security());

        let set_result = tool
            .execute(json!({
                "action": "set",
                "channel": "discord",
                "enabled": true,
                "strategy": "first",
                "sample_rate": 1.0,
                "emojis": ["✅"]
            }))
            .await
            .unwrap();
        assert!(set_result.success, "{:?}", set_result.error);

        let result = tool
            .execute(json!({
                "action": "simulate",
                "channel": "discord",
                "text": "hello world",
                "chat_type": "group",
                "chat_id": "c-1",
                "runs": 5
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);

        let output: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["input"]["runs"], json!(5));
        assert_eq!(output["aggregate"]["runs"], json!(5));
        assert_eq!(output["aggregate"]["emoji_counts"]["✅"], json!(5));
        assert_eq!(output["aggregate"]["no_emoji_count"], json!(0));
        assert_eq!(output["aggregate"]["suppressed_count"], json!(0));
        assert_eq!(
            output["aggregate"]["source_counts"]["channel_pool"],
            json!(5)
        );
    }
}
