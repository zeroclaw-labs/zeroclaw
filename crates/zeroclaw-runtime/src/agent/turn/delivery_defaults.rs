//! Channel delivery-default injection for cron_add and interactive tools.

const AUTO_DELIVERY_DEFAULT_CHANNELS: &[&str] = &[
    "telegram",
    "discord",
    "slack",
    "mattermost",
    "matrix",
    "dingtalk",
    "lark",
    "feishu",
];

/// Tools that take an optional `channel` arg and otherwise pick
/// `channels.iter().next()` (HashMap order). Without an originating-channel
/// default they can route an interactive prompt to a random external channel
/// while the user is sitting in the RPC/ACP/WS session that issued the turn —
/// which looks like a hang until timeout/cancel.
const ORIGINATING_CHANNEL_TOOLS: &[&str] = &["ask_user", "poll", "escalate_to_human"];

/// When `tool_args` omits `channel`, fill it from the turn's originating
/// channel name so interactive tools prefer the conversation back-channel
/// (`rpc` / `acp` / `ws` / …) over an arbitrary map entry.
pub(crate) fn maybe_inject_originating_channel(
    tool_name: &str,
    tool_args: &mut serde_json::Value,
    channel_name: &str,
) {
    if !ORIGINATING_CHANNEL_TOOLS.contains(&tool_name) {
        return;
    }
    let channel_name = channel_name.trim();
    if channel_name.is_empty() || channel_name == "agent" || channel_name == "daemon" {
        // Configless / non-channel turns have no conversation back-channel to
        // prefer; leave selection to the tool's existing map fallback.
        return;
    }
    let Some(args) = tool_args.as_object_mut() else {
        return;
    };
    let needs_channel = args
        .get("channel")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| value.trim().is_empty());
    if needs_channel {
        args.insert(
            "channel".to_string(),
            serde_json::Value::String(channel_name.to_string()),
        );
    }
}

pub(crate) fn maybe_inject_channel_delivery_defaults(
    tool_name: &str,
    tool_args: &mut serde_json::Value,
    channel_name: &str,
    channel_reply_target: Option<&str>,
) {
    // Interactive tools first — independent of cron delivery defaults.
    maybe_inject_originating_channel(tool_name, tool_args, channel_name);

    if tool_name != "cron_add" {
        return;
    }

    if !AUTO_DELIVERY_DEFAULT_CHANNELS.contains(&channel_name) {
        return;
    }

    let Some(reply_target) = channel_reply_target
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let Some(args) = tool_args.as_object_mut() else {
        return;
    };

    let is_agent_job = args
        .get("job_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|job_type| job_type.eq_ignore_ascii_case("agent"))
        || args
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|prompt| !prompt.trim().is_empty());
    if !is_agent_job {
        return;
    }

    let default_delivery = || {
        serde_json::json!({
            "mode": "announce",
            "channel": channel_name,
            "to": reply_target,
        })
    };

    match args.get_mut("delivery") {
        None => {
            args.insert("delivery".to_string(), default_delivery());
        }
        Some(delivery) if delivery.is_null() => {
            *delivery = default_delivery();
        }
        Some(serde_json::Value::Object(delivery)) => {
            if delivery
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|mode| mode.eq_ignore_ascii_case("none"))
            {
                return;
            }

            delivery
                .entry("mode".to_string())
                .or_insert_with(|| serde_json::Value::String("announce".to_string()));

            let needs_channel = delivery
                .get("channel")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if needs_channel {
                delivery.insert(
                    "channel".to_string(),
                    serde_json::Value::String(channel_name.to_string()),
                );
            }

            let needs_target = delivery
                .get("to")
                .and_then(serde_json::Value::as_str)
                .is_none_or(|value| value.trim().is_empty());
            if needs_target {
                delivery.insert(
                    "to".to_string(),
                    serde_json::Value::String(reply_target.to_string()),
                );
            }
        }
        Some(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{maybe_inject_channel_delivery_defaults, maybe_inject_originating_channel};

    #[test]
    fn null_delivery_is_replaced_with_channel_defaults() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "delivery": null,
        });

        maybe_inject_channel_delivery_defaults(
            "cron_add",
            &mut args,
            "discord",
            Some("channel-42"),
        );

        assert_eq!(
            args["delivery"],
            serde_json::json!({
                "mode": "announce",
                "channel": "discord",
                "to": "channel-42",
            })
        );
    }

    #[test]
    fn ask_user_gets_originating_rpc_channel_when_omitted() {
        // Regression: without this, ask_user picks channels.iter().next()
        // (HashMap order) and can hang on an external channel while the user
        // is waiting in the RPC/Code session that issued the turn.
        let mut args = serde_json::json!({
            "question": "Pick one",
            "choices": ["A", "B"],
        });
        maybe_inject_originating_channel("ask_user", &mut args, "rpc");
        assert_eq!(args["channel"], "rpc");
    }

    #[test]
    fn ask_user_does_not_override_explicit_channel() {
        let mut args = serde_json::json!({
            "question": "Pick one",
            "channel": "telegram.default",
        });
        maybe_inject_originating_channel("ask_user", &mut args, "rpc");
        assert_eq!(args["channel"], "telegram.default");
    }

    #[test]
    fn ask_user_skips_generic_agent_channel_name() {
        let mut args = serde_json::json!({ "question": "hi" });
        maybe_inject_originating_channel("ask_user", &mut args, "agent");
        assert!(args.get("channel").is_none());
    }

    #[test]
    fn poll_and_escalate_also_get_originating_channel() {
        for tool in ["poll", "escalate_to_human"] {
            let mut args = serde_json::json!({ "question": "q" });
            maybe_inject_originating_channel(tool, &mut args, "acp");
            assert_eq!(args["channel"], "acp", "tool={tool}");
        }
    }

    #[test]
    fn injected_channel_matches_escalate_tool_schema_parameter() {
        // Guard against the injection being a no-op: `escalate_to_human` must
        // actually declare the `channel` parameter we inject, otherwise the
        // helper "passes" while the tool ignores the key and falls back to
        // channels.iter().next(). Behavioural coverage of the routing itself
        // lives in zeroclaw-tools::escalate tests.
        let tool = zeroclaw_tools::escalate::EscalateToHumanTool::new(
            std::sync::Arc::new(zeroclaw_config::policy::SecurityPolicy::default()),
            vec![],
            std::sync::Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
        );
        let schema = zeroclaw_api::tool::Tool::parameters_schema(&tool);
        assert!(
            schema["properties"]["channel"].is_object(),
            "escalate_to_human must declare the injected `channel` parameter; schema: {schema}"
        );
    }
}
