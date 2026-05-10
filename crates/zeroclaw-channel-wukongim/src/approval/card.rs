// src/approval/card.rs
use crate::connection::WkMessageType;
use serde::{Deserialize, Serialize};
use zeroclaw_api::channel::ChannelApprovalRequest;

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalCard {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub timeout_secs: u64,
    pub title: String,
    pub body: WkApprovalBody,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<Vec<WkAction>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalBody {
    pub content: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkAction {
    pub text: String,
    pub value: String,
    pub style: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WkApprovalAction {
    #[serde(rename = "type")]
    pub msg_type: u32,
    pub approval_id: String,
    pub action: String,
}

pub fn build_approval_card(
    approval_id: &str,
    request: &ChannelApprovalRequest,
    timeout_secs: u64,
) -> WkApprovalCard {
    let (title, content) = if request.tool_name == "cron_add" {
        let mut summary = request.arguments_summary.clone();
        summary = summary
            .replace("job_type: agent, ", "任务类型: 智能体, ")
            .replace("job_type: shell, ", "任务类型: 脚本, ")
            .replace("name: ", "任务名称: ")
            .replace("prompt: ", "提示词: ")
            .replace("command: ", "执行命令: ")
            .replace("schedule: ", "\n执行计划: ");

        let mut time_info = summary
            .split("\n执行计划: ")
            .last()
            .unwrap_or("按计划执行")
            .to_string();
        if time_info.contains("\"at\":")
            && let Some(start) = time_info.find("\"at\":\"")
        {
            let rest = &time_info[start + 6..];
            if let Some(end) = rest.find('"') {
                time_info = rest[..end].replace('T', " ").replace('Z', " (UTC)");
            }
        }
        (
            "📋 任务执行审批",
            format!(
                "1. **执行的是什么**\n添加定时任务: **{}**\n\n2. **执行的时间相关信息**\n{}\n\n3. **执行内容的总结**\n{}",
                request.tool_name, time_info, summary
            ),
        )
    } else {
        (
            "📋 任务执行审批",
            format!(
                "🔧 智能体请求执行: **{}**\n\n**执行内容总结**:\n{}",
                request.tool_name, request.arguments_summary
            ),
        )
    };

    WkApprovalCard {
        msg_type: WkMessageType::INTERACTIVE_CARD,
        approval_id: approval_id.to_string(),
        timeout_secs,
        title: title.to_string(),
        body: WkApprovalBody {
            content: content.to_string(),
        },
        actions: Some(vec![
            WkAction {
                text: "同意".to_string(),
                value: "approve".to_string(),
                style: "primary".to_string(),
            },
            WkAction {
                text: "拒绝".to_string(),
                value: "deny".to_string(),
                style: "danger".to_string(),
            },
        ]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::channel::ChannelApprovalRequest;

    fn req(tool: &str, summary: &str) -> ChannelApprovalRequest {
        ChannelApprovalRequest {
            tool_name: tool.to_string(),
            arguments_summary: summary.to_string(),
        }
    }

    #[test]
    fn card_has_type_20() {
        let card = build_approval_card("id1", &req("shell_exec", "cmd: ls"), 300);
        let json = serde_json::to_string(&card).unwrap();
        assert!(json.contains("\"type\":20"));
    }

    #[test]
    fn card_has_approve_and_deny_actions() {
        let card = build_approval_card("id2", &req("shell_exec", "cmd: echo"), 60);
        let actions = card.actions.unwrap();
        assert_eq!(actions[0].value, "approve");
        assert_eq!(actions[1].value, "deny");
    }

    #[test]
    fn cron_add_card_localizes_job_type() {
        let card = build_approval_card(
            "id3",
            &req(
                "cron_add",
                "job_type: agent, name: daily, schedule: 0 9 * * *",
            ),
            300,
        );
        assert!(card.body.content.contains("智能体"));
        assert!(card.body.content.contains("daily"));
    }

    #[test]
    fn approval_action_deny_deserializes() {
        let json = r#"{"type":21,"approval_id":"id1","action":"deny"}"#;
        let a: WkApprovalAction = serde_json::from_str(json).unwrap();
        assert_eq!(a.action, "deny");
        assert_eq!(a.msg_type, 21);
    }
}
